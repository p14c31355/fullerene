//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::memory_map::EfiMemoryDescriptor;
use x86_64::{PhysAddr, VirtAddr};

pub fn init_memory_management(
    memory_map: &'static [EfiMemoryDescriptor],
    _physical_memory_offset: VirtAddr,
    _kernel_phys_start: PhysAddr,
) {
    heap::init_frame_allocator(memory_map);
}
