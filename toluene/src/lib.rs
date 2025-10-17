
/// Simple write syscall wrapper
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        petroleum::syscall(
            petroleum::SyscallNumber::Write as u64,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        ) as i64
    }
}

/// Simple exit syscall wrapper
pub fn exit(code: i32) -> ! {
    unsafe {
        petroleum::syscall(petroleum::SyscallNumber::Exit as u64, code as u64, 0, 0, 0, 0, 0);
    }
    loop {} // unreachable, but to make ! return type
}

/// Get PID syscall wrapper
pub fn getpid() -> u64 {
    unsafe { petroleum::syscall(petroleum::SyscallNumber::GetPid as u64, 0, 0, 0, 0, 0, 0) }
}

/// Yield syscall wrapper
pub fn sleep() {
    unsafe {
        petroleum::syscall(petroleum::SyscallNumber::Yield as u64, 0, 0, 0, 0, 0, 0);
    }
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }
}
