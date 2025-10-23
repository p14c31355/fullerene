use x86_64::{VirtAddr, structures::paging::PageTableFlags};

// Consolidated constants to reduce pollution and duplication

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_DESCRIPTOR_PAGES: u64 = 1_048_576;
pub const MAX_SYSTEM_MEMORY: u64 = 512 * 1024 * 1024 * 1024u64;
pub const EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC: u32 = 15;
pub const UEFI_COMPAT_PAGES: u64 = 16383;
pub const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024;
pub const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;
pub const VGA_MEMORY_START: u64 = 0xA0000u64;
pub const VGA_MEMORY_END: u64 = 0xC0000u64;
pub const BOOT_CODE_START: u64 = 0x100000u64;
pub const BOOT_CODE_PAGES: u64 = 0x8000u64;

// Page table flags constants
pub static READ_WRITE_NO_EXEC: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::NO_EXECUTE);
pub static READ_ONLY: PageTableFlags = PageTableFlags::PRESENT;
pub static READ_WRITE: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);
pub static READ_EXECUTE: PageTableFlags = PageTableFlags::PRESENT;

// Page table offsets
pub const HIGHER_HALF_OFFSET: VirtAddr = VirtAddr::new(0xFFFF_8000_0000_0000);
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);
pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);
pub const TEMP_VA_FOR_ZERO: VirtAddr = VirtAddr::new(0xFFFF_B000_0000_0000);
pub const TEMP_LOW_VA: VirtAddr = VirtAddr::new(0x1000u64);
