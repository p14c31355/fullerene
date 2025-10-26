/// User memory validation functions
///
/// Provides functions for validating user space memory access,
/// used by syscall handlers and memory management.
use crate::common::logging::{SystemError, SystemResult};
use x86_64::VirtAddr;
use spin::Once;

/// Heap start address
pub static HEAP_START: Once<usize> = Once::new();

/// Heap end address (start + size)
pub static HEAP_END: Once<usize> = Once::new();

/// Set heap range for allocator-related page fault detection
pub fn set_heap_range(start: usize, size: usize) {
    HEAP_START.call_once(|| start);
    HEAP_END.call_once(|| start + size);
}

/// Check if an address is in user space
pub fn is_user_address(addr: VirtAddr) -> bool {
    // User space is typically 0x0000000000000000 to 0x00007FFFFFFFFFFF
    // Kernel space is 0xFFFF800000000000 and above
    addr.as_u64() < 0x0000800000000000
}

/// Check if an address is within the allocator's heap range
pub fn is_allocator_related_address(addr: usize) -> bool {
    if let Some(&start) = HEAP_START.get() {
        if let Some(&end) = HEAP_END.get() {
            return addr >= start && addr < end;
        }
    }
    false
}

/// Validate user buffer access
pub fn validate_user_buffer(ptr: usize, count: usize, allow_kernel: bool) -> SystemResult<()> {
    if count == 0 {
        return Ok(());
    }

    if ptr == 0 {
        return Err(SystemError::InvalidArgument);
    }

    let start = VirtAddr::new(ptr as u64);
    if !allow_kernel && !is_user_address(start) {
        return Err(SystemError::InvalidArgument);
    }

    if let Some(end_ptr) = ptr.checked_add(count - 1) {
        let end = VirtAddr::new(end_ptr as u64);
        if !allow_kernel && !is_user_address(end) {
            return Err(SystemError::InvalidArgument);
        }
    } else {
        // Overflow in end_ptr calculation
        return Err(SystemError::InvalidArgument);
    }

    Ok(())
}

/// Common syscall argument validation helper
pub fn validate_syscall_fd(fd: i32) -> SystemResult<()> {
    if fd < 0 {
        Err(SystemError::InvalidArgument)
    } else {
        Ok(())
    }
}

pub fn validate_syscall_buffer(ptr: usize, allow_kernel: bool) -> SystemResult<()> {
    validate_user_buffer(ptr, 1, allow_kernel)
}

/// Helper function to create framebuffer configuration
pub fn create_framebuffer_config(
    address: u64,
    width: u32,
    height: u32,
    pixel_format: super::uefi::EfiGraphicsPixelFormat,
    bpp: u32,
    stride: u32,
) -> super::uefi::FullereneFramebufferConfig {
    super::uefi::FullereneFramebufferConfig {
        address,
        width,
        height,
        pixel_format,
        bpp,
        stride,
    }
}
