//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::{EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig};
use petroleum::page_table::EfiMemoryDescriptor;

use crate::MEMORY_MAP;

use core::ffi::c_void;
use x86_64::{PhysAddr, VirtAddr};

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
    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };
    for entry in config_table_entries {
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

// Helper function to find heap start from memory map (using generic)
pub fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> PhysAddr {
    // First, try to find EfiLoaderData
    if let Some(addr) = find_memory_descriptor_address(descriptors, |desc| {
        desc.type_ == EfiMemoryType::EfiLoaderData && desc.number_of_pages > 0
    }) {
        return PhysAddr::new(addr as u64);
    }
    // If not found, find EfiConventionalMemory large enough
    let required_pages = (heap::HEAP_SIZE + 4095) / 4096;
    if let Some(addr) = find_memory_descriptor_address(descriptors, |desc| {
        desc.type_ == EfiMemoryType::EfiConventionalMemory
            && desc.number_of_pages >= required_pages as u64
    }) {
        return PhysAddr::new(addr as u64);
    }
    panic!("No suitable memory region found for heap");
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

    // Calculate physical_memory_offset from kernel's location in memory map
    kernel_log!("Starting to calculate physical_memory_offset...");
    let mut physical_memory_offset = VirtAddr::new(0);
    let mut kernel_phys_start = PhysAddr::new(0);
    kernel_log!("Kernel virtual address: 0x{:x}", kernel_virt_addr);

    // Find physical_memory_offset and kernel_phys_start
    kernel_log!("Scanning memory descriptors to find kernel location...");
    let mut found_in_descriptor = false;
    let memory_map_ref = *MEMORY_MAP.get().unwrap(); // Deref the && to &
    for (i, desc) in memory_map_ref.iter().enumerate() {
        let virt_start = desc.virtual_start;
        let virt_end = virt_start + desc.number_of_pages * 4096;
        if kernel_virt_addr >= virt_start && kernel_virt_addr < virt_end {
            physical_memory_offset = VirtAddr::new(desc.virtual_start - desc.physical_start);
            found_in_descriptor = true;
            kernel_log!(
                "Found kernel in descriptor {}: phys_offset=0x{:x}",
                i,
                physical_memory_offset.as_u64()
            );
            if desc.type_ == EfiMemoryType::EfiLoaderCode {
                kernel_phys_start = PhysAddr::new(desc.physical_start);
                kernel_log!(
                    "Kernel is in EfiLoaderCode, phys_start=0x{:x}",
                    kernel_phys_start.as_u64()
                );
            }
            break; // Found it, no need to continue scanning
        }
    }

    if !found_in_descriptor {
        kernel_log!("WARNING: Kernel virtual address not found in any descriptor!");
    }

    if kernel_phys_start.is_null() {
        kernel_log!("Could not determine kernel's physical start address, setting to 0");
        // Try to find any suitable memory for kernel
        for desc in *MEMORY_MAP.get().unwrap() {
            if desc.type_ == EfiMemoryType::EfiLoaderCode && desc.number_of_pages > 0 {
                kernel_phys_start = PhysAddr::new(desc.physical_start);
                kernel_log!(
                    "Using first EfiLoaderCode descriptor: phys_start=0x{:x}",
                    kernel_phys_start.as_u64()
                );
                break;
            }
        }
        if kernel_phys_start.is_null() {
            panic!("Could not determine kernel's physical start address from any EfiLoaderCode descriptor.");
    }

    // Assume identity mapping for now
    if !found_in_descriptor {
        physical_memory_offset = VirtAddr::new(0);
        kernel_log!(
            "WARNING: Kernel virtual address not found, assuming identity mapping (offset=0)"
        );
    }

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
    heap::reinit_page_table(physical_memory_offset, kernel_phys_start);
    kernel_log!("Page table reinit completed successfully");
}
