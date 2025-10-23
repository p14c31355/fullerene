use super::constants::{
    EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC, MAX_DESCRIPTOR_PAGES, MAX_SYSTEM_MEMORY,
};
use crate::common::EfiMemoryType;

// EFI Memory Descriptor as defined in UEFI spec
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EfiMemoryDescriptor {
    pub type_: crate::common::EfiMemoryType,
    pub padding: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

impl core::fmt::Debug for EfiMemoryDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EfiMemoryDescriptor")
            .field("type_", &self.type_)
            .field("padding", &self.padding)
            .field("physical_start", &self.physical_start)
            .field("virtual_start", &self.virtual_start)
            .field("number_of_pages", &self.number_of_pages)
            .field("attribute", &self.attribute)
            .finish()
    }
}

// Generic validation trait for different descriptor types
pub trait MemoryDescriptorValidator {
    fn is_valid(&self) -> bool;
    fn get_physical_start(&self) -> u64;
    fn get_page_count(&self) -> u64;
    fn is_memory_available(&self) -> bool;
}

// Implementation for EFI memory descriptors
impl MemoryDescriptorValidator for EfiMemoryDescriptor {
    fn is_valid(&self) -> bool {
        is_valid_memory_descriptor(self)
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages
    }

    fn is_memory_available(&self) -> bool {
        use crate::common::EfiMemoryType;
        const EFI_ACPI_RECLAIM_MEMORY: u32 = 9; // Memory that holds ACPI tables that can be reclaimed after ACPI initialization
        const EFI_PERSISTENT_MEMORY: u32 = 14; // Memory that persists across reboot, typically NVDIMM-backed

        let mem_type = self.type_;
        matches!(
            mem_type,
            EfiMemoryType::EfiBootServicesData |     // 4
            EfiMemoryType::EfiConventionalMemory // 7
        ) || matches!(
            mem_type as u32,
            EFI_ACPI_RECLAIM_MEMORY | EFI_PERSISTENT_MEMORY
        )
    }
}

/// Validate an EFI memory descriptor for safety
pub fn is_valid_memory_descriptor(descriptor: &EfiMemoryDescriptor) -> bool {
    // Check memory type is within valid UEFI range (0x0-0x7FFFFFFF)
    // Allow OEM-specific memory types up to the UEFI maximum
    // But still be conservative about obviously garbage values
    let mem_type = descriptor.type_ as u32;
    if mem_type >= 0x80000000 {
        debug_log_no_alloc!("Invalid memory type (too high): ", mem_type);
        return false;
    }
    debug_log_validate_macro!("Memory type", mem_type);

    // Check physical start is page-aligned
    if descriptor.physical_start % 4096 != 0 {
        debug_log_no_alloc!(
            "Unaligned physical_start: 0x",
            descriptor.physical_start as usize
        );
        return false;
    }
    debug_log_validate_macro!("Physical start", descriptor.physical_start as usize);

    // Check number of pages is reasonable
    if descriptor.number_of_pages == 0 || descriptor.number_of_pages > MAX_DESCRIPTOR_PAGES {
        debug_log_no_alloc!("Invalid page count: ", descriptor.number_of_pages as usize);
        return false;
    }
    debug_log_validate_macro!("Page count", descriptor.number_of_pages as usize);

    // Check for potential overflow when calculating end address
    let page_size = 4096u64;
    if let Some(end_addr) = descriptor.physical_start.checked_add(
        descriptor
            .number_of_pages
            .checked_mul(page_size)
            .unwrap_or(u64::MAX),
    ) {
        // Ensure end address doesn't exceed reasonable system limits (512GB)
        if end_addr > MAX_SYSTEM_MEMORY {
            debug_log_no_alloc!("Memory region too large: end_addr=0x", end_addr as usize);
            return false;
        }
        debug_log_validate_macro!("End address", end_addr as usize);
    } else {
        debug_log_no_alloc!("Overflow in address calculation");
        return false;
    }

    true
}

// Generic function to process memory descriptors using traits with integrated frame calculation
pub fn process_memory_descriptors<T, F>(descriptors: &[T], mut processor: F)
where
    T: MemoryDescriptorValidator,
    F: FnMut(&T, usize, usize), // (descriptor, start_frame, end_frame)
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

// Mark available frames as free based on memory map
pub fn mark_available_frames(
    frame_allocator: &mut crate::page_table::bitmap_allocator::BitmapFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
) {
    process_memory_descriptors_safely!(memory_map, |descriptor: &EfiMemoryDescriptor, start_frame: usize, end_frame: usize| {
        let actual_end = end_frame.min(frame_allocator.frame_count);
        frame_allocator.set_frame_range(start_frame, actual_end, false);
    });

    // Mark frame 0 as used to avoid allocating the null page
    frame_allocator.set_frame_used(0);
}

// Calculate frame allocation parameters from memory map
pub fn calculate_frame_allocation_params(memory_map: &[EfiMemoryDescriptor]) -> (u64, usize, usize) {
    // Only consider valid descriptors to prevent corrupted data from causing excessive bitmap allocation
    let mut max_addr: u64 = 0;

    for descriptor in memory_map {
        if is_valid_memory_descriptor(descriptor) {
            let end_addr = descriptor
                .physical_start
                .saturating_add(descriptor.number_of_pages.saturating_mul(4096));
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

use crate::debug_log_no_alloc;
use crate::page_table::bitmap_allocator::BitmapFrameAllocator;
