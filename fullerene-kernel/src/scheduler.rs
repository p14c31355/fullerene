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

use core::sync::atomic::{AtomicBool, Ordering};

use crate::gui;

/// Flag set by the solvent callback when the user requests a shell.
static LAUNCH_SHELL: AtomicBool = AtomicBool::new(false);

/// Set the launch‑shell flag from the solvent side.
pub fn request_shell_launch() {
    LAUNCH_SHELL.store(true, Ordering::SeqCst);
}

/// Main kernel scheduler loop.
///
/// Renders the initial desktop, then enters an idle loop that drives
/// `gui::runtime_tick()`.  Shell (and future apps) are launched on
/// demand.
pub fn scheduler_loop() -> ! {
    petroleum::serial::_print(format_args!("Scheduler loop started\n"));

    // Render initial desktop frame.
    gui::render();

    // Verify GOP framebuffer is operational (one-shot diagnostic).
    let kernel_lock = crate::contexts::kernel::get_kernel();
    let kg = kernel_lock.lock();
    match kg.as_ref().and_then(|k| k.framebuffer.info()) {
        Some(info) => match petroleum::graphics::verify_drawing_test(&info) {
            petroleum::graphics::DrawingTestResult::Pass => {
                petroleum::serial::serial_log(format_args!("=== GRAPHICS_TEST PASS ===\n"));
            }
            petroleum::graphics::DrawingTestResult::Fail(msg) => {
                petroleum::serial::serial_log(format_args!(
                    "=== GRAPHICS_TEST FAIL: {} ===\n",
                    msg
                ));
            }
        },
        None => {
            petroleum::serial::serial_log(format_args!(
                "=== GRAPHICS_TEST FAIL: KernelContext.framebuffer has no renderer ===\n"
            ));
        }
    }
    drop(kg);

    // Wire kernel renderer into Solvent so runtime ticks can paint the display.
    gui::set_render_fn(gui::render);

    // Report that the desktop is ready — a clear survival checkpoint.
    petroleum::serial::_print(format_args!(
        "Desktop idle loop — GOP/memory/interrupts/scheduler OK\n"
    ));

    // Idle loop: drive runtime ticks without a shell.
    // Shell and other apps are launched via AppGrid or context menu.
    let mut tick_counter: u64 = 0;
    loop {
        gui::runtime_tick(tick_counter);

        // Check if the user requested a shell launch (via AppGrid / menu).
        if LAUNCH_SHELL.swap(false, Ordering::SeqCst) {
            petroleum::serial::_print(format_args!("Launching shell on demand\n"));
            crate::shell::shell_main();
            // After shell exits, re‑render the desktop and keep idling.
            gui::render();
            petroleum::serial::_print(format_args!("Shell exited, back to idle\n"));
        }

        tick_counter = tick_counter.wrapping_add(1);
        core::hint::spin_loop();
    }
}

/// Shell entry-point for process spawning.
pub extern "C" fn shell_process_main() -> ! {
    log::info!("Shell process started");
    crate::shell::shell_main();
    crate::process::terminate_process(crate::process::current_pid().unwrap(), 0);
    petroleum::halt_loop();
}
