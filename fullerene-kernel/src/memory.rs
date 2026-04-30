//! Memory management module containing memory map parsing and initialization

use crate::heap;
use petroleum::common::uefi::{ConfigWithMetadata, FRAMEBUFFER_CONFIG_MAGIC};
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::efi_memory::{
    EfiMemoryDescriptor, MemoryDescriptorValidator, MemoryMapDescriptor,
};

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
    petroleum::uefi_helpers::find_framebuffer_config(system_table)
}

pub fn find_heap_start(descriptors: &[impl MemoryDescriptorValidator]) -> PhysAddr {
    petroleum::uefi_helpers::find_heap_start(descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petroleum::page_table::efi_memory::EfiMemoryDescriptor;
    use petroleum::common::EfiMemoryType;
    use x86_64::PhysAddr;

    struct MockDescriptor {
        type_: u32,
        start: u64,
        pages: u64,
    }

    impl petroleum::page_table::efi_memory::MemoryDescriptorValidator for MockDescriptor {
        fn is_valid(&self) -> bool { true }
        fn get_type(&self) -> u32 { self.type_ }
        fn get_physical_start(&self) -> u64 { self.start }
        fn get_page_count(&self) -> u64 { self.pages }
        fn is_memory_available(&self) -> bool { true }
    }

    #[test]
    fn test_find_heap_start_valid() {
        let descriptors = [
            MockDescriptor { type_: EfiMemoryType::EfiConventionalMemory as u32, start: 0x1000, pages: 512 },
        ];
        assert_eq!(find_heap_start(&descriptors), PhysAddr::new(0x1000));
    }

    #[test]
    fn test_find_heap_start_too_small() {
        let descriptors = [
            MockDescriptor { type_: EfiMemoryType::EfiConventionalMemory as u32, start: 0x1000, pages: 128 },
        ];
        assert_eq!(find_heap_start(&descriptors), PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR));
    }

    #[test]
    fn test_find_heap_start_out_of_range() {
        let descriptors = [
            MockDescriptor { type_: EfiMemoryType::EfiConventionalMemory as u32, start: 0x5000000, pages: 512 },
        ];
        assert_eq!(find_heap_start(&descriptors), PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR));
    }

    #[test]
    fn test_find_heap_start_wrong_type() {
        let descriptors = [
            MockDescriptor { type_: EfiMemoryType::EfiReservedMemoryType as u32, start: 0x1000, pages: 512 },
        ];
        assert_eq!(find_heap_start(&descriptors), PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR));
    }

    #[test]
    fn test_find_heap_start_multiple_pick_first() {
        let descriptors = [
            MockDescriptor { type_: EfiMemoryType::EfiConventionalMemory as u32, start: 0x1000, pages: 512 },
            MockDescriptor { type_: EfiMemoryType::EfiConventionalMemory as u32, start: 0x2000, pages: 512 },
        ];
        assert_eq!(find_heap_start(&descriptors), PhysAddr::new(0x1000));
    }
}

pub fn setup_kernel_location(
    memory_map: *mut c_void,
    memory_map_size: usize,
    kernel_virt_addr: u64,
) -> PhysAddr {
    petroleum::uefi_helpers::setup_kernel_location(memory_map, memory_map_size, kernel_virt_addr)
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
