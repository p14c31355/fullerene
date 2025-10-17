#![no_std]
#![no_main]

//! User space system call wrappers for toluene

use crate::{write, exit, getpid, sleep};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Write to stdout
    let message = b"Hello from toluene user program!\n";
    let _ = write(1, message);

    // Get our PID and display it
    let pid = getpid();
    let pid_msg1 = b"My PID is: ";
    let pid_msg2 = b"\n";
    let _ = write(1, pid_msg1);

    // Convert PID to string to display it
    let mut pid_buffer = [0u8; 20];
    let mut len = 0;
    if pid == 0 {
        pid_buffer[0] = b'0';
        len = 1;
    } else {
        let mut n = pid;
        while n > 0 {
            pid_buffer[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        pid_buffer[..len].reverse();
    }
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
