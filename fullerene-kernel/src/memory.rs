//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::{EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig};
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
) -> Option<usize>
where
    F: Fn(&EfiMemoryDescriptor) -> bool,
{
    descriptors
        .iter()
        .find(|desc| predicate(desc))
        .map(|desc| desc.physical_start as usize)
}

// Helper function to find framebuffer config (using generic)
pub fn find_framebuffer_config(system_table: &EfiSystemTable) -> Option<&FullereneFramebufferConfig> {
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
            i,
            entry.vendor_table as usize
        ));

        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

// Helper function to find heap start from memory map (using generic)
pub fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> PhysAddr {
    // Find the largest EfiConventionalMemory descriptor and use its physical start for heap
    let mut largest_addr = None;
    let mut largest_pages = 0u64;
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiConventionalMemory && desc.number_of_pages >= 4 { // at least 16KB
            if desc.number_of_pages > largest_pages {
                largest_pages = desc.number_of_pages;
                largest_addr = Some(desc.physical_start);
            }
        }
    }
    if let Some(addr) = largest_addr {
        PhysAddr::new(addr)
    } else {
        // Fallback if no conventional memory found
        PhysAddr::new(0x100000)
    }
}

pub fn setup_memory_maps(
    memory_map: *mut c_void,
    memory_map_size: usize,
    kernel_virt_addr: u64,
) -> (VirtAddr, PhysAddr) {
    // Use the passed memory map
    petroleum::serial::debug_print_str_to_com1("About to create memory map slice\n");
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            memory_map as *const EfiMemoryDescriptor,
            memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    petroleum::serial::debug_print_str_to_com1("Memory map slice created\n");
    kernel_log!(
        "Memory map slice size: {}, descriptor count: {}",
        memory_map_size,
        descriptors.len()
    );
    // Reduce log verbosity for faster boot
    if descriptors.len() < 20 {
        for (i, desc) in descriptors.iter().enumerate() {
            kernel_log!(
                "Memory descriptor {}: type={:#x}, phys_start=0x{:x}, virt_start=0x{:x}, pages=0x{:x}",
                i,
                desc.type_ as u32,
                desc.physical_start,
                desc.virtual_start,
                desc.number_of_pages
            );
        }
    }
    kernel_log!("Memory map parsing: finished descriptor dump");
    // Initialize MEMORY_MAP with descriptors
    MEMORY_MAP.call_once(|| {
        // Since UEFI memory map is static until exit_boot_services, this is safe
        unsafe { &*(descriptors as *const _) }
    });
    kernel_log!("MEMORY_MAP initialized");

    let mut physical_memory_offset = VirtAddr::new(0);
    let mut kernel_phys_start = PhysAddr::new(0);

    kernel_log!("Scanning memory descriptors to find kernel location...");

    // The kernel is loaded at 0x100000 physical address as determined from PE loading logs
    // efi_main at 0x1713e0 indicates virtual address, physical base is 0x100000
    kernel_phys_start = PhysAddr::new(0x100000);

    kernel_log!("Using fixed kernel physical start: 0x{:x}", kernel_phys_start.as_u64());

// Calculate the physical_memory_offset for the higher-half kernel mapping.
// This offset is such that physical_address + offset = higher_half_virtual_address.
physical_memory_offset = VirtAddr::new(HIGHER_HALF_KERNEL_VIRT_BASE - kernel_phys_start.as_u64());

// Store the higher-half offset for heap allocation
crate::heap::HIGHER_HALF_OFFSET.call_once(|| physical_memory_offset);

    kernel_log!(
        "Physical memory offset calculation complete: offset=0x{:x}, kernel_phys_start=0x{:x}",
        physical_memory_offset.as_u64(),
        kernel_phys_start.as_u64()
    );

    (physical_memory_offset, kernel_phys_start)
}

pub fn init_memory_management(
    memory_map: &'static [EfiMemoryDescriptor],
    physical_memory_offset: VirtAddr,
    kernel_phys_start: PhysAddr,
) {
    kernel_log!("Starting heap frame allocator init...");

    kernel_log!(
        "Calling heap::init_frame_allocator with {} descriptors",
        memory_map.len()
    );
    heap::init_frame_allocator(memory_map);
    kernel_log!("Heap frame allocator init completed successfully");

    kernel_log!(
        "Calling heap::init_page_table with offset 0x{:x}",
        physical_memory_offset.as_u64()
    );
    heap::init_page_table(physical_memory_offset);
    kernel_log!("Page table init completed successfully");

    kernel_log!(
        "Calling heap::reinit_page_table with offset 0x{:x} and kernel_phys_start 0x{:x}",
        physical_memory_offset.as_u64(),
        kernel_phys_start.as_u64()
    );
    heap::reinit_page_table(physical_memory_offset, kernel_phys_start, None);
    kernel_log!("Page table reinit completed successfully");
}
