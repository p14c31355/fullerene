// Syscall submodules
pub mod interface;
pub mod handlers;
pub mod user;

// Re-export public API
pub use interface::*;
pub use handlers::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(SyscallNumber::Exit as u64, 1);
        assert_eq!(SyscallNumber::Write as u64, 4);
        assert_eq!(SyscallNumber::Read as u64, 3);
    }

    #[test]
    fn test_exit_syscall() {
        // This would normally exit, but in test we can't
        let result = super::handlers::syscall_exit(0);
        assert!(result.is_ok());
    }
}
