#![no_std]
#![no_main]

//! User space system call wrappers for toluene

use petroleum::common::syscall::{exit, getpid, sleep, write};

petroleum::define_panic_handler!();

/// Helper macro to safely write bytes to a file descriptor, ignoring errors.
macro_rules! safe_print {
    ($fd:expr, $msg:expr) => {
        let _ = write($fd, $msg);
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    // Write initial message to stdout
    safe_print!(1, b"Hello from toluene user program!\n");

    // Get our PID and display it
    let pid = getpid() as usize;
    let mut pid_buffer = [0u8; 20];
    let len = petroleum::serial::format_dec_to_buffer(pid, &mut pid_buffer);
    let pid_msg = &pid_buffer[..len];
    safe_print!(1, b"My PID is: ");
    safe_print!(1, pid_msg);
    safe_print!(1, b"\n");

    // Sleep a bit to simulate work
    for _ in 0..10 {
        sleep();
    }

    // Write final message and exit
    safe_print!(1, b"Toluene program finished executing.\n");
    exit(0);
}
