#![no_std]
#![no_main]

//! Rust-only userland init and service supervisor.
//!
//! PID 1 is intentionally a normal native ELF process.  The kernel gives it
//! the init role at bootstrap, but service policy lives here: the service
//! table, restart policy, exponential backoff, and process-control polling.
//! Adding another service therefore does not require a shell-specific kernel
//! path; it only adds another `ServiceSpec` to this table.

use core::arch::asm;

// Keep these freestanding payload constants synchronized with the ABI source
// of truth in fullerene-kernel/abi/src/lib.rs.
const EXIT: u64 = 1;
const WRITE: u64 = 4;
const WAIT: u64 = 7;
const YIELD: u64 = 22;
const SPAWN: u64 = 23;
const HANDLE_REVOKE: u64 = 92;
const CREATE_TERMINAL: u64 = 65;
const OPEN_PROCESS_CONTROL: u64 = 110;
const PROCESS_CONTROL_STATUS: u64 = 112;
const PROCESS_CONTROL_REAP: u64 = 113;
const LAUNCHD_POLL_REQUEST: u64 = 115;

const PROCESS_READY: u64 = 0;
const PROCESS_RUNNING: u64 = 1;
const PROCESS_BLOCKED: u64 = 2;
const PROCESS_TERMINATED: u64 = 3;
const STABLE_RUN_TICKS: u32 = 256;
const MAX_BACKOFF_TICKS: u32 = 256;

static SHELL_IMAGE: &[u8] = include_bytes!(env!("FULLERENE_SHELL_IMAGE"));

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

impl RestartPolicy {
    fn should_restart(self, exit_code: i32) -> bool {
        match self {
            Self::Never => false,
            Self::OnFailure => exit_code != 0,
            Self::Always => true,
        }
    }
}

#[derive(Clone, Copy)]
struct ServiceSpec {
    name: &'static [u8],
    terminal_title: &'static [u8],
    image: &'static [u8],
    restart: RestartPolicy,
    required: bool,
}

#[derive(Clone, Copy)]
struct ServiceSlot {
    pid: u64,
    control: u64,
    restart_count: u32,
    cooldown: u32,
    healthy_ticks: u32,
    stopped: bool,
}

// The shell is on-demand, not a boot service. The desktop preserves the
// former Nozzle launch gesture and launchd turns its request into this job.
static SHELL_SERVICE: ServiceSpec = ServiceSpec {
    name: b"shell",
    terminal_title: b"Terminal",
    image: SHELL_IMAGE,
    restart: RestartPolicy::Never,
    required: false,
};

// Other launchd-managed services can be added here without changing the
// shell-on-demand path. An empty table means desktop boot has no user-facing
// terminal side effect.
static SERVICES: &[ServiceSpec] = &[];

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

