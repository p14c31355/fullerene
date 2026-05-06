//! User memory validation functions
//!
//! This module provides functions for validating user space memory access,
//! used by syscall handlers and memory management.
use crate::common::logging::{SystemError, SystemResult};
use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::VirtAddr;

/// Heap start address
pub static HEAP_START: AtomicUsize = AtomicUsize::new(0);

/// Heap end address (start + size)
pub static HEAP_END: AtomicUsize = AtomicUsize::new(0);

/// Physical memory offset for virtual to physical address translation
pub static PHYSICAL_MEMORY_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Set heap range for allocator-related page fault detection
pub fn set_heap_range(start: usize, size: usize) {
    HEAP_START.store(start, Ordering::SeqCst);
    HEAP_END.store(start + size, Ordering::SeqCst);
}

/// Set the physical memory offset for virtual to physical address translation
pub fn set_physical_memory_offset(offset: usize) {
    PHYSICAL_MEMORY_OFFSET.store(offset, Ordering::Relaxed);
}

/// Get the physical memory offset for virtual to physical address translation
pub fn get_physical_memory_offset() -> usize {
    PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed)
}

/// Convert virtual address to physical address using the offset
pub fn virtual_to_physical(virtual_addr: usize) -> usize {
    virtual_addr - get_physical_memory_offset()
}

/// Convert physical address to virtual address using the offset
pub fn physical_to_virtual(physical_addr: usize) -> usize {
    physical_addr + get_physical_memory_offset()
}

/// Check if an address is in user space
pub fn is_user_address(addr: VirtAddr) -> bool {
    // User space is typically 0x0000000000000000 to 0x00007FFFFFFFFFFF
    // Kernel space is 0xFFFF800000000000 and above
    addr.as_u64() < 0x0000800000000000
}

/// Check if an address is within the allocator's heap range
pub fn is_allocator_related_address(addr: usize) -> bool {
    let start = HEAP_START.load(Ordering::SeqCst);
    let end = HEAP_END.load(Ordering::SeqCst);
    if start != 0 {
        addr >= start && addr < end
    } else {
        false
    }
}

/// Safe wrapper for allocating memory with a given layout
pub fn allocate_layout(layout: Layout) -> Result<*mut u8, SystemError> {
    let ptr = unsafe { alloc::alloc::alloc(layout) };
    if ptr.is_null() {
        Err(SystemError::MemOutOfMemory)
    } else {
        Ok(ptr)
    }
}

/// Safe wrapper for deallocating memory with a given layout
pub fn deallocate_layout(ptr: *mut u8, layout: Layout) {
    unsafe { alloc::alloc::dealloc(ptr, layout) };
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