#[cfg(not(target_arch = "aarch64"))]
use crate::page_table::allocator::BitmapFrameAllocator;
use core::cell::UnsafeCell;
#[cfg(not(target_arch = "aarch64"))]
use x86_64::VirtAddr;

pub const BOOT_CODE_PAGES: u64 = 16384;
pub const BOOT_CODE_START: u64 = 0x100000;

pub const TEMP_LOW_VA: u64 = 0x1000;
pub const VGA_MEMORY_START: u64 = 0xb8000;
/// Dedicated virtual address for VGA text buffer, placed just after
/// the 1 GB kernel direct-map area so that 4K mappings never split
/// a huge page (which causes triple faults on InsydeH2O bare metal).
pub const VGA_VIRT_ADDR: u64 = 0xFFFF_8000_4000_0000;
pub const VGA_MEMORY_END: u64 = 0xb8fa0;

pub const MAX_DESCRIPTOR_PAGES: u64 = 134_217_728; // 512 GiB / 4096
pub const MAX_SYSTEM_MEMORY: u64 = 512 * 1024 * 1024 * 1024u64;
pub const UEFI_COMPAT_PAGES: u64 = 16383;

#[cfg(not(target_arch = "aarch64"))]
pub const HIGHER_HALF_OFFSET: VirtAddr = VirtAddr::new(0xFFFF_8000_0000_0000);
#[cfg(not(target_arch = "aarch64"))]
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);
#[cfg(not(target_arch = "aarch64"))]
pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);

#[cfg(not(target_arch = "aarch64"))]
pub type BootInfoFrameAllocator = BitmapFrameAllocator;

/// aarch64 (the ANE desktop port): the registry holds a function-pointer
/// adapter because the ANE's physical frame allocator lives in the kernel
/// (the DSM memory-map allocator), not in petroleum's x86 bitmap. The API
/// mirrors the x86 path so the shared page_buf code is unchanged.
#[cfg(target_arch = "aarch64")]
pub struct AneFrameAllocator {
    allocate: fn(usize) -> Option<u64>,
    free: fn(u64, usize),
}

/// Error from the ANE frame allocator adapter (no contiguous run available).
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
pub struct AneAllocError;

#[cfg(target_arch = "aarch64")]
impl AneFrameAllocator {
    pub fn new(allocate: fn(usize) -> Option<u64>, free: fn(u64, usize)) -> Self {
        Self { allocate, free }
    }

    /// Allocate `pages` contiguous 4 KiB frames; returns the physical base.
    pub fn allocate_contiguous_frames(&mut self, pages: usize) -> Result<u64, AneAllocError> {
        (self.allocate)(pages).ok_or(AneAllocError)
    }

    pub fn free_contiguous_frames(&mut self, start: u64, pages: usize) {
        (self.free)(start, pages);
    }
}

#[cfg(target_arch = "aarch64")]
pub type BootInfoFrameAllocator = AneFrameAllocator;

// Global accessor for BootInfoFrameAllocator (deadlock-free, single-threaded kernel context)
struct SyncUnsafeCell<T> {
    inner: UnsafeCell<T>,
}

unsafe impl<T> Sync for SyncUnsafeCell<T> {}

static FRAME_ALLOCATOR: SyncUnsafeCell<Option<BootInfoFrameAllocator>> = SyncUnsafeCell {
    inner: UnsafeCell::new(None),
};
static FRAME_ALLOCATOR_ACCESS: spin::Mutex<()> = spin::Mutex::new(());

pub fn init_frame_allocator(allocator: BootInfoFrameAllocator) {
    unsafe {
        *FRAME_ALLOCATOR.inner.get() = Some(allocator);
    }
}

/// Run a closure with exclusive access to the frame allocator.
///
/// This is the preferred way to access the frame allocator, ensuring
/// only one mutable reference exists at a time.
pub fn with_frame_allocator<F, R>(f: F) -> R
where
    F: FnOnce(&mut BootInfoFrameAllocator) -> R,
{
    let _access = FRAME_ALLOCATOR_ACCESS.lock();
    unsafe {
        let allocator = (*FRAME_ALLOCATOR.inner.get())
            .as_mut()
            .expect("Frame allocator not initialized");
        f(allocator)
    }
}

/// Get mutable access to the frame allocator.
///
/// NOTE: This returns `&'static mut` which can lead to multiple mutable
/// references if called multiple times. Prefer `with_frame_allocator`
/// which provides a closure-based guard.
///
/// # Safety
///
/// The caller must ensure this is called only once at a time (single-
/// threaded boot phase or with external synchronization).
pub unsafe fn get_frame_allocator() -> &'static mut BootInfoFrameAllocator {
    unsafe {
        (*FRAME_ALLOCATOR.inner.get())
            .as_mut()
            .expect("Frame allocator not initialized")
    }
}

/// Deprecated alias for `get_frame_allocator`.
pub unsafe fn get_frame_allocator_mut() -> &'static mut BootInfoFrameAllocator {
    unsafe { get_frame_allocator() }
}
