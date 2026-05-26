//! Input device interrupt handlers
//!
//! This module handles keyboard and mouse interrupts.

use super::apic::send_eoi;
use petroleum::port_read_u8;
use x86_64::structures::idt::InterruptStackFrame;

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
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Increment global tick counter (lock-free atomic increment)
    super::TICK_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    send_eoi();
}
