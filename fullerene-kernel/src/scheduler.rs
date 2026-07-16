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
//!   ├── solvent::poll_*()       — poll input devices (no interrupt path)
//!   ├── gui::runtime_tick()     — solvent tick_core + framebuffer render
//!   ├── shell launch check      — via KERNEL lock (independent of SCHEDULER)
//!   ├── advance_tick()
//!   └── hlt()
//! ```

use core::sync::atomic::Ordering;
use x86_64::VirtAddr;

use crate::gui;
use crate::scheduler_context::SCHEDULER;

/// Read CMOS RTC and convert to microseconds since Unix epoch (1970-01-01 00:00:00 UTC).
/// Returns `None` if RTC is unavailable or invalid.
fn read_rtc_us() -> Option<u64> {
    // Obtain wall-clock callback from Solvent
    let cb = solvent::RUNTIME_CONTEXT.callbacks().wall_clock?;
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

/// Set the launch‑shell flag from the solvent side.
pub fn request_shell_launch() {
    crate::contexts::kernel::with_kernel(|k| {
        k.shell.request_launch();
    });
}

/// Main kernel scheduler loop.
///
/// Renders the initial desktop, then enters an idle loop that drives
/// `gui::runtime_tick()`.  Shell (and future apps) are launched on
/// demand.
pub fn scheduler_loop() -> ! {
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

    // Wire kernel renderer into Solvent so runtime ticks can paint the display.
    gui::set_render_fn(gui::render);

    // Register NMI recovery restart context with a dedicated stack.
    let recovery_rsp = {
        let base = core::ptr::addr_of!(NMI_RECOVERY_STACK) as u64;
        VirtAddr::new((base + core::mem::size_of::<[u8; 4096]>() as u64) & !15u64)
    };
    SCHEDULER.set_recovery(
        recovery_rsp,
        VirtAddr::from_ptr(mmio_recovery_restart as *const ()),
    );

    // Idle loop: drive runtime ticks.
    // Shell and other apps are launched via AppGrid or context menu.
    loop {
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

        // Poll input devices before the runtime tick so that even
        // without interrupt delivery (some firmware / VM configs) the
        // desktop remains responsive and doesn't hang after the first
        // rendered frame.
        solvent::poll_mouse_state();
        solvent::poll_keyboard();

        gui::runtime_tick(SCHEDULER.current_tick());

        // Check if the user requested a shell launch (via AppGrid / menu).
        if crate::contexts::kernel::with_kernel(|k| k.shell.take_launch_request()).unwrap_or(false)
        {
            petroleum::serial::_print(format_args!("Launching shell on demand\n"));
            crate::shell::shell_main();
            // After shell exits, re‑render the desktop and keep idling.
            gui::render();
            petroleum::serial::_print(format_args!("Shell exited, back to idle\n"));
        }

        SCHEDULER.advance_tick();
        x86_64::instructions::hlt();
    }
}

/// Shell entry-point for process spawning.
pub extern "C" fn shell_process_main() -> ! {
    log::info!("Shell process started");
    crate::shell::shell_main();
    crate::process::terminate_process(crate::process::current_pid().unwrap(), 0);
    petroleum::halt_loop();
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
    #[cfg(not(nitrogen_no_iwlwifi))]
    nitrogen::iwlwifi::force_init_failed();
    scheduler_loop()
}
