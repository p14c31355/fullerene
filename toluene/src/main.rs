#![no_std]
#![no_main]

//! User space system call wrappers for toluene

use petroleum::syscall::{exit, getpid, sleep, write};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Write to stdout
    let message = b"Hello from toluene user program!\n";
    let _ = write(1, message);

    // Get our PID and display it
    let pid = getpid() as usize;
    let pid_msg1 = b"My PID is: ";
    let pid_msg2 = b"\n";
    let _ = write(1, pid_msg1);

    let mut pid_buffer = [0u8; 20];
    let len = petroleum::serial::format_dec_to_buffer(pid, &mut pid_buffer);
    let pid_bytes = &pid_buffer[..len];
    let _ = write(1, pid_bytes);
    let _ = write(1, pid_msg2);

    // Sleep a bit to simulate work
    for _ in 0..10 {
        sleep();
    }

    // Write final message
    let message2 = b"Toluene program finished executing.\n";
    let _ = write(1, message2);

    // Exit gracefully
    exit(0);
}
