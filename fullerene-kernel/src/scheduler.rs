//! System scheduler — thin entry point into the idle/event loop.
//!
//! The actual scheduling is handled by the Solvent runtime via
//! `gui::runtime_tick()`.  This module renders the initial desktop
//! frame and then enters an idle loop that drives runtime ticks.
//! Shell and other apps are launched on demand via AppGrid or
//! context menu, not started automatically.
//!
//! # Normal boot flow
//!
//! ```text
//! Boot → Desktop → Idle (runtime ticks only)
//!                  → User launches Shell from AppGrid / menu
//! ```
//!
//! This gives a clear survival checkpoint: if the Desktop appears,
//! GOP, memory, interrupts, and the scheduler are all confirmed
//! working.  Any failure after that is isolated to the specific app
//! being launched.

use crate::gui;
use crate::vdso;
use solvent;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::VirtAddr;

/// NMI recovery dedicated stack.
static NMI_RECOVERY_STACK: [u8; 4096] = [0u8; 4096];

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
        let base = NMI_RECOVERY_STACK.as_ptr() as u64;
        VirtAddr::new((base + NMI_RECOVERY_STACK.len() as u64) & !15u64)
    };
    set_recovery_restart(
        recovery_rsp,
        VirtAddr::from_ptr(mmio_recovery_restart as *const ()),
    );

    // Idle loop: drive runtime ticks without a shell.
    // Shell and other apps are launched via AppGrid or context menu.
    let mut tick_counter: u64 = 0;
    loop {
        // VDSO: process pending syscall requests from user processes
        vdso::poll_all_vdso_rings();

        // VDSO: update time metadata for all processes
        let now_us = if solvent::get_tsc_per_ms() > 0 {
            let tsc = unsafe { core::arch::x86_64::_rdtsc() };
            (tsc as u128 * 1000 / solvent::get_tsc_per_ms() as u128) as u64
        } else {
            crate::interrupts::TICK_COUNTER.load(core::sync::atomic::Ordering::Relaxed)
        };
        vdso::update_vdso_metadata(now_us, now_us);

        // Poll input devices before the runtime tick so that even
        // without interrupt delivery (some firmware / VM configs) the
        // desktop remains responsive and doesn't hang after the first
        // rendered frame.
        solvent::poll_mouse_state();
        solvent::poll_keyboard();

        gui::runtime_tick(tick_counter);

        // Check if the user requested a shell launch (via AppGrid / menu).
        if crate::contexts::kernel::with_kernel(|k| k.shell.take_launch_request()).unwrap_or(false)
        {
            petroleum::serial::_print(format_args!("Launching shell on demand\n"));
            crate::shell::shell_main();
            // After shell exits, re‑render the desktop and keep idling.
            gui::render();
            petroleum::serial::_print(format_args!("Shell exited, back to idle\n"));
        }

        tick_counter = tick_counter.wrapping_add(1);
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

// ── NMI recovery restart ───────────────────────────────────────

static RECOVERY_RSP: AtomicU64 = AtomicU64::new(0);
static RECOVERY_RIP: AtomicU64 = AtomicU64::new(0);

/// Set the recovery RSP and RIP for the timer ISR to use after NMI
/// MMIO watchdog recovery.
pub fn set_recovery_restart(rsp: VirtAddr, rip: VirtAddr) {
    RECOVERY_RSP.store(rsp.as_u64(), Ordering::Release);
    RECOVERY_RIP.store(rip.as_u64(), Ordering::Release);
}

/// Get the recovery RSP and RIP for the timer ISR.
pub fn get_recovery_restart_fn() -> Option<(VirtAddr, VirtAddr)> {
    let rsp = RECOVERY_RSP.load(Ordering::Acquire);
    let rip = RECOVERY_RIP.load(Ordering::Acquire);
    if rsp != 0 && rip != 0 {
        Some((VirtAddr::new(rsp), VirtAddr::new(rip)))
    } else {
        None
    }
}

/// Restart the scheduler loop after an NMI watchdog recovery.
/// Called from the timer ISR on a fresh stack.
#[unsafe(no_mangle)]
pub extern "C" fn mmio_recovery_restart() -> ! {
    petroleum::serial::serial_log(format_args!(
        "[mmio_recovery_restart] WiFi init hung, restarting scheduler loop\n"
    ));
    // Safe: no locks held from the hung context on this fresh stack.
    nitrogen::iwlwifi::force_init_failed();
    scheduler_loop()
}
