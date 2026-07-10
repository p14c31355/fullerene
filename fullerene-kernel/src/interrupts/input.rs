//! Input device interrupt handlers
//!
//! This module handles keyboard and mouse interrupts.

use super::apic::send_eoi;
use petroleum::port_read_u8;
use x86_64::structures::idt::{InterruptStackFrame, InterruptStackFrameValue};

/// Macro to create input device interrupt handlers
macro_rules! define_input_interrupt_handler {
    ($handler_name:ident, $port:expr, $process_input:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $handler_name(_stack_frame: InterruptStackFrame) {
            let data = port_read_u8!($port);
            $process_input(data);
            send_eoi();
        }
    };
}

// Keyboard interrupt handler
//
// Reads one byte from the PS/2 data port and feeds it to the Nitrogen
// PS/2 keyboard driver for scancode processing.  The driver handles
// scancode-to-ASCII conversion, modifier keys, and input buffering.
define_input_interrupt_handler!(keyboard_handler, 0x60, |scancode: u8| {
    nitrogen::ps2::keyboard::handle_keyboard_scancode(scancode);
});

// Mouse interrupt handler
//
// Reads one byte from the PS/2 data port and feeds it to the Nitrogen
// PS/2 mouse driver for packet processing.  No manual packet parsing
// is performed here – the driver handles that with proper validation.
define_input_interrupt_handler!(mouse_handler, 0x60, |byte: u8| {
    nitrogen::ps2::mouse::handle_mouse_data(byte);
});

/// Timer interrupt handler (no preemption - scheduler loop handles yielding)
/// Also detects NMI MMIO watchdog recovery and redirects to the scheduler loop.
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn timer_handler(mut frame: InterruptStackFrame) {
    // Increment global tick counter (lock-free atomic increment)
    super::TICK_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    if nitrogen::mmio::mmio_watchdog_recovery_triggered() {
        petroleum::serial::serial_log(format_args!(
            "[timer_handler] NMI recovery triggered — jumping to scheduler_loop\n"
        ));
        let restart_fn = crate::scheduler_context::SCHEDULER.recovery_target();
        if let Some((rsp, rip)) = restart_fn {
            let new_frame = InterruptStackFrameValue::new(
                rip,
                frame.code_segment,
                frame.cpu_flags,
                rsp,
                frame.stack_segment,
            );
            unsafe {
                frame.as_mut().write(new_frame);
            }
            // Clear the trigger only after successfully writing the new frame.
            // If no restart target is available, leave the trigger set so a
            // later recovery attempt can succeed.
            nitrogen::mmio::clear_watchdog_recovery_trigger();
        }
    }

    send_eoi();
}