unsafe fn syscall(number: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") number,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

fn write(message: &[u8]) {
    unsafe {
        let _ = syscall(
            WRITE,
            1,
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
            0,
        );
    }
}

fn write_service_event(service: &ServiceSpec, event: &[u8]) {
    write(b"launchd: ");
    write(service.name);
    write(event);
    write(b"\n");
}

fn fatal_start_failure(service: &ServiceSpec, reason: &[u8]) -> ! {
    write_service_event(service, reason);
    write(b"launchd: fatal service bootstrap failure\n");
    unsafe {
        let _ = syscall(EXIT, 1, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn failed_start(service: &ServiceSpec, reason: &[u8], restart_count: u32) -> ServiceSlot {
    if service.required {
        fatal_start_failure(service, reason);
    }

    // Optional jobs are allowed to remain stopped. They are retried through
    // the same bounded backoff as an exited job, but a broken optional job
    // must not take PID 1 down with it.
    write_service_event(service, reason);
    write_service_event(service, b" optional service deferred");
    if service.restart.should_restart(-1) {
        ServiceSlot {
            pid: 0,
            control: 0,
            restart_count: restart_count.saturating_add(1),
            cooldown: backoff_ticks(restart_count),
            healthy_ticks: 0,
            stopped: false,
        }
    } else {
        write_service_event(service, b" is not configured for retry");
        ServiceSlot {
            pid: 0,
            control: 0,
            restart_count,
            cooldown: 0,
            healthy_ticks: 0,
            stopped: true,
        }
    }
}

fn launch_service(service: &ServiceSpec, restart_count: u32) -> ServiceSlot {
    let terminal = unsafe {
        syscall(
            CREATE_TERMINAL,
            service.terminal_title.as_ptr() as u64,
            service.terminal_title.len() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if (terminal as i64) < 0 {
        return failed_start(service, b" terminal creation failed", restart_count);
    }

    let pid = unsafe {
        syscall(
            SPAWN,
            service.image.as_ptr() as u64,
            service.image.len() as u64,
            service.name.as_ptr() as u64,
            service.name.len() as u64,
            terminal,
            0,
        )
    };
    if (pid as i64) < 0 {
        // There is no child to own the terminal when spawn fails. The kernel
        // rolls back the provisional endpoint ownership, while required
        // bootstrap jobs halt PID 1 and optional jobs are retried.
        return failed_start(service, b" spawn failed", restart_count);
    }

    let control = unsafe { syscall(OPEN_PROCESS_CONTROL, pid, 0, 0, 0, 0, 0) };
    if (control as i64) < 0 {
        // The child is already born and owns the terminal.  Wait for it
        // before stopping PID 1; this preserves the parent-side zombie rule.
        write_service_event(service, b" process-control open failed; waiting");
        let _ = unsafe { syscall(WAIT, pid, 0, 0, 0, 0, 0) };
        return failed_start(
            service,
            b" process administration unavailable",
            restart_count,
        );
    }

    write_service_event(service, b" started");
    ServiceSlot {
        pid,
        control,
        restart_count,
        cooldown: 0,
        healthy_ticks: 0,
        stopped: false,
    }
}

fn backoff_ticks(restart_count: u32) -> u32 {
    let shift = restart_count.min(8);
    (1u32 << shift).min(MAX_BACKOFF_TICKS)
}

fn poll_service(service: &ServiceSpec, mut slot: ServiceSlot) -> Option<ServiceSlot> {
    let status = unsafe { syscall(PROCESS_CONTROL_STATUS, slot.control, 0, 0, 0, 0, 0) };
    if (status as i64) < 0 {
        // A lost capability must not turn into an unreaped child.  The birth
        // parent relationship is the safe fallback for collection.
        let _ = unsafe { syscall(WAIT, slot.pid, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(HANDLE_REVOKE, slot.control, 0, 0, 0, 0, 0) };
        write_service_event(service, b" administration lost; restarting");
        slot.pid = 0;
        slot.control = 0;
        if service.restart.should_restart(-1) {
            slot.cooldown = backoff_ticks(slot.restart_count);
            slot.restart_count = slot.restart_count.saturating_add(1);
        } else {
            slot.stopped = true;
        }
        slot.healthy_ticks = 0;
        return Some(slot);
    }

    if status == PROCESS_READY || status == PROCESS_RUNNING || status == PROCESS_BLOCKED {
        slot.healthy_ticks = slot.healthy_ticks.saturating_add(1);
        return Some(slot);
    }

    if status != PROCESS_TERMINATED {
        return Some(slot);
    }

    let exit_status = unsafe { syscall(PROCESS_CONTROL_REAP, slot.control, 0, 0, 0, 0, 0) };
    let exit_code = if (exit_status as i64) < 0 {
        // Keep the process from becoming a zombie even if the capability
        // operation races with cleanup.
        let _ = unsafe { syscall(WAIT, slot.pid, 0, 0, 0, 0, 0) };
        -1
    } else {
        exit_status as i32
    };
    let _ = unsafe { syscall(HANDLE_REVOKE, slot.control, 0, 0, 0, 0, 0) };

    write_service_event(service, b" exited");
    if !service.restart.should_restart(exit_code) {
        write_service_event(service, b" is not configured for restart");
        return Some(ServiceSlot {
            pid: 0,
            control: 0,
            restart_count: slot.restart_count,
            cooldown: 0,
            healthy_ticks: 0,
            stopped: true,
        });
    }

    let restart_count = if slot.healthy_ticks >= STABLE_RUN_TICKS {
        0
    } else {
        slot.restart_count.saturating_add(1)
    };
    let delay = backoff_ticks(restart_count);
    write_service_event(service, b" scheduled for restart");
    Some(ServiceSlot {
        pid: 0,
        control: 0,
        restart_count,
        cooldown: delay,
        healthy_ticks: 0,
        stopped: false,
    })
}

/// Advance a service slot through the common stopped, cooldown, launch, and
/// polling state machine. Keeping shell and table-managed services on this
/// path ensures restart backoff expires consistently.
fn advance_service(service: &ServiceSpec, mut slot: ServiceSlot) -> ServiceSlot {
    if slot.stopped {
        return slot;
    }
    if slot.pid == 0 {
        if slot.cooldown > 0 {
            slot.cooldown -= 1;
            slot
        } else {
            launch_service(service, slot.restart_count)
        }
    } else {
        poll_service(service, slot).unwrap_or(slot)
    }
}

fn service_loop() -> ! {
    write(b"launchd: PID 1 started\n");
    let mut slots = [None; SERVICES.len()];
    let mut shell_slot = None;

    loop {
        let shell_request = unsafe { syscall(LAUNCHD_POLL_REQUEST, 0, 0, 0, 0, 0, 0) };
        let shell_is_stopped = shell_slot.map_or(true, |slot: ServiceSlot| slot.stopped);
        if shell_request == 1 && shell_is_stopped {
            shell_slot = Some(launch_service(&SHELL_SERVICE, 0));
        }
        shell_slot = shell_slot.map(|slot| advance_service(&SHELL_SERVICE, slot));

        let mut index = 0;
        while index < SERVICES.len() {
            let service = &SERVICES[index];
            slots[index] = Some(match slots[index] {
                None => launch_service(service, 0),
                Some(slot) => advance_service(service, slot),
            });
            index += 1;
        }

        unsafe {
            let _ = syscall(YIELD, 0, 0, 0, 0, 0, 0);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    service_loop()
}
