use x86_64::{PhysAddr, VirtAddr, structures::paging::PageTableFlags};

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

pub struct PeParser {
    pe_base: *const u8,
    pe_offset: usize,
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

unsafe fn find_pe_base(start_ptr: *const u8) -> Option<*const u8> {
    log_page_table_op!("PE base", "starting search", start_ptr as usize);

    for i in 0..PeParser::MAX_PE_SEARCH_DISTANCE {
        let candidate_addr = unsafe {
            match (start_ptr as usize).checked_sub(i) {
                Some(addr) => addr as *const u8,
                None => break,
            }
        };

        unsafe {
            if candidate_addr.read() == b'M' && candidate_addr.add(1).read() == b'Z' {
                log_page_table_op!("PE base", "found MZ candidate", candidate_addr as usize);
                let pe_offset = crate::read_unaligned!(candidate_addr, 0x3c, u32) as usize;

                if pe_offset > 0 && pe_offset < PeParser::MAX_PE_OFFSET {
                    let pe_sig = crate::read_unaligned!(candidate_addr, pe_offset, u32);
                    if pe_sig == 0x00004550 {
                        log_page_table_op!("PE base", "found valid PE", candidate_addr as usize);
                        return Some(candidate_addr);
                    }
                }
            }
        }

        if i % 100000 == 0 && i != 0 {
            log_page_table_op!("PE base", "progress", i);
        }

        if i >= PeParser::MAX_PE_SEARCH_DISTANCE / 4 {
            log_page_table_op!("PE base", "long search warning", i);
        }
    }

    log_page_table_op!("PE base", "search complete - no PE found");
    None
}

pub fn derive_pe_flags(characteristics: u32) -> PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    let mut flags = Flags::PRESENT;
    if (characteristics & 0x8000_0000) != 0 {
        flags |= Flags::WRITABLE;
    }
    if (characteristics & 0x2000_0000) == 0 {
        flags |= Flags::NO_EXECUTE;
    }
    flags
}

pub unsafe fn calculate_kernel_memory_size(kernel_phys_start: PhysAddr) -> u64 {
    log_page_table_op!(
        "PE size calculation",
        "starting",
        kernel_phys_start.as_u64() as usize
    );

    if kernel_phys_start.as_u64() == 0 {
        crate::debug_log_no_alloc!("Kernel phys start is 0, using fallback size");
        return FALLBACK_KERNEL_SIZE;
    }

    let parser = match unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
        Some(p) => {
            log_page_table_op!("PE size calculation", "parser created successfully", 0);
            p
        }
        None => {
            log_page_table_op!("PE size calculation", "parser creation failed, using fallback", 0);
            return FALLBACK_KERNEL_SIZE;
        }
    };

    match unsafe { parser.size_of_image() } {
        Some(size) => {
            let padded_size = (size + KERNEL_MEMORY_PADDING).div_ceil(4096) * 4096;
            log_page_table_op!(
                "PE size calculation",
                "parsing successful",
                padded_size as usize
            );
            padded_size
        }
        None => {
            log_page_table_op!("PE size calculation", "size_of_image failed, using fallback", 0);
            FALLBACK_KERNEL_SIZE
        }
    }
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