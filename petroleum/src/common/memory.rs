//! User memory validation functions
//!
//! This module provides functions for validating user space memory access,
//! used by syscall handlers and memory management.
use crate::common::logging::{SystemError, SystemResult};
use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::VirtAddr;

// ── RUNTIME GLOBAL STATE ──────────────────────────────────────────────
// These statics are set once during kernel initialisation and remain valid
// for the entire kernel lifetime (they live in the BSS which is identity-
// + higher-half mapped). They are NOT early-only.

/// Heap start address - volatile for bare-metal reliability
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

/// Get the current heap range (start, end)
pub fn get_heap_range() -> (usize, usize) {
    let start = HEAP_START.load(Ordering::SeqCst);
    let end = HEAP_END.load(Ordering::SeqCst);
    (start, end)
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

/// Safely create a slice from a physical address and length.
///
/// # Safety
/// The caller must ensure that the physical memory range is mapped and accessible.
pub unsafe fn phys_to_slice(phys_addr: usize, len: usize) -> &'static [u8] {
    unsafe {
        let virt_addr = physical_to_virtual(phys_addr);
        core::slice::from_raw_parts(virt_addr as *const u8, len)
    }
}

/// Safely create a mutable slice from a physical address and length.
///
/// # Safety
/// The caller must ensure that the physical memory range is mapped and accessible.
pub unsafe fn phys_to_slice_mut(phys_addr: usize, len: usize) -> &'static mut [u8] {
    unsafe {
        let virt_addr = physical_to_virtual(phys_addr);
        core::slice::from_raw_parts_mut(virt_addr as *mut u8, len)
    }
}

/// Check if an address is in user space
pub fn is_user_address(addr: VirtAddr) -> bool {
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

/// Validated pointer to user-space memory.
///
/// `UserPtr` represents a pointer that has been validated to point into
/// the user address range.  Access is performed through explicit copy
/// operations rather than returning borrowed slices, so the kernel
/// always owns its copies of user data.
#[derive(Debug, Clone, Copy)]
pub struct UserPtr<T: ?Sized> {
    ptr: *const T,
}

impl<T> UserPtr<T> {
    /// Create a `UserPtr` from a raw pointer, validating it is in user space.
    pub fn new(ptr: *const T) -> SystemResult<Self> {
        if ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        let addr = ptr as u64;
        if addr >= 0x0000800000000000 {
            return Err(SystemError::PermissionDenied);
        }
        Ok(Self { ptr })
    }

    /// Create a `UserPtr` from a raw mutable pointer.
    pub fn new_mut(ptr: *mut T) -> SystemResult<Self> {
        Self::new(ptr as *const T)
    }

    /// Copy a value from user space into kernel-owned memory.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `T` is valid for the memory at the pointer.
    pub unsafe fn copy_from_user(&self) -> Result<T, SystemError> {
        unsafe { Ok((self.ptr as *const T).read()) }
    }

    /// Copy a value into user space.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `T` is valid for the memory at the pointer
    /// and that the user buffer is writable.
    pub unsafe fn copy_to_user(&self, val: T) -> SystemResult<()> {
        unsafe {
            (self.ptr as *mut T).write_volatile(val);
        }
        Ok(())
    }

    /// Get the raw pointer (for use in syscall ABI where needed).
    pub fn as_raw_ptr(&self) -> *const T {
        self.ptr
    }
}

/// A validated user-space byte slice for safe copy operations.
///
/// Unlike `user_slice` which returns `&'static [u8]`, `UserSlice` only
/// provides copy-in/copy-out operations, ensuring the kernel owns its
/// data copies.
#[derive(Debug, Clone, Copy)]
pub struct UserSlice {
    ptr: *mut u8,
    len: usize,
}

impl UserSlice {
    /// Validate a user-space buffer range and create a `UserSlice`.
    ///
    /// Checks:
    /// - Non-null pointer
    /// - Entire range is in user space
    /// - No arithmetic overflow
    pub fn new(ptr: *mut u8, len: usize) -> SystemResult<Self> {
        if len == 0 {
            return Ok(Self { ptr, len: 0 });
        }
        if ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        // Check the start address
        let start = ptr as u64;
        if start >= 0x0000800000000000 {
            return Err(SystemError::PermissionDenied);
        }
        // Check the end address (with overflow guard)
        let end = start.checked_add(len as u64).ok_or(SystemError::InvalidArgument)?;
        if end > 0x0000800000000000 {
            return Err(SystemError::PermissionDenied);
        }
        Ok(Self { ptr, len })
    }

    /// Return the length of the slice.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copy data FROM user space INTO a kernel-owned buffer.
    ///
    /// # Safety
    ///
    /// The caller must ensure the user pages are mapped.  Page faults
    /// during copy are caught by the kernel's page fault handler.
    pub unsafe fn copy_from_user(&self, buf: &mut [u8]) -> SystemResult<()> {
        let count = buf.len().min(self.len);
        if count == 0 {
            return Ok(());
        }
        unsafe {
            core::ptr::copy_nonoverlapping(self.ptr, buf.as_mut_ptr(), count);
        }
        Ok(())
    }

    /// Copy data FROM a kernel-owned buffer INTO user space.
    ///
    /// # Safety
    ///
    /// The caller must ensure the user pages are mapped and writable.
    pub unsafe fn copy_to_user(&self, buf: &[u8]) -> SystemResult<()> {
        let count = buf.len().min(self.len);
        if count == 0 {
            return Ok(());
        }
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), self.ptr, count);
        }
        Ok(())
    }

    /// Create a `UserSlice` from a raw pointer and length (no validation).
    ///
    /// # Safety
    ///
    /// The caller must guarantee the pointer and length are valid.
    pub unsafe fn from_raw_parts(ptr: *mut u8, len: usize) -> SystemResult<Self> {
        // Minimal null check only; caller guarantees user-space validity.
        if len > 0 && ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        Ok(Self { ptr, len })
    }
}

