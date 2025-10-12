//! Input device interrupt handlers
//!
//! This module handles keyboard and mouse interrupts.

use super::apic::send_eoi;
use petroleum::port_read_u8;
use spin::Mutex;
use x86_64::structures::idt::InterruptStackFrame;

/// Keyboard queue structure
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyboardQueue {
    pub buffer: [u8; 256],
    pub head: usize,
    pub tail: usize,
}

impl KeyboardQueue {
    pub const fn new() -> Self {
        KeyboardQueue {
            buffer: [0; 256],
            head: 0,
            tail: 0,
        }
    }
}

/// Mouse state structure
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseState {
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
    pub packet: [u8; 3],
    pub packet_idx: usize,
}

impl MouseState {
    pub const fn new() -> Self {
        MouseState {
            x: 0,
            y: 0,
            buttons: 0,
            packet: [0; 3],
            packet_idx: 0,
        }
    }
}

/// Global input device state
pub static KEYBOARD_QUEUE: Mutex<KeyboardQueue> = Mutex::new(KeyboardQueue::new());
pub static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState::new());

/// Macro to create input device interrupt handlers
#[macro_export]
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

/// Keyboard interrupt handler
define_input_interrupt_handler!(keyboard_handler, 0x60, |scancode: u8| {
    use petroleum::lock_and_modify;

    // Enqueue scancode to keyboard queue
    lock_and_modify!(KEYBOARD_QUEUE, queue, {
        let tail = queue.tail;
        queue.buffer[tail] = scancode;
        queue.tail = (queue.tail + 1) % queue.buffer.len();
        if queue.tail == queue.head {
            // Queue full, drop oldest
            queue.head = (queue.head + 1) % queue.buffer.len();
        }
    });

    // Call keyboard driver to handle scancode
    crate::keyboard::handle_keyboard_scancode(scancode);
});

/// Mouse interrupt handler
define_input_interrupt_handler!(mouse_handler, 0x60, |byte: u8| {
    use petroleum::lock_and_modify;

    lock_and_modify!(MOUSE_STATE, mouse, {
        let current_idx = mouse.packet_idx;
        mouse.packet[current_idx] = byte;
        mouse.packet_idx += 1;

        if mouse.packet_idx == 3 {
            // Full packet received, process
            let status = mouse.packet[0];
            let dx = mouse.packet[1] as i8 as i16;
            let dy = mouse.packet[2] as i8 as i16;

            mouse.x = mouse.x.wrapping_add(dx);
            mouse.y = mouse.y.wrapping_add(dy);
            mouse.buttons = status & 0x07;

            mouse.packet_idx = 0;
            mouse.packet = [0; 3];
        }
    });
});

/// Timer interrupt handler
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    use petroleum::lock_and_modify;

    // Increment global tick counter
    lock_and_modify!(super::TICK_COUNTER, counter, {
        *counter += 1;
    });

    // Perform scheduling
    unsafe {
        let old_pid = crate::process::current_pid();
        crate::process::schedule_next();
        let new_pid = crate::process::current_pid();

        if let (Some(old_pid_val), Some(new_pid_val)) = (old_pid, new_pid) {
            if old_pid_val != new_pid_val {
                crate::process::context_switch(old_pid, new_pid_val);
            }
        }
    }

    send_eoi();
}
