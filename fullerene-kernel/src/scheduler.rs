//! System scheduler — idle loop driven by [`SchedulerContext`].
//!
//! All scheduling state lives in the [`SCHEDULER`] singleton.
//! This module is the thin entry point that boots the desktop, registers
//! the NMI recovery target, and enters the idle tick loop.
//!
//! # Tick loop
//!
//! ```text
//! scheduler_loop()
//!   ├── update_vdso_all()       — publish time to every process's VDSO page
//!   ├── gui::runtime_tick()     — input polling, tick_core + framebuffer render
//!   ├── launchd bootstrap       — user ELF PID 1
//!   ├── advance_tick()
//!   └── hlt()
//! ```

use core::sync::atomic::Ordering;
use x86_64::VirtAddr;

use crate::gui;
use crate::scheduler_context::SCHEDULER;

static LAUNCHD_IMAGE: &[u8] = include_bytes!(env!("FULLERENE_LAUNCHD_IMAGE"));
/// Set by the desktop callback and consumed by PID 1 through the native ABI.
/// Keeping the request at the kernel boundary preserves the old Nozzle launch
/// gesture without making the kernel start the shell itself.
static LAUNCH_SHELL_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Request an interactive shell from the desktop action path.
pub fn request_shell_launch() {
    LAUNCH_SHELL_REQUESTED.store(true, Ordering::Release);
}

/// Consume one pending interactive-shell request. Only the launchd syscall
/// handler calls this, after verifying that the caller is PID 1.
pub fn take_shell_launch_request() -> bool {
    LAUNCH_SHELL_REQUESTED.swap(false, Ordering::Acquire)
}

/// Read CMOS RTC and convert to microseconds since Unix epoch (1970-01-01 00:00:00 UTC).
/// Returns `None` if RTC is unavailable or invalid.
fn read_rtc_us() -> Option<u64> {
    // Obtain wall-clock callback from Solvent
    let cb = solvent::RUNTIME_CONTEXT.callback_snapshot().wall_clock?;
    let (year, month, day, hour, minute, second) = cb()?;

    // Validate ranges
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    // Convert to days since Unix epoch (1970-01-01)
    // Algorithm based on standard calendar calculations
    let mut y = year as i64;
    let mut m = month as i64;

    // Adjust for months: March = 0, ..., Feb = 11 (makes leap-year math easier)
    if m <= 2 {
        y -= 1;
        m += 12;
    }

    // Days since epoch using Zeller-style formula
    let days_since_epoch = (365 * y) + (y / 4) - (y / 100) + (y / 400)  // Years to days with leap years
        + (30 * m + 3 * (m + 1) / 5)                  // Months to days
        + (day as i64)                                 // Add day of month
        - 719561; // Adjust to Unix epoch (days from year 0 to 1970-01-01)

    // Convert to seconds
    let total_seconds =
        days_since_epoch * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);

    // Convert to microseconds
    if total_seconds < 0 {
        return None; // Time before Unix epoch
    }

    Some((total_seconds as u64) * 1_000_000)
}

/// NMI recovery dedicated stack (writable, 16-byte aligned).
/// Must be mutable so recovery pushes can write to it without faulting.
#[repr(align(16))]
struct AlignedStack {
    _bytes: [u8; 65536],
}

#[allow(dead_code)]
static mut NMI_RECOVERY_STACK: AlignedStack = AlignedStack { _bytes: [0; 65536] };

/// Start PID 1 through the same ELF loader used by every user program.
/// The first allocator PID after the scheduler's internal idle slot must be
/// one; failure is fatal because running without init would orphan lifecycle
/// ownership and reaping.
fn bootstrap_launchd() {
    if let Some((pid, state)) = SCHEDULER.with_list(|list| {
        list.iter()
            .find(|(_, process)| process.role == crate::process::ProcessRole::Init)
            .map(|(pid, process)| (*pid, process.state))
    }) {
        petroleum::serial::serial_log(format_args!(
            "retaining existing launchd PID {} during scheduler recovery ({:?})\n",
            pid.0, state
        ));
        return;
    }
    let pid = crate::loader::load_program(LAUNCHD_IMAGE, "launchd")
        .expect("failed to load launchd as PID 1");
    assert_eq!(pid.0, 1, "launchd did not receive PID 1");
    SCHEDULER.with_process(pid, |process| {
        process.role = crate::process::ProcessRole::Init;
    });
    petroleum::serial::serial_log(format_args!("launchd bootstrapped as PID 1\n"));
}

