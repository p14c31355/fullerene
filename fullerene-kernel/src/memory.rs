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

// Add a constant for the higher-half kernel virtual base address
const HIGHER_HALF_KERNEL_VIRT_BASE: u64 = 0xFFFF_8000_0000_0000; // Common higher-half address

// Macro to reduce repetitive serial logging - local copy since we moved function here
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;

macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

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
    petroleum::serial::serial_log(format_args!(
        "find_framebuffer_config: System table has {} configuration table entries\n",
        system_table.number_of_table_entries
    ));

    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };

    for (i, entry) in config_table_entries.iter().enumerate() {
        petroleum::serial::serial_log(format_args!(
            "Config table {}: table={:#x}, checking for GOP GUID\n",
            i, entry.vendor_table as usize
        ));

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
            && desc.physical_start < 0x100000000 // below 4GB
        {
            return PhysAddr::new(desc.physical_start);
        }
    }
    // Fallback if no suitable memory found
    PhysAddr::new(crate::boot::FALLBACK_HEAP_START_ADDR)
}

pub fn setup_memory_maps(
    memory_map: *mut c_void,
    memory_map_size: usize,
    kernel_virt_addr: u64,
) -> PhysAddr {
    // Use the passed memory map
    petroleum::serial::debug_print_str_to_com1("About to create memory map slice\n");
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            memory_map as *const EfiMemoryDescriptor,
            memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    petroleum::serial::debug_print_str_to_com1("Memory map slice created\n");
    log::info!(
        "Memory map slice size: {}, descriptor count: {}",
        memory_map_size,
        descriptors.len()
    );
    // Reduce log verbosity for faster boot
    if descriptors.len() < 20 {
        for (i, desc) in descriptors.iter().enumerate() {
            log::info!(
                "Memory descriptor {}: type={:#x}, phys_start=0x{:x}, virt_start=0x{:x}, pages=0x{:x}",
                i,
                desc.type_ as u32,
                desc.physical_start,
                desc.virtual_start,
                desc.number_of_pages
            );
        }
    }
    log::info!("Memory map parsing: finished descriptor dump");
    // Initialize MEMORY_MAP with descriptors
    MEMORY_MAP.call_once(|| {
        // Since UEFI memory map is static until exit_boot_services, this is safe
        unsafe { &*(descriptors as *const _) }
    });
    log::info!("MEMORY_MAP initialized");

    let physical_memory_offset;
    let kernel_phys_start;

    log::info!("Scanning memory descriptors to find kernel location...");

    // Find the memory descriptor containing the kernel (efi_main is virtual address,
    // but UEFI uses identity mapping initially, so check physical range containing kernel_virt_addr)
    // Since UEFI identity-maps initially, kernel_virt_addr should equal its physical address
    if kernel_virt_addr >= 0x1000 {
        kernel_phys_start = PhysAddr::new(kernel_virt_addr);
        log::info!(
            "Using identity-mapped kernel physical start: 0x{:x}",
            kernel_phys_start.as_u64()
        );
    } else {
        log::info!(
            "Warning: Invalid kernel address 0x{:x}, falling back to hardcoded value",
            kernel_virt_addr
        );
        kernel_phys_start = PhysAddr::new(0x100000);
    }

    // Calculate the physical_memory_offset for the higher-half kernel mapping.
    // This offset is such that physical_address + offset = higher_half_virtual_address.
    // Use a simpler offset that maps physical addresses to the higher half directly
    physical_memory_offset = VirtAddr::new(HIGHER_HALF_KERNEL_VIRT_BASE);

    log::info!(
        "Physical memory offset calculation complete: offset=0x{:x}, kernel_phys_start=0x{:x}",
        physical_memory_offset.as_u64(),
        kernel_phys_start.as_u64()
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

    log::info!(
        "Calling heap::init_page_table with offset 0x{:x}",
        physical_memory_offset.as_u64()
    );
    unsafe { petroleum::page_table::init(physical_memory_offset) };
    log::info!("Page table init completed successfully");

    log::info!(
        "Calling heap::reinit_page_table with offset 0x{:x} and kernel_phys_start 0x{:x}",
        physical_memory_offset.as_u64(),
        kernel_phys_start.as_u64()
    );
    heap::reinit_page_table(kernel_phys_start, None, None);
    log::info!("Page table reinit completed successfully");
}
