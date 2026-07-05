//! User memory validation functions
//!
//! This module provides functions for validating user space memory access,
//! used by syscall handlers and memory management.
use crate::common::logging::{SystemError, SystemResult};
use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PageTable, PageTableFlags};
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

/// Walk the current page table (from CR3) to retrieve the flags for a virtual address.
///
/// Walks the full 4-level page table hierarchy. If a huge page (1 GiB or 2 MiB)
/// is encountered at an intermediate level, the flags from that entry are returned.
fn walk_page_table_for_flags(vaddr: VirtAddr) -> Option<PageTableFlags> {
    let offset = get_physical_memory_offset();
    let (p4_frame, _) = Cr3::read();
    let p4_ptr = (p4_frame.start_address().as_u64() as usize + offset) as *const PageTable;
    let p4 = unsafe { &*p4_ptr };

    let p4e = &p4[((vaddr.as_u64() >> 39) & 0x1FF) as usize];
    if !p4e.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    let mut flags = p4e.flags();
    if flags.contains(PageTableFlags::HUGE_PAGE) {
        return Some(flags);
    }

    let p3_ptr = (p4e.addr().as_u64() as usize + offset) as *const PageTable;
    let p3 = unsafe { &*p3_ptr };
    let p3e = &p3[((vaddr.as_u64() >> 30) & 0x1FF) as usize];
    if !p3e.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    flags = flags & p3e.flags();
    if flags.contains(PageTableFlags::HUGE_PAGE) {
        return Some(flags);
    }

    let p2_ptr = (p3e.addr().as_u64() as usize + offset) as *const PageTable;
    let p2 = unsafe { &*p2_ptr };
    let p2e = &p2[((vaddr.as_u64() >> 21) & 0x1FF) as usize];
    if !p2e.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    flags = flags & p2e.flags();
    if flags.contains(PageTableFlags::HUGE_PAGE) {
        return Some(flags);
    }

    let p1_ptr = (p2e.addr().as_u64() as usize + offset) as *const PageTable;
    let p1 = unsafe { &*p1_ptr };
    let p1e = &p1[((vaddr.as_u64() >> 12) & 0x1FF) as usize];
    if !p1e.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    flags = flags & p1e.flags();
    Some(flags)
}

/// Validate that the given user-space address range is fully mapped and
/// accessible according to the specified permissions.
///
/// Walks the current page table (CR3) page by page.
pub fn validate_user_range(addr: *const u8, len: usize, writable: bool) -> Result<(), SystemError> {
    if len == 0 {
        return Ok(());
    }
    let start_addr_u64 = addr as u64;
    let end_addr_u64 = start_addr_u64
        .checked_add(len as u64 - 1)
        .ok_or(SystemError::InvalidArgument)?;

    let start = VirtAddr::try_new(start_addr_u64)
        .map_err(|_| SystemError::InvalidArgument)?;
    let end = VirtAddr::try_new(end_addr_u64)
        .map_err(|_| SystemError::InvalidArgument)?;

    // Must be in user space
    if !is_user_address(start) || !is_user_address(end) {
        return Err(SystemError::PermissionDenied);
    }

    // Walk pages
    let page_start = start.align_down(4096u64);
    let page_end = end.align_down(4096u64);
    let num_pages = ((page_end - page_start) / 4096) + 1;

    for i in 0..num_pages {
        let vaddr = page_start + (i * 4096);
        let flags = walk_page_table_for_flags(vaddr).ok_or(SystemError::InvalidArgument)?;
        if !flags.contains(PageTableFlags::PRESENT) {
            return Err(SystemError::InvalidArgument);
        }
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err(SystemError::PermissionDenied);
        }
        if writable && !flags.contains(PageTableFlags::WRITABLE) {
            return Err(SystemError::PermissionDenied);
        }
    }
    Ok(())
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
/// the user address range with proper page-level permissions.  Access is
/// performed through explicit copy operations rather than returning
/// borrowed slices, so the kernel always owns its copies of user data.
#[derive(Debug, Clone, Copy)]
pub struct UserPtr<T> {
    ptr: *const T,
}

