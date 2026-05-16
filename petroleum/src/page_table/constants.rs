use crate::page_table::allocator::BitmapFrameAllocator;
use core::cell::UnsafeCell;
use x86_64::VirtAddr;

pub const BOOT_CODE_PAGES: u64 = 16384;
pub const BOOT_CODE_START: u64 = 0x100000;

pub const TEMP_LOW_VA: u64 = 0x1000;
pub const VGA_MEMORY_START: u64 = 0xb8000;
pub const VGA_MEMORY_END: u64 = 0xb8fa0;

pub const MAX_DESCRIPTOR_PAGES: u64 = 134_217_728; // 512 GiB / 4096
pub const MAX_SYSTEM_MEMORY: u64 = 512 * 1024 * 1024 * 1024u64;
pub const UEFI_COMPAT_PAGES: u64 = 16383;

pub const HIGHER_HALF_OFFSET: VirtAddr = VirtAddr::new(0xFFFF_8000_0000_0000);
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);
pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);

pub const MAX_BITMAP_CAPACITY: usize = 4096;
pub const MAX_PCI_DEVICES: usize = 16;
pub const MAX_SERIAL_BUF: usize = 256;
pub const MAX_USER_SPACE_SIZE: usize = 4096;
pub type BootInfoFrameAllocator = BitmapFrameAllocator;

// Global accessor for BootInfoFrameAllocator (deadlock-free, single-threaded kernel context)
struct SyncUnsafeCell<T> {
    inner: UnsafeCell<T>,
}

unsafe impl<T> Sync for SyncUnsafeCell<T> {}

static FRAME_ALLOCATOR: SyncUnsafeCell<Option<BootInfoFrameAllocator>> = SyncUnsafeCell {
    inner: UnsafeCell::new(None),
};

pub fn init_frame_allocator(allocator: BootInfoFrameAllocator) {
    unsafe {
        *FRAME_ALLOCATOR.inner.get() = Some(allocator);
    }
}

pub fn get_frame_allocator() -> &'static mut BootInfoFrameAllocator {
    unsafe {
        (*FRAME_ALLOCATOR.inner.get())
            .as_mut()
            .expect("Frame allocator not initialized")
    }
}

pub fn get_frame_allocator_mut() -> &'static mut BootInfoFrameAllocator {
    get_frame_allocator()
}
