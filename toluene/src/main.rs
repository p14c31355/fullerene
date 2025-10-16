#![no_std]
#![no_main]

//! User space system call wrappers for toluene

/// Simple system call wrapper (for user space programs)
#[inline]
unsafe fn syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    let result: u64;
    // Use syscall instruction with System V ABI (x86-64)
    // RAX = syscall number, RDI/RSI/RDX/R10/R8/R9 = arguments
    core::arch::asm!(
        "syscall",
        in("rax") syscall_num,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rax") result,
        // syscall may clobber rcx and r11 per ABI
        out("rcx") _, out("r11") _,
    );
    result
}

/// Simple write syscall wrapper
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        syscall(
            4, // SYS_WRITE
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        ) as i64
    }
}

/// Simple exit syscall wrapper
pub fn exit(code: i32) {
    unsafe {
        syscall(1, code as u64, 0, 0, 0, 0, 0);
    }
}

/// Get PID syscall wrapper
pub fn getpid() -> u64 {
    unsafe { syscall(20, 0, 0, 0, 0, 0) }
}

/// Yield syscall wrapper
pub fn sleep() {
    unsafe {
        syscall(22, 0, 0, 0, 0, 0);
    }
}

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
