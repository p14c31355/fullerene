//! User Space Memory Validation Functions
//!
//! This module provides functions for validating user space memory access and managing inter-process memory operations.

use super::*;

/// Helper functions for user space memory validation
pub mod user_space {
    use super::*;

    /// Check if an address is in user space
    pub fn is_user_address(addr: x86_64::VirtAddr) -> bool {
        // User space is typically 0x0000000000000000 to 0x00007FFFFFFFFFFF
        // Kernel space is 0xFFFF800000000000 and above
        addr.as_u64() < 0x0000800000000000
    }

    /// Map a user page for kernel access
    pub fn map_user_page(
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            manager.map_page(virtual_addr, physical_addr, flags)
        } else {
            Err(SystemError::InternalError)
        }
    }

    /// Validate user buffer access
    pub fn validate_user_buffer(
        ptr: usize,
        count: usize,
        allow_kernel: bool,
    ) -> Result<(), crate::syscall::interface::SyscallError> {
        use x86_64::VirtAddr;

        if ptr == 0 && count == 0 {
            return Ok(());
        }

        let start = VirtAddr::new(ptr as u64);
        if !allow_kernel && !is_user_address(start) {
            return Err(crate::syscall::interface::SyscallError::InvalidArgument);
        }

        if count == 0 {
            return Ok(());
        }

        if let Some(end_ptr) = ptr.checked_add(count - 1) {
            let end = VirtAddr::new(end_ptr as u64);
            if !allow_kernel && !is_user_address(end) {
                return Err(crate::syscall::interface::SyscallError::InvalidArgument);
            }
        } else {
            return Err(crate::syscall::interface::SyscallError::InvalidArgument);
        }

        Ok(())
    }
}

// Re-export functions for easier access
pub use user_space::{is_user_address, map_user_page};
