//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::uefi::{ConfigWithMetadata, FRAMEBUFFER_CONFIG_MAGIC};
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::memory_map::{EfiMemoryDescriptor, MemoryMapDescriptor};

use crate::MEMORY_MAP;

use alloc::vec::Vec;
use core::ffi::c_void;
use petroleum::{
    check_memory_initialized, debug_log, debug_log_no_alloc, debug_mem_descriptor, debug_print,
    mem_debug, write_serial_bytes,
};
use x86_64::{PhysAddr, VirtAddr};

// Add a constant for the higher-half kernel virtual base address
const HIGHER_HALF_KERNEL_VIRT_BASE: u64 = 0xFFFF_8000_0000_0000; // Common higher-half address

pub fn init_memory_management(
    memory_map: &'static [EfiMemoryDescriptor],
    physical_memory_offset: VirtAddr,
    kernel_phys_start: PhysAddr,
) {
    log::info!("Starting heap frame allocator init...");

    log::info!(
        "Calling heap::init_frame_allocator with {} descriptors",
        memory_map.len()
    );
    heap::init_frame_allocator(memory_map);
    log::info!("Heap frame allocator init completed successfully");

    log::info!("Page tables already initialized by bootloader, skipping reinit in kernel");
}
