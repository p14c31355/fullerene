use super::constants::{MAX_DESCRIPTOR_PAGES, MAX_SYSTEM_MEMORY};
use crate::common::EfiMemoryType;
use crate::debug_log_validate_macro;

// EFI Memory Descriptor as defined in UEFI spec
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub type_: crate::common::EfiMemoryType,
    pub padding: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

#[derive(Clone, Copy)]
pub struct MemoryMapDescriptor {
    ptr: *const u8,
    descriptor_size: usize,
}

impl MemoryMapDescriptor {
    pub fn new(ptr: *const u8, descriptor_size: usize) -> Self {
        Self {
            ptr,
            descriptor_size,
        }
    }

    pub fn type_(&self) -> u32 {
        unsafe { core::ptr::read_unaligned(self.ptr as *const u32) }
    }

    pub fn padding(&self) -> u32 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(4) as *const u32) }
    }

    pub fn physical_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(8) as *const u64) }
    }

    pub fn virtual_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(16) as *const u64) }
    }

    pub fn number_of_pages(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(24) as *const u64) }
    }

    pub fn attribute(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(self.descriptor_size - 8) as *const u64) }
    }
}

pub trait MemoryDescriptorValidator {
    fn is_valid(&self) -> bool;
    fn get_type(&self) -> u32;
    fn get_physical_start(&self) -> u64;
    fn get_page_count(&self) -> u64;
    fn is_memory_available(&self) -> bool;
}

impl MemoryDescriptorValidator for MemoryMapDescriptor {
    fn get_type(&self) -> u32 {
        self.type_()
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start()
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages()
    }

    fn is_valid(&self) -> bool {
        is_valid_memory_descriptor(self)
    }

    fn is_memory_available(&self) -> bool {
        let mem_type = self.get_type();
        matches!(mem_type, 4u32 | 7u32) || matches!(mem_type, 9u32 | 14u32)
    }
}

unsafe impl Send for MemoryMapDescriptor {}
unsafe impl Sync for MemoryMapDescriptor {}

impl MemoryDescriptorValidator for EfiMemoryDescriptor {
    fn get_type(&self) -> u32 {
        self.type_ as u32
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages
    }

    fn is_valid(&self) -> bool {
        let mem_type = self.get_type();
        let phys = self.get_physical_start();
        let pages = self.get_page_count();

        validate_descriptor_common(mem_type, phys, pages)
    }

    fn is_memory_available(&self) -> bool {
        let mem_type = self.get_type();
        matches!(mem_type, 4u32 | 7u32) || matches!(mem_type, 9u32 | 14u32)
    }
}

/// Private helper function to validate memory descriptor properties common to both descriptor types
fn validate_descriptor_common(mem_type: u32, phys: u64, pages: u64) -> bool {
    if mem_type > 15 {
        debug_log_no_alloc!("Invalid memory type (out of range): 0x", mem_type as usize);
        return false;
    }
    debug_log_validate_macro!("Memory type", mem_type as usize);

    if phys % 4096 != 0 {
        debug_log_no_alloc!("Unaligned physical_start: 0x", phys as usize);
        return false;
    }
    debug_log_validate_macro!("Physical start", phys as usize);

    if pages == 0 || pages > MAX_DESCRIPTOR_PAGES {
        debug_log_no_alloc!("Invalid page count: ", pages as usize);
        return false;
    }
    debug_log_validate_macro!("Page count", pages as usize);

    let page_size = 4096u64;
    let end_addr = match phys.checked_add(pages.saturating_mul(page_size)) {
        Some(end) if end > 0 => end,
        _ => {
            debug_log_no_alloc!("Overflow in address calculation");
            return false;
        }
    };

    if end_addr > MAX_SYSTEM_MEMORY {
        debug_log_no_alloc!("Memory region too large: end_addr=0x", end_addr as usize);
        return false;
    }
    debug_log_validate_macro!("End address", end_addr as usize);

    true
}

pub fn is_valid_memory_descriptor(descriptor: &MemoryMapDescriptor) -> bool {
    if descriptor.descriptor_size < 40 {
        debug_log_no_alloc!("Descriptor size too small: ", descriptor.descriptor_size);
        return false;
    }

    let mem_type = descriptor.get_type();
    let phys = descriptor.get_physical_start();
    let pages = descriptor.get_page_count();

    validate_descriptor_common(mem_type, phys, pages)
}

pub fn process_memory_descriptors<T, F>(descriptors: &[T], mut processor: F)
where
    T: MemoryDescriptorValidator,
    F: FnMut(&T, usize, usize),
{
    for descriptor in descriptors {
        if descriptor.is_valid() && descriptor.is_memory_available() {
            let start_frame = (descriptor.get_physical_start() / 4096) as usize;
            let end_frame = start_frame.saturating_add(descriptor.get_page_count() as usize);
            if start_frame < end_frame {
                processor(descriptor, start_frame, end_frame);
            }
        }
    }
}

pub fn mark_available_frames<T: MemoryDescriptorValidator>(
    frame_allocator: &mut crate::page_table::bitmap_allocator::BitmapFrameAllocator,
    memory_map: &[T],
) {
    process_memory_descriptors(memory_map, |_, start_frame, end_frame| {
        let actual_end = end_frame.min(frame_allocator.total_frames());
        frame_allocator.set_frame_range(start_frame, actual_end, false);
    });
    frame_allocator.set_frame_used(0);
}

pub fn calculate_frame_allocation_params<T: MemoryDescriptorValidator>(
    memory_map: &[T],
) -> (u64, usize, usize) {
    let mut max_addr: u64 = 0;

    for descriptor in memory_map {
        if descriptor.is_valid() {
            let end_addr = descriptor
                .get_physical_start()
                .saturating_add(descriptor.get_page_count().saturating_mul(4096));
            if end_addr > max_addr {
                max_addr = end_addr;
            }
        }
    }

    if max_addr == 0 {
        debug_log_no_alloc!("No valid descriptors found in memory map");
        return (0, 0, 0);
    }

    let capped_max_addr = max_addr.min(32 * 1024 * 1024 * 1024u64);
    let total_frames = (capped_max_addr.div_ceil(4096)) as usize;
    let bitmap_size = (total_frames + 63) / 64;
    (max_addr, total_frames, bitmap_size)
}

// debug_log_no_alloc imported from macros
use crate::page_table::bitmap_allocator::BitmapFrameAllocator;