/// Validate user buffer access (legacy, replaced by `UserSlice`).
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
        return Err(SystemError::InvalidArgument);
    }
    Ok(())
}

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

/// Read a value from user space into kernel-owned memory.
///
/// # Safety
///
/// The caller must ensure the pointer is valid and the memory is mapped.
pub unsafe fn read_user<T>(ptr: *const T) -> Result<T, SystemError> {
    let user = UserPtr::new(ptr)?;
    unsafe { user.copy_from_user() }
}

/// Write a value into user space.
///
/// # Safety
///
/// The caller must ensure the pointer is valid and the memory is mapped writable.
pub unsafe fn write_user<T>(ptr: *mut T, val: T) -> SystemResult<()> {
    let user = UserPtr::new_mut(ptr)?;
    unsafe { user.copy_to_user(val) }
}

/// Copy a byte slice from user space into a kernel-owned buffer.
///
/// Returns the number of bytes copied.
///
/// # Safety
///
/// The caller must ensure the user pointer is valid and mapped.
pub unsafe fn copy_from_user(ptr: *const u8, buf: &mut [u8]) -> SystemResult<usize> {
    let slice = UserSlice::new(ptr as *mut u8, buf.len())?;
    unsafe { slice.copy_from_user(buf)?; }
    Ok(buf.len().min(slice.len()))
}

/// Copy a byte slice from kernel memory into user space.
///
/// # Safety
///
/// The caller must ensure the user pointer is valid, mapped, and writable.
pub unsafe fn copy_to_user(ptr: *mut u8, buf: &[u8]) -> SystemResult<()> {
    let slice = UserSlice::new(ptr, buf.len())?;
    unsafe { slice.copy_to_user(buf) }
}

/// Legacy: create a temporary user-space slice (borrows from user memory).
///
/// NOTE: Prefer `copy_from_user` / `UserSlice` instead.  This returns a
/// `'static` borrow that is unsound if the user buffer is deallocated.
///
/// # Safety
///
/// The caller must ensure the pointer is valid for the entire `'static`
/// lifetime of the returned slice.
pub unsafe fn user_slice(
    ptr: *const u8,
    count: usize,
    allow_kernel: bool,
) -> Result<&'static [u8], SystemError> {
    unsafe {
        validate_user_buffer(ptr as usize, count, allow_kernel)?;
        Ok(core::slice::from_raw_parts(ptr, count))
    }
}

/// Legacy: create a temporary mutable user-space slice.
///
/// NOTE: Prefer `copy_to_user` / `UserSlice` instead.  This returns a
/// `'static` mut borrow that is unsound if the user buffer is deallocated.
///
/// # Safety
///
/// The caller must ensure the pointer is valid for the entire `'static`
/// lifetime of the returned slice.
pub unsafe fn user_slice_mut(
    ptr: *mut u8,
    count: usize,
    allow_kernel: bool,
) -> Result<&'static mut [u8], SystemError> {
    unsafe {
        validate_user_buffer(ptr as usize, count, allow_kernel)?;
        Ok(core::slice::from_raw_parts_mut(ptr, count))
    }
}

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
