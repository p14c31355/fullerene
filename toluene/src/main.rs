#![no_std]
#![no_main]

mod user;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Write to stdout
    let message = b"Hello from toluene user program!\n";
    let _ = user::write(1, message);

    // Get our PID and display it
    let pid = user::getpid();
    let pid_msg1 = b"My PID is: ";
    let pid_msg2 = b"\n";
    let _ = user::write(1, pid_msg1);

    // Simple PID conversion to string (for demo)
    let pid_bytes = &[b'0' + (pid % 10) as u8];
    let _ = user::write(1, pid_bytes);
    let _ = user::write(1, pid_msg2);

    // Sleep a bit to simulate work
    for _ in 0..10 {
        user::sleep();
    }

    // Write final message
    let message2 = b"Toluene program finished executing.\n";
    let _ = user::write(1, message2);

    // Exit gracefully
    user::exit(0);
}