impl<T> UserPtr<T> {
    /// Create a `UserPtr` from a raw pointer, validating the address is
    /// in user space and the page is present + user-accessible.
    pub fn new(ptr: *const T) -> SystemResult<Self> {
        if ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        let len = core::mem::size_of::<T>();
        validate_user_range(ptr as *const u8, len, false)?;
        Ok(Self { ptr })
    }

    /// Create a `UserPtr` from a raw mutable pointer, also validating
    /// the page is writable.
    pub fn new_mut(ptr: *mut T) -> SystemResult<Self> {
        if ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        let len = core::mem::size_of::<T>();
        validate_user_range(ptr as *const u8, len, true)?;
        Ok(Self { ptr })
    }

    /// Copy a value from user space into kernel-owned memory.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `T` is valid for the memory at the pointer.
    pub unsafe fn copy_from_user(&self) -> Result<T, SystemError> {
        unsafe { Ok(core::ptr::read_unaligned(self.ptr)) }
    }

    /// Copy a value into user space.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `T` is valid for the memory at the pointer
    /// and that the user buffer is writable.
    pub unsafe fn copy_to_user(&self, val: T) -> SystemResult<()> {
        unsafe {
            core::ptr::write_unaligned(self.ptr as *mut T, val);
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
/// `UserSlice` performs page-level validation at construction (via
/// `validate_user_range`) and only provides explicit copy-in/copy-out
/// operations, ensuring the kernel always owns its data copies.
#[derive(Debug, Clone, Copy)]
pub struct UserSlice {
    ptr: *mut u8,
    len: usize,
    writable: bool,
}

impl UserSlice {
    /// Validate a user-space buffer range and create a `UserSlice`.
    ///
    /// Checks:
    /// - Non-null pointer (when len > 0)
    /// - Entire range is in user space
    /// - Page-level validation: present, user-accessible, and (if writable) writable
    pub fn new(ptr: *mut u8, len: usize, writable: bool) -> SystemResult<Self> {
        if len == 0 {
            return Ok(Self {
                ptr,
                len: 0,
                writable,
            });
        }
        if ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        // Page-level validation
        validate_user_range(ptr as *const u8, len, writable)?;
        Ok(Self { ptr, len, writable })
    }

    /// Return the length of the slice.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return whether this slice was created for write access.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Copy data FROM user space INTO a kernel-owned buffer.
    ///
    /// # Safety
    ///
    /// Pages were validated at construction, so the copy is safe as long
    /// as no other thread unmaps them concurrently (which is the caller's
    /// responsibility — typically ensured by pinning the process).
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
    /// Pages were validated at construction, so the copy is safe as long
    /// as no other thread unmaps them concurrently.
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
    /// The caller must guarantee the pointer, length, and permissions are valid.
    pub unsafe fn from_raw_parts(ptr: *mut u8, len: usize, writable: bool) -> SystemResult<Self> {
        if len > 0 && ptr.is_null() {
            return Err(SystemError::InvalidArgument);
        }
        Ok(Self { ptr, len, writable })
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
    let start = VirtAddr::try_new(ptr as u64)
        .map_err(|_| SystemError::InvalidArgument)?;
    if !allow_kernel && !is_user_address(start) {
        return Err(SystemError::InvalidArgument);
    }
    if let Some(end_ptr) = ptr.checked_add(count - 1) {
        let end = VirtAddr::try_new(end_ptr as u64)
            .map_err(|_| SystemError::InvalidArgument)?;
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
    let slice = UserSlice::new(ptr as *mut u8, buf.len(), false)?;
    unsafe { slice.copy_from_user(buf)?; }
    Ok(buf.len().min(slice.len()))
}

/// Copy a byte slice from kernel memory into user space.
///
/// # Safety
///
/// The caller must ensure the user pointer is valid, mapped, and writable.
pub unsafe fn copy_to_user(ptr: *mut u8, buf: &[u8]) -> SystemResult<()> {
    let slice = UserSlice::new(ptr, buf.len(), true)?;
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
        if !allow_kernel {
            validate_user_range(ptr, count, false)?;
        } else {
            validate_user_buffer(ptr as usize, count, allow_kernel)?;
        }
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
        if !allow_kernel {
            validate_user_range(ptr, count, true)?;
        } else {
            validate_user_buffer(ptr as usize, count, allow_kernel)?;
        }
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
