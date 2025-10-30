//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::uefi::{ConfigWithMetadata, FRAMEBUFFER_CONFIG_MAGIC};
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::efi_memory::{EfiMemoryDescriptor, MemoryDescriptorValidator, MemoryMapDescriptor};

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



// Helper function to find framebuffer config (using generic)
pub fn find_framebuffer_config(
    system_table: &EfiSystemTable,
) -> Option<&FullereneFramebufferConfig> {
    log::info!(
        "find_framebuffer_config: called with system_table=0x{:x}",
        system_table as *const _ as usize
    );
    log::info!(
        "find_framebuffer_config: System table has {} configuration table entries",
        system_table.number_of_table_entries
    );

    // Check for null pointer after UEFI boot services exit
    if system_table.configuration_table.is_null() {
        log::info!(
            "find_framebuffer_config: Configuration table is null (UEFI boot services exited)"
        );
        return None;
    }

    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };

    for (i, entry) in config_table_entries.iter().enumerate() {
        log::info!(
            "Config table {}: table={:#x}, checking for GOP GUID",
            i,
            entry.vendor_table as usize
        );

        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

pub fn find_heap_start(descriptors: &[impl MemoryDescriptorValidator]) -> PhysAddr {
    // Find the lowest suitable memory region within first 64MB from EfiConventionalMemory with sufficient size for heap
    // This ensures heap is within the identity-mapped range during page table reinitialization
    const HEAP_PAGES: u64 = 256; // approx 1MB for heap + structures
    for desc in descriptors {
        if desc.get_type() == EfiMemoryType::EfiConventionalMemory as u32
            && desc.get_page_count() >= HEAP_PAGES
            && desc.get_physical_start() < 0x4000000 // within first 64MB
            && desc.get_physical_start() + (desc.get_page_count() * 4096) <= 0x4000000
        // ensure entire region fits
        {
            return PhysAddr::new(desc.get_physical_start());
        }
    }
    // Fallback if no suitable memory found within first 64MB
    PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
}

pub fn setup_kernel_location(
    memory_map: *mut c_void,
    memory_map_size: usize,
    kernel_virt_addr: u64,
) -> PhysAddr {
    // Read descriptor_size from the beginning of the memory map
    debug_log_no_alloc!("setup_kernel_location called with size: ", memory_map_size);
    let _descriptor_item_size = unsafe { *(memory_map as *const usize) };
    debug_log_no_alloc!("Descriptor size: ", _descriptor_item_size);

    let config_size = core::mem::size_of::<ConfigWithMetadata>();
    // Check for framebuffer config appended to memory map
    let config_with_metadata_ptr = unsafe {
        (memory_map as *const u8).add(memory_map_size - config_size) as *const ConfigWithMetadata
    };
    let config_with_metadata = unsafe { &*config_with_metadata_ptr };
    let has_config = config_with_metadata.magic == FRAMEBUFFER_CONFIG_MAGIC;

    if config_with_metadata.magic == FRAMEBUFFER_CONFIG_MAGIC {
        debug_log_no_alloc!("Framebuffer config found in memory map");
        petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .call_once(|| spin::Mutex::new(Some(config_with_metadata.config)));
    } else {
        debug_log_no_alloc!("No framebuffer config found in memory map (magic mismatch)");
    }

    // Find the kernel physical start (efi_main is virtual address,
    // but UEFI uses identity mapping initially, so check physical range containing kernel_virt_addr)
    // Since UEFI identity-maps initially, kernel_virt_addr should equal its physical address
    let kernel_phys_start = if kernel_virt_addr >= 0x1000 {
        PhysAddr::new(kernel_virt_addr)
    } else {
        debug_log_no_alloc!("Warning: Invalid kernel address, falling back");
        PhysAddr::new(0x100000)
    };

    mem_debug!(
        "Kernel physical start set to: ",
        kernel_phys_start.as_u64() as usize,
        "\n"
    );

    kernel_phys_start
}

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
