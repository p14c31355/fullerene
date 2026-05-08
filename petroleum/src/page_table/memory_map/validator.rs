use crate::page_table::constants::{MAX_DESCRIPTOR_PAGES, MAX_SYSTEM_MEMORY};
use crate::page_table::types::MemoryDescriptorValidator;
use crate::page_table::memory_map::descriptor::{EfiMemoryDescriptor, MemoryMapDescriptor};

impl MemoryDescriptorValidator for MemoryMapDescriptor {
    fn get_type(&self) -> u32 {
        self.type_()
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start()
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages()
    }

    fn is_valid(&self) -> bool {
        is_valid_memory_descriptor(self)
    }

    fn is_memory_available(&self) -> bool {
        let mem_type = self.get_type();
        // Available memory types according to UEFI spec:
        // 1: Loader Code
        // 2: Loader Data
        // 3: Boot Services Code
        // 4: Boot Services Data
        // 7: Conventional Memory
        // 9: ACPI Reclaimed Memory
        matches!(mem_type, 7u32 | 3u32 | 4u32 | 1u32 | 2u32 | 9u32)
    }
}

impl MemoryDescriptorValidator for EfiMemoryDescriptor {
    fn get_type(&self) -> u32 {
        self.type_ as u32
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages
    }

    fn is_valid(&self) -> bool {
        let mem_type = self.get_type();
        let phys = self.get_physical_start();
        let pages = self.get_page_count();

        validate_descriptor_common(mem_type, phys, pages)
    }

    fn is_memory_available(&self) -> bool {
        let mem_type = self.get_type();
        // Available memory types according to UEFI spec:
        // 1: Conventional Memory
        // 2: Boot Services Code
        // 3: Boot Services Data
        // 4: Loader Code
        // 5: Loader Data
        // 9: ACPI Reclaimed Memory
        // 11: ACPI Memory
        matches!(mem_type, 7u32 | 3u32 | 4u32 | 1u32 | 2u32 | 9u32)
    }
}

/// Helper function to validate memory descriptor properties common to both descriptor types
pub(crate) fn validate_descriptor_common(mem_type: u32, phys: u64, pages: u64) -> bool {
    if mem_type > 15 {
        crate::debug_log_no_alloc!("Invalid memory type (out of range): 0x", mem_type as usize);
        return false;
    }
    // debug_log_validate_macro is not defined in this scope, it was probably a local macro or similar
    // I'll omit it for now to ensure compilation.

    if phys % 4096 != 0 {
        crate::debug_log_no_alloc!("Unaligned physical_start: 0x", phys as usize);
        return false;
    }

    if pages == 0 || pages > MAX_DESCRIPTOR_PAGES {
        crate::debug_log_no_alloc!("Invalid page count: ", pages as usize);
        return false;
    }

    let page_size = 4096u64;
    let end_addr = match phys.checked_add(pages.saturating_mul(page_size)) {
        Some(end) if end > 0 => end,
        _ => {
            crate::debug_log_no_alloc!("Overflow in address calculation");
            return false;
        }
    };

    if end_addr > MAX_SYSTEM_MEMORY {
        crate::debug_log_no_alloc!("Memory region too large: end_addr=0x", end_addr as usize);
        return false;
    }

    true
}

pub fn is_valid_memory_descriptor(descriptor: &MemoryMapDescriptor) -> bool {
    if descriptor.descriptor_size < 40 {
        crate::debug_log_no_alloc!("Descriptor size too small: ", descriptor.descriptor_size);
        return false;
    }

    let mem_type = descriptor.get_type();
    let phys = descriptor.get_physical_start();
    let pages = descriptor.get_page_count();

    validate_descriptor_common(mem_type, phys, pages)
}