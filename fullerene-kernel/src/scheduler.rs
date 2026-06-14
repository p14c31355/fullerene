//! System scheduler — thin entry point into the shell/event loop.
//!
//! The actual scheduling is handled by the Solvent runtime via
//! `gui::runtime_tick()`; this module simply bootstraps the shell
//! and enters the cooperative event loop.

use crate::gui;

/// Main kernel scheduler loop.
///
/// Delegates to the shell which cooperatively yields to the runtime
/// (mouse, timers, rendering) inside `LatticeTerminal::read_byte()`.
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

    // Enter cooperative shell loop (shell yields → runtime ticks → back to shell).
    crate::shell::shell_main();

    petroleum::halt_loop();
}

/// Shell entry-point for process spawning.
pub extern "C" fn shell_process_main() -> ! {
    log::info!("Shell process started");
    crate::shell::shell_main();
    crate::process::terminate_process(crate::process::current_pid().unwrap(), 0);
    petroleum::halt_loop();
}
