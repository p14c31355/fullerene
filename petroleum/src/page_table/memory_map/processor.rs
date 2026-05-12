use crate::page_table::allocator::{BitmapFrameAllocator, FrameAllocatorExt};
use crate::page_table::constants::MAX_DESCRIPTOR_PAGES;
use crate::page_table::types::MemoryDescriptorValidator;

pub fn process_memory_descriptors<T, F>(descriptors: &[T], mut processor: F)
where
    T: MemoryDescriptorValidator,
    F: FnMut(&T, usize, usize),
{
    process_valid_descriptors(descriptors, |desc, start_frame, end_frame| {
        if desc.is_memory_available() {
            processor(desc, start_frame, end_frame);
        }
    });
}

pub fn process_valid_descriptors<T, F>(descriptors: &[T], mut processor: F)
where
    T: MemoryDescriptorValidator,
    F: FnMut(&T, usize, usize),
{
    for descriptor in descriptors.iter() {
        // Skip descriptors with excessively large page counts to avoid overflow or invalid entries
        if descriptor.get_page_count() > MAX_DESCRIPTOR_PAGES {
            crate::debug_log_no_alloc!(
                "Skipping descriptor with excessive page count: ",
                descriptor.get_page_count() as usize
            );
            continue;
        }
        if descriptor.is_valid() {
            let start_frame = (descriptor.get_physical_start() / 4096) as usize;
            let end_frame = start_frame.saturating_add(descriptor.get_page_count() as usize);
            if start_frame < end_frame {
                processor(descriptor, start_frame, end_frame);
            }
        }
    }
}

pub fn mark_available_frames<T: MemoryDescriptorValidator>(
    frame_allocator: &mut BitmapFrameAllocator,
    memory_map: &[T],
) {
    process_memory_descriptors(memory_map, |_, start_frame, end_frame| {
        let actual_end = end_frame.min(frame_allocator.total_frames());
        frame_allocator.set_frame_range(start_frame, actual_end, false);
    });
    frame_allocator.set_frame_used(0, true);
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
        crate::debug_log_no_alloc!("No valid descriptors found in memory map");
        return (0, 0, 0);
    }

    let capped_max_addr = max_addr.min(32 * 1024 * 1024 * 1024u64);
    let total_frames = (capped_max_addr.div_ceil(4096)) as usize;
    let bitmap_size = (total_frames + 63) / 64;
    (max_addr, total_frames, bitmap_size)
}

pub fn for_each_memory_descriptor<T: MemoryDescriptorValidator, F>(
    memory_map: &[T],
    types: &[crate::common::EfiMemoryType],
    mut f: F,
) where
    F: FnMut(&T),
{
    for desc in memory_map {
        if types.iter().any(|&t| desc.get_type() == t as u32) && desc.get_page_count() > 0 {
            f(desc);
        }
    }
}