/// Main kernel scheduler loop.
///
/// Renders the initial desktop, bootstraps launchd after GUI readiness, then
/// enters an idle loop that drives `gui::runtime_tick()`. launchd consumes
/// desktop shell-launch requests and creates the native shell through the ABI;
/// the scheduler itself does not create the shell.
pub fn scheduler_loop() -> ! {
    solvent::install_scheduler_yield(crate::process::yield_from_scheduler_stack);
    let boot_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let tsc_per_ms = solvent::get_tsc_per_ms();
    let boot_ms_est = if tsc_per_ms > 0 {
        boot_tsc / tsc_per_ms
    } else {
        0
    };
    petroleum::serial::serial_log(format_args!(
        "[boot] scheduler_loop at ~{} ms (TSC freq {} Hz)\n",
        boot_ms_est,
        tsc_per_ms * 1000,
    ));

    // Render initial desktop frame.
    gui::render();

    bootstrap_launchd();
    // Give PID 1 its first turn before the device phase. This is both a
    // useful bootstrap invariant and prevents a slow optional driver from
    // delaying init's ability to create its managed endpoints.
    crate::process::yield_current();

    // Wire kernel renderer into Solvent so runtime ticks can paint the display.
    gui::set_render_fn(gui::render);
    gui::set_cursor_render_fn(gui::render_cursor);

    // Exercise the same command registration, shell service, VFS loader, and
    // cooperative scheduling path used by an interactive invocation.
    #[cfg(ipc_kernel_smoke)]
    crate::shell::run_ipc_kernel_smoke();
    #[cfg(linux_musl_smoke)]
    crate::shell::run_linux_musl_smoke();
    #[cfg(linux_busybox_smoke)]
    crate::shell::busybox_smoke();
    // Register NMI recovery restart context with a dedicated stack.
    let recovery_rsp = {
        let base = core::ptr::addr_of!(NMI_RECOVERY_STACK) as u64;
        VirtAddr::new((base + core::mem::size_of::<[u8; 4096]>() as u64) & !15u64)
    };
    SCHEDULER.set_recovery(
        recovery_rsp,
        VirtAddr::from_ptr(mmio_recovery_restart as *const ()),
    );

    #[cfg(usb_xhci_smoke)]
    crate::shell::usb_xhci_smoke();

    // Idle loop: drive runtime ticks.
    // Shell and other apps are launched via AppGrid or context menu.
    loop {
        if SCHEDULER.current_tick().is_multiple_of(1_000) {
            let (count, total_tsc, max_tsc) = nitrogen::i2c_hid::input_service_metrics();
            let (pointer_count, pointer_max_tsc) = solvent::pointer_latency_metrics();
            let average_tsc = if count == 0 { 0 } else { total_tsc / count };
            petroleum::serial::serial_log(format_args!(
                "[input] hid_services={} avg_tsc={} max_tsc={} pointer_events={} pointer_max_tsc={} tsc_per_ms={}\n",
                count,
                average_tsc,
                max_tsc,
                pointer_count,
                pointer_max_tsc,
                solvent::get_tsc_per_ms(),
            ));
        }

        // VDSO: update time metadata for all processes.
        // Compute monotonic uptime in microseconds
        let uptime_us = if solvent::get_tsc_per_ms() > 0 {
            let tsc = unsafe { core::arch::x86_64::_rdtsc() };
            (tsc as u128 * 1000 / solvent::get_tsc_per_ms() as u128) as u64
        } else {
            crate::interrupts::TICK_COUNTER.load(Ordering::Relaxed)
        };

        // Obtain wall-clock time from RTC; fallback to uptime if RTC unavailable
        let wall_us = read_rtc_us().unwrap_or(uptime_us);

        SCHEDULER.update_vdso_all(uptime_us, wall_us);

        // Device SQs are submitted by services and consumed here, in the
        // scheduler context. Their CQs are drained after execution so a GUI
        // or service tick never performs firmware/MMIO work synchronously.
        let device_phase_deadline = unsafe { core::arch::x86_64::_rdtsc() }
            .saturating_add(solvent::get_tsc_per_ms().max(1).saturating_mul(10));
        #[cfg(not(nitrogen_no_usb))]
        {
            crate::drivers::registry::process_usb_submission_queue_until(1, device_phase_deadline);
            crate::drivers::registry::consume_usb_completion_queue(1);
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: USB phase returned");
        }
        // USB rescan is an explicit user request. Run it before the generic
        // storage SQ: a Gemibook storage/MMIO probe must not strand an
        // already-accepted USB request before its activation boundary.
        #[cfg(not(nitrogen_no_storage))]
        {
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: storage phase begin");
            crate::drivers::registry::process_driver_submission_queue_until(
                8,
                device_phase_deadline,
            );
            crate::drivers::registry::consume_driver_completion_queue_until(
                8,
                device_phase_deadline,
            );
            crate::drivers::registry::usb_rescan_scheduler_diag(
                "scheduler: storage phase returned",
            );
        }
        // Pump HID + cursor after the storage/USB phases. USB requests must
        // get the first scheduler opportunity: on machines whose I2C-HID
        // transaction does not return, a pre-device HID pump would strand an
        // already-accepted USB request before its activation boundary.
        crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: HID phase begin");
        gui::pump_hid_cursor();
        crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: HID phase returned");
        crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: audio phase begin");
        crate::contexts::audio::process_audio_submission_queue(2);
        crate::contexts::audio::poll_audio_playback();
        crate::contexts::audio::consume_audio_completion_queue(4);
        crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: audio phase returned");

        // Drain requests left by the preceding GUI tick before entering the
        // next one. This closes the gap where a nested/runtime-driven tick
        // can enqueue Wi-Fi initialization while the scheduler has not yet
        // reached the normal post-GUI device phase.
        #[cfg(not(nitrogen_no_iwlwifi))]
        {
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: WiFi phase begin");
            let wifi_phase_deadline = unsafe { core::arch::x86_64::_rdtsc() }
                .saturating_add(solvent::get_tsc_per_ms().max(1).saturating_mul(2));
            nitrogen::iwlwifi::process_wifi_submission_queue_until(16, wifi_phase_deadline);
            nitrogen::iwlwifi::consume_wifi_completion_queue_until(16, wifi_phase_deadline);
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: WiFi phase returned");
        }
        // BusyBox smoke is a synchronous ABI test. During the harness, the
        // nested runtime pump handles only input and rendering; after a
        // physical smoke run returns, normal desktop ticks resume.
        #[cfg(not(linux_busybox_smoke))]
        {
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: GUI phase begin");
            gui::runtime_tick(SCHEDULER.current_tick());
            crate::process::service_terminal_close_request();
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: GUI phase returned");
        }
        #[cfg(linux_busybox_smoke)]
        if !solvent::headless_smoke_active() {
            gui::runtime_tick(SCHEDULER.current_tick());
            crate::process::service_terminal_close_request();
        }

        // The GUI/service tick above is the producer for Wi-Fi requests (the
        // network menu enqueues InitStep there). Process Wi-Fi after it so a
        // newly requested initialization does not wait for another scheduler
        // turn or get stranded behind a lifecycle timeout. Wi-Fi has its own
        // per-phase MMIO/PCIe watchdogs; do not use the shared storage-device
        // deadline for this path.
        #[cfg(not(nitrogen_no_iwlwifi))]
        {
            crate::drivers::registry::usb_rescan_scheduler_diag("scheduler: post-GUI WiFi begin");
            let wifi_phase_deadline = unsafe { core::arch::x86_64::_rdtsc() }
                .saturating_add(solvent::get_tsc_per_ms().max(1).saturating_mul(2));
            nitrogen::iwlwifi::process_wifi_submission_queue_until(16, wifi_phase_deadline);
            nitrogen::iwlwifi::consume_wifi_completion_queue_until(16, wifi_phase_deadline);
            crate::drivers::registry::usb_rescan_scheduler_diag(
                "scheduler: post-GUI WiFi returned",
            );
        }
        // The timer interrupt intentionally does not preempt. Give ready
        // kernel tasks (for example a WASM viewer launched from the desktop)
        // an explicit scheduling point before idling again.
        if SCHEDULER.active_count() > 1 {
            crate::process::yield_current();
        }
        // A task may only release its address space after switching away
        // from it. Reap terminated tasks from the idle context.
        SCHEDULER.cleanup();
        SCHEDULER.advance_tick();
        x86_64::instructions::hlt();
    }
}

/// Restart the scheduler loop after an NMI watchdog recovery.
/// Called from the timer ISR on a fresh stack.
#[unsafe(no_mangle)]
pub extern "C" fn mmio_recovery_restart() -> ! {
    petroleum::serial::serial_log(format_args!(
        "[mmio_recovery_restart] WiFi init hung, restarting scheduler loop\n"
    ));
    // Force-reset the APIC_CONTROLLER lock in case the hung context held it.
    unsafe {
        crate::interrupts::apic::reset_apic_controller_lock();
    }
    // The watchdog may have interrupted I2C-HID while its controller object
    // held the stale BAR mapping. Do not let the next scheduler tick poll or
    // drop that object and issue another MMIO transaction.
    nitrogen::i2c_hid::disable_after_mmio_fault();
    #[cfg(not(nitrogen_no_iwlwifi))]
    nitrogen::iwlwifi::force_init_failed();
    scheduler_loop()
}
