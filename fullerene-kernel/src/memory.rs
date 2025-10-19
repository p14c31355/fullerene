//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;

use crate::MEMORY_MAP;

use core::ffi::c_void;
use x86_64::{PhysAddr, VirtAddr};
use petroleum::write_serial_bytes;

// Add a constant for the higher-half kernel virtual base address
const HIGHER_HALF_KERNEL_VIRT_BASE: u64 = 0xFFFF_8000_0000_0000; // Common higher-half address

// Generic helper for searching memory descriptors
fn find_memory_descriptor_address<F>(
    descriptors: &[EfiMemoryDescriptor],
    predicate: F,
) -> Option<u64>
where
    F: Fn(&EfiMemoryDescriptor) -> bool,
{
    descriptors
        .iter()
        .find(|desc| predicate(desc))
        .map(|desc| desc.physical_start)
}

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

pub fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> PhysAddr {
    // Find the lowest suitable memory region below 4GB from EfiConventionalMemory with sufficient size for heap
    const HEAP_PAGES: u64 = 256; // approx 1MB for heap + structures
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiConventionalMemory
            && desc.number_of_pages >= HEAP_PAGES
            && desc.physical_start < 0x100000000
        // below 4GB
        {
            return PhysAddr::new(desc.physical_start);
        }
    }
    // Fallback if no suitable memory found
    PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
}

pub fn setup_memory_maps(
    memory_map: *mut c_void,
    memory_map_size: usize,
    kernel_virt_addr: u64,
) -> PhysAddr {
    // Use the passed memory map
    write_serial_bytes!(0x3F8, 0x3FD, b"About to create memory map slice\n");
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            memory_map as *const EfiMemoryDescriptor,
            memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    write_serial_bytes!(0x3F8, 0x3FD, b"Memory map slice created\n");
    petroleum::serial::debug_print_str_to_com1("Memory map slice size: ");
    petroleum::serial::debug_print_hex(memory_map_size);
    petroleum::serial::debug_print_str_to_com1(", descriptor count: ");
    petroleum::serial::debug_print_hex(descriptors.len());
    petroleum::serial::debug_print_str_to_com1("\n");
    // Reduce log verbosity for faster boot
    if descriptors.len() < 20 {
        for (i, desc) in descriptors.iter().enumerate() {
            petroleum::serial::debug_print_str_to_com1("Memory descriptor ");
            petroleum::serial::debug_print_hex(i);
            petroleum::serial::debug_print_str_to_com1(": type=");
            petroleum::serial::debug_print_hex(desc.type_ as usize);
            petroleum::serial::debug_print_str_to_com1(", phys_start=");
            petroleum::serial::debug_print_hex(desc.physical_start as usize);
            petroleum::serial::debug_print_str_to_com1(", virt_start=");
            petroleum::serial::debug_print_hex(desc.virtual_start as usize);
            petroleum::serial::debug_print_str_to_com1(", pages=");
            petroleum::serial::debug_print_hex(desc.number_of_pages as usize);
            petroleum::serial::debug_print_str_to_com1("\n");
        }
    }
    write_serial_bytes!(0x3F8, 0x3FD, b"Memory map parsing: finished descriptor dump\n");
    // Initialize MEMORY_MAP with descriptors
    MEMORY_MAP.call_once(|| {
        // Since UEFI memory map is static until exit_boot_services, this is safe
        unsafe { &*(descriptors as *const _) }
    });
    write_serial_bytes!(0x3F8, 0x3FD, b"MEMORY_MAP initialized\n");

    let physical_memory_offset;
    let kernel_phys_start;

    write_serial_bytes!(0x3F8, 0x3FD, b"Scanning memory descriptors to find kernel location...\n");

    // Find the memory descriptor containing the kernel (efi_main is virtual address,
    // but UEFI uses identity mapping initially, so check physical range containing kernel_virt_addr)
    // Since UEFI identity-maps initially, kernel_virt_addr should equal its physical address
    if kernel_virt_addr >= 0x1000 {
        kernel_phys_start = PhysAddr::new(kernel_virt_addr);
        petroleum::serial::debug_print_str_to_com1("Using identity-mapped kernel physical start: ");
        petroleum::serial::debug_print_hex(kernel_phys_start.as_u64() as usize);
        petroleum::serial::debug_print_str_to_com1("\n");
    } else {
        petroleum::serial::debug_print_str_to_com1("Warning: Invalid kernel address ");
        petroleum::serial::debug_print_hex(kernel_virt_addr as usize);
        petroleum::serial::debug_print_str_to_com1(", falling back to hardcoded value\n");
        kernel_phys_start = PhysAddr::new(0x100000);
    }

    // Calculate the physical_memory_offset for the higher-half kernel mapping.
    // This offset is such that physical_address + offset = higher_half_virtual_address.
    // Use a simpler offset that maps physical addresses to the higher half directly
    physical_memory_offset = VirtAddr::new(HIGHER_HALF_KERNEL_VIRT_BASE);

    petroleum::serial::debug_print_str_to_com1("Physical memory offset calculation complete: offset=");
    petroleum::serial::debug_print_hex(physical_memory_offset.as_u64() as usize);
    petroleum::serial::debug_print_str_to_com1(", kernel_phys_start=");
    petroleum::serial::debug_print_hex(kernel_phys_start.as_u64() as usize);
    petroleum::serial::debug_print_str_to_com1("\n");

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
