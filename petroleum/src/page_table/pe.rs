use x86_64::{PhysAddr, structures::paging::PageTableFlags};
use goblin::pe::PE;
use crate::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable};
use core::ffi::c_void;

pub const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024;
pub const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PeSectionHeader {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub _pointer_to_relocations: u32,
    pub _pointer_to_linenumbers: u32,
    pub _number_of_relocations: u16,
    pub _number_of_linenumbers: u16,
    pub characteristics: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PeSection {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub characteristics: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BaseRelocationBlock {
    pub page_rva: u32,
    pub block_size: u32,
    pub entries: [u32; 0],
}

pub struct PeParser {
    pub pe_base: *const u8,
    pub pe_offset: usize,
}

impl PeParser {
    const MAX_PE_SEARCH_DISTANCE: usize = 10 * 1024 * 1024;
    const MAX_PE_OFFSET: usize = 16 * 1024 * 1024;
    const MAX_PE_HEADER_OFFSET: usize = 1024 * 1024;
    const MAX_PE_SECTIONS: usize = 16;

    pub unsafe fn new(kernel_ptr: *const u8) -> Option<Self> {
        unsafe { find_pe_base(kernel_ptr) }.map(|base| {
            let pe_offset = crate::read_unaligned!(base, 0x3c, u32) as usize;
            Self {
                pe_base: base,
                pe_offset,
            }
        })
    }

    pub unsafe fn size_of_image(&self) -> Option<u64> {
        if self.pe_offset == 0
            || self.pe_offset >= PeParser::MAX_PE_HEADER_OFFSET
            || self.pe_base.is_null()
        {
            return None;
        }
        let magic = crate::read_unaligned!(self.pe_base, self.pe_offset + 24, u16);
        if magic != 0x10B && magic != 0x20B {
            return None;
        }
        Some(crate::read_unaligned!(self.pe_base, self.pe_offset + 24 + 0x38, u32) as u64)
    }

    pub unsafe fn sections(&self) -> Option<[PeSection; PeParser::MAX_PE_SECTIONS]> {
        if self.pe_offset == 0
            || self.pe_offset >= PeParser::MAX_PE_HEADER_OFFSET
            || self.pe_base.is_null()
        {
            return None;
        }
        let num_sections =
            unsafe { crate::read_unaligned!(self.pe_base, self.pe_offset + 6, u16) } as usize;
        let optional_header_size =
            unsafe { crate::read_unaligned!(self.pe_base, self.pe_offset + 20, u16) } as usize;
        let section_table_offset = self.pe_offset + 24 + optional_header_size;

        let mut sections = [PeSection {
            name: [0; 8],
            virtual_size: 0,
            virtual_address: 0,
            size_of_raw_data: 0,
            pointer_to_raw_data: 0,
            characteristics: 0,
        }; PeParser::MAX_PE_SECTIONS];
        for i in 0..num_sections.min(PeParser::MAX_PE_SECTIONS) {
            let offset = section_table_offset + i * 40;
            let header = unsafe { crate::read_unaligned!(self.pe_base, offset, PeSectionHeader) };
            sections[i] = PeSection {
                name: header.name,
                virtual_size: header.virtual_size,
                virtual_address: header.virtual_address,
                size_of_raw_data: header.size_of_raw_data,
                pointer_to_raw_data: header.pointer_to_raw_data,
                characteristics: header.characteristics,
            };
        }
        Some(sections)
    }
}

pub unsafe fn find_pe_base(start_ptr: *const u8) -> Option<*const u8> { unsafe {
    log_page_table_op!("PE base", "starting search", start_ptr as usize);

    for i in 0..PeParser::MAX_PE_SEARCH_DISTANCE {
        let candidate_addr = match (start_ptr as usize).checked_sub(i) {
            Some(addr) => addr as *const u8,
            None => break,
        };

        if candidate_addr.read() == b'M' && candidate_addr.add(1).read() == b'Z' {
            log_page_table_op!("PE base", "found MZ candidate", candidate_addr as usize);
            let pe_offset = crate::read_unaligned!(candidate_addr, 0x3c, u32) as usize;

            if pe_offset > 0 && pe_offset < 16 * 1024 * 1024 {
                let pe_sig = crate::read_unaligned!(candidate_addr, pe_offset, u32);
                if pe_sig == 0x00004550 {
                    log_page_table_op!("PE base", "found valid PE", candidate_addr as usize);
                    return Some(candidate_addr);
                }
            }
        }
    }

    log_page_table_op!("PE base", "search complete - no PE found");
    None
}}

pub fn derive_pe_flags(characteristics: u32) -> PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    let mut flags = Flags::PRESENT;
    
    // Ensure data sections are writable. 
    // In early boot, we prefer over-permissioning to avoid triple faults.
    if (characteristics & 0x8000_0000) != 0 || (characteristics & 0x2000_0000) == 0 {
        flags |= Flags::WRITABLE;
    }
    
    if (characteristics & 0x2000_0000) == 0 {
        flags |= Flags::NO_EXECUTE;
    }
    flags
}

pub unsafe fn calculate_kernel_memory_size(kernel_phys_start: PhysAddr) -> u64 { unsafe {
    log_page_table_op!("PE size calculation", "starting", kernel_phys_start.as_u64() as usize);

    if kernel_phys_start.as_u64() == 0 {
        crate::debug_log_no_alloc!("Kernel phys start is 0, using fallback size");
        return FALLBACK_KERNEL_SIZE;
    }

    let parser = match PeParser::new(kernel_phys_start.as_u64() as *const u8) {
        Some(p) => p,
        None => {
            log_page_table_op!("PE size calculation", "parser creation failed, using fallback", 0);
            return FALLBACK_KERNEL_SIZE;
        }
    };

    match parser.size_of_image() {
        Some(size) => {
            let padded_size = (size + KERNEL_MEMORY_PADDING).div_ceil(4096) * 4096;
            log_page_table_op!("PE size calculation", "parsing successful", padded_size as usize);
            padded_size
        }
        None => {
            log_page_table_op!("PE size calculation", "size_of_image failed, using fallback", 0);
            FALLBACK_KERNEL_SIZE
        }
    }
}}

/// Load and parse EFI PE image from file data
pub fn load_efi_image(
    st: &EfiSystemTable,
    file: &[u8],
    phys_offset: usize,
) -> Result<
    (PhysAddr, u64, extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !),
    BellowsError,
> {
    let bs = unsafe { &*st.boot_services };
    
    let pe = PE::parse(file).map_err(|_| BellowsError::PeParse("Failed to parse PE image"))?;
    
    let optional_header = pe.header.optional_header.as_ref().ok_or(BellowsError::PeParse("Missing optional header"))?;
    let address_of_entry_point = optional_header.standard_fields.address_of_entry_point as usize;
    let image_size = optional_header.windows_fields.size_of_image as u64;
    
    let pages_needed = (image_size.max(address_of_entry_point as u64 + 4096)).div_ceil(4096) as usize;
    let preferred_base = optional_header.windows_fields.image_base as usize;
    
    let mut phys_addr: usize = 0;
    let status = if preferred_base >= 0x1000_0000 {
        let mut addr = 0;
        let s = (bs.allocate_pages)(2, EfiMemoryType::EfiLoaderCode, pages_needed, &mut addr);
        if EfiStatus::from(s) != EfiStatus::Success {
            let mut addr2 = 0;
            let s2 = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, pages_needed, &mut addr2);
            phys_addr = addr2;
            s2
        } else {
            phys_addr = addr;
            s
        }
    } else {
        let s = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, pages_needed, &mut phys_addr);
        s
    };

    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed("Failed to allocate memory for PE image."));
    }

    // Copy headers
    let size_of_headers = optional_header.windows_fields.size_of_headers as usize;
    unsafe {
        core::ptr::copy_nonoverlapping(file.as_ptr(), phys_addr as *mut u8, size_of_headers);
    }

    // Copy sections
    for section in &pe.sections {
        let src_addr = unsafe { file.as_ptr().add(section.pointer_to_raw_data as usize) };
        let dst_addr = unsafe { (phys_addr as *mut u8).add(section.virtual_address as usize) };
        
        if (src_addr as usize).saturating_add(section.size_of_raw_data as usize) > (file.as_ptr() as usize).saturating_add(file.len())
            || (dst_addr as usize).saturating_add(section.size_of_raw_data as usize) > (phys_addr as usize).saturating_add(pages_needed * 4096)
        {
            (bs.free_pages)(phys_addr, pages_needed);
            return Err(BellowsError::PeParse("Section data out of bounds."));
        }
        
        unsafe {
            core::ptr::copy_nonoverlapping(src_addr, dst_addr, section.size_of_raw_data as usize);
        }
    }

    // Relocations
    let image_base = optional_header.windows_fields.image_base as usize;
    let virtual_base = phys_offset + phys_addr;
    let image_base_delta = (virtual_base as i64) - (image_base as i64);

    if image_base_delta != 0 {
        // Use DataDirectory to find the base relocation table
        if let Some(reloc_dir) = optional_header.data_directories.get_base_relocation_table() {
            let reloc_rva = reloc_dir.virtual_address as usize;
            let reloc_size = reloc_dir.size as usize;
            
            let reloc_data_ptr = unsafe { (phys_addr as *mut u8).add(reloc_rva) };
            let mut current_reloc_ptr = reloc_data_ptr;
            let end_reloc_ptr = unsafe { reloc_data_ptr.add(reloc_size) };

            while current_reloc_ptr < end_reloc_ptr {
                let block = unsafe { &*(current_reloc_ptr as *const BaseRelocationBlock) };
                if block.block_size == 0 { break; }
                
                let num_entries = (block.block_size as usize).saturating_sub(8) / 2;
                
                for i in 0..num_entries {
                    let entry_offset = 8 + i * 2;
                    let entry_ptr = unsafe { current_reloc_ptr.add(entry_offset) };
                    let entry = unsafe { core::ptr::read_unaligned(entry_ptr as *const u16) };

                    // Type 10 is IMAGE_REL_BASED_DIR64 (64-bit absolute address)
                    if (entry >> 12) == 10 {
                        let offset = (entry & 0x0FFF) as usize;
                        let target_addr = phys_addr + block.page_rva as usize + offset;
                        
                        unsafe {
                            let ptr = target_addr as *mut u64;
                            if !ptr.is_null() {
                                let val = core::ptr::read_unaligned(ptr);
                                core::ptr::write_unaligned(ptr, (val as i64).wrapping_add(image_base_delta) as u64);
                            }
                        }
                    }
                }
                current_reloc_ptr = unsafe { current_reloc_ptr.add(block.block_size as usize) };
            }
        }
    }

    let entry_point_phys = phys_addr.saturating_add(address_of_entry_point);
    if entry_point_phys >= phys_addr + pages_needed * 4096 || entry_point_phys < phys_addr {
        (bs.free_pages)(phys_addr, pages_needed);
        return Err(BellowsError::PeParse("Entry point address is outside allocated memory."));
    }
    
    let entry_point_virt = phys_offset + entry_point_phys;
    let entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> ! = unsafe { core::mem::transmute(entry_point_virt) };
    Ok((PhysAddr::new(phys_addr as u64), entry_point_phys as u64, entry))
}

#[repr(C)]
pub struct Elf64Ehdr {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

#[repr(C)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}
