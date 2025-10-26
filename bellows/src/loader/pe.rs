#[repr(C, packed)]
pub struct ImageDosHeader {
    pub e_magic: u16,
    pub _pad: [u8; 58],
    pub e_lfanew: i32,
}

#[repr(C, packed)]
pub struct ImageFileHeader {
    pub _machine: u16,
    pub number_of_sections: u16,
    pub _time_date_stamp: u32,
    pub _pointer_to_symbol_table: u32,
    pub _number_of_symbols: u32,
    pub size_of_optional_header: u16,
    pub _characteristics: u16,
}

#[repr(C, packed)]
pub struct ImageDataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

#[repr(C, packed)]
pub struct ImageOptionalHeader64 {
    pub _magic: u16,
    pub _major_linker_version: u8,
    pub _minor_linker_version: u8,
    pub _size_of_code: u32,
    pub _size_of_initialized_data: u32,
    pub _size_of_uninitialized_data: u32,
    pub address_of_entry_point: u32,
    pub _base_of_code: u32,
    pub image_base: u64,
    pub _section_alignment: u32,
    pub _file_alignment: u32,
    pub _major_operating_system_version: u16,
    pub _minor_operating_system_version: u16,
    pub _major_image_version: u16,
    pub _minor_image_version: u16,
    pub _major_subsystem_version: u16,
    pub _minor_subsystem_version: u16,
    pub _win32_version_value: u32,
    pub size_of_image: u32,
    pub _size_of_headers: u32,
    pub _checksum: u32,
    pub _subsystem: u16,
    pub _dll_characteristics: u16,
    pub size_of_stack_reserve: u64,
    pub size_of_stack_commit: u64,
    pub size_of_heap_reserve: u64,
    pub size_of_heap_commit: u64,
    pub _loader_flags: u32,
    pub number_of_rva_and_sizes: u32,
    pub data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
pub struct ImageNtHeaders64 {
    pub _signature: u32,
    pub _file_header: ImageFileHeader,
    pub optional_header: ImageOptionalHeader64,
}

#[repr(C, packed)]
pub struct ImageSectionHeader {
    pub _name: [u8; 8],
    pub _virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub _pointer_to_relocations: u32,
    pub _pointer_to_linenumbers: u32,
    pub _number_of_relocations: u16,
    pub _number_of_linenumbers: u16,
    pub _characteristics: u32,
}

#[repr(C, packed)]
pub struct ImageBaseRelocation {
    pub virtual_address: u32,
    pub size_of_block: u32,
}

#[repr(u16)]
pub enum ImageRelBasedType {
    Absolute = 0,
    Dir64 = 10,
}

use core::{ffi::c_void, mem, mem::offset_of, ptr};
use petroleum::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable};
use petroleum::read_unaligned;

use log;

pub fn load_efi_image(
    st: &EfiSystemTable,
    file: &[u8],
) -> petroleum::common::Result<
    extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
> {
    let bs = unsafe { &*st.boot_services };

    if file.len() < mem::size_of::<ImageDosHeader>() {
        return Err(BellowsError::PeParse("File too small for DOS header."));
    }
    let dos_header_ptr = file.as_ptr() as *const ImageDosHeader;
    let e_magic = unsafe { ptr::read_unaligned(dos_header_ptr as *const u16) };
    if e_magic != 0x5a4d {
        return Err(BellowsError::PeParse("Invalid DOS signature (MZ)."));
    }
    let e_lfanew = unsafe {
        ptr::read_unaligned(
            (dos_header_ptr as *const u8).add(offset_of!(ImageDosHeader, e_lfanew)) as *const i32,
        )
    };
    petroleum::println!("DOS header parsed. e_lfanew: {:#x}", e_lfanew);

    let nt_headers_offset = e_lfanew as usize;
    if nt_headers_offset + mem::size_of::<ImageNtHeaders64>() > file.len() {
        return Err(BellowsError::PeParse("Invalid NT headers offset."));
    }
    let nt_headers_ptr = unsafe { file.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64 };
    let optional_header_magic = unsafe {
        ptr::read_unaligned(
            (nt_headers_ptr as *const u8)
                .add(offset_of!(ImageNtHeaders64, optional_header))
                .add(offset_of!(ImageOptionalHeader64, _magic)) as *const u16,
        )
    };
    if optional_header_magic != 0x20b {
        return Err(BellowsError::PeParse("Invalid PE32+ magic number."));
    }

    let image_size = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, size_of_image), u32) as usize;
    let address_of_entry_point = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, address_of_entry_point), u32) as usize;
    let pages_needed = (image_size.max(address_of_entry_point + 4096)).div_ceil(4096);

    let preferred_base = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, image_base), u64) as usize;
    let mut phys_addr: usize = 0;
    let mut status;

    if preferred_base >= 0x1000_0000 {
        phys_addr = 0x100000;
        status = (bs.allocate_pages)(2, EfiMemoryType::EfiLoaderCode, pages_needed, &mut phys_addr);
        if EfiStatus::from(status) != EfiStatus::Success {
            phys_addr = 0;
            status = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, pages_needed, &mut phys_addr);
        }
    } else {
        status = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, pages_needed, &mut phys_addr);
    }

    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed("Failed to allocate memory for PE image."));
    }

    let size_of_headers = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, _size_of_headers), u32) as usize;
    unsafe {
        ptr::copy_nonoverlapping(file.as_ptr(), phys_addr as *mut u8, size_of_headers);
    }

    let number_of_sections = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, _file_header) + offset_of!(ImageFileHeader, number_of_sections), u16) as usize;
    let size_of_optional_header = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, _file_header) + offset_of!(ImageFileHeader, size_of_optional_header), u16) as usize;

    let section_headers_offset = nt_headers_offset + mem::size_of::<u32>() + mem::size_of::<ImageFileHeader>() + size_of_optional_header;
    let section_headers_size = number_of_sections * mem::size_of::<ImageSectionHeader>();
    if section_headers_offset + section_headers_size > file.len() {
        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
        return Err(BellowsError::PeParse("Section headers out of bounds."));
    }

    for i in 0..number_of_sections {
        let section_header_base_ptr = unsafe { file.as_ptr().add(section_headers_offset + i * mem::size_of::<ImageSectionHeader>()) };
        let virtual_address = unsafe { ptr::read_unaligned(section_header_base_ptr.add(offset_of!(ImageSectionHeader, virtual_address)) as *const u32) };
        let size_of_raw_data = unsafe { ptr::read_unaligned(section_header_base_ptr.add(offset_of!(ImageSectionHeader, size_of_raw_data)) as *const u32) };
        let pointer_to_raw_data = unsafe { ptr::read_unaligned(section_header_base_ptr.add(offset_of!(ImageSectionHeader, pointer_to_raw_data)) as *const u32) };

        let src_addr = unsafe { file.as_ptr().add(pointer_to_raw_data as usize) };
        let dst_addr = unsafe { (phys_addr as *mut u8).add(virtual_address as usize) };

        if (src_addr as usize).saturating_add(size_of_raw_data as usize) > (file.as_ptr() as usize).saturating_add(file.len()) ||
           (dst_addr as usize).saturating_add(size_of_raw_data as usize) > ((phys_addr as *mut u8) as usize).saturating_add(pages_needed * 4096) {
            unsafe { (bs.free_pages)(phys_addr, pages_needed) };
            return Err(BellowsError::PeParse("Section data out of bounds."));
        }

        unsafe {
            ptr::copy_nonoverlapping(src_addr, dst_addr, size_of_raw_data as usize);
        }
    }

    let image_base = petroleum::read_unaligned!(nt_headers_ptr, offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, image_base), u64) as usize;
    let image_base_delta = (phys_addr as u64).wrapping_sub(image_base as u64);

    if image_base_delta != 0 {
        let reloc_data_dir_ptr = unsafe { (nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header) + offset_of!(ImageOptionalHeader64, data_directory) + mem::size_of::<ImageDataDirectory>() * 5) as *const ImageDataDirectory };
        let reloc_data_dir = unsafe { ptr::read_unaligned(reloc_data_dir_ptr) };

        if reloc_data_dir.virtual_address != 0 {
            let reloc_table_ptr = unsafe { (phys_addr as *mut u8).add(reloc_data_dir.virtual_address as usize) };
            if (reloc_table_ptr as usize).saturating_add(reloc_data_dir.size as usize) > phys_addr.saturating_add(pages_needed * 4096) {
                unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                return Err(BellowsError::PeParse("Relocation table out of bounds."));
            }

            let mut current_reloc_block_ptr = reloc_table_ptr as *const ImageBaseRelocation;
            let end_reloc_table_ptr = unsafe { reloc_table_ptr.add(reloc_data_dir.size as usize) };

            while (current_reloc_block_ptr as *const u8) < end_reloc_table_ptr {
                let virtual_address = unsafe { ptr::read_unaligned((current_reloc_block_ptr as *const u8).add(offset_of!(ImageBaseRelocation, virtual_address)) as *const u32) };
                let reloc_block_size = unsafe { ptr::read_unaligned((current_reloc_block_ptr as *const u8).add(offset_of!(ImageBaseRelocation, size_of_block)) as *const u32) as usize };

                if reloc_block_size == 0 { break; }

                let mut fixup_ptr = unsafe { (current_reloc_block_ptr as *const u8).add(mem::size_of::<ImageBaseRelocation>()) as *const u16 };
                let end_of_block_ptr = unsafe { (current_reloc_block_ptr as *const u8).add(reloc_block_size) };

                while (fixup_ptr as *const u8) < end_of_block_ptr {
                    let fixup = unsafe { ptr::read_unaligned(fixup_ptr) };
                    let fixup_type = (fixup >> 12) as u8;
                    let fixup_offset = (fixup & 0xFFF) as usize;

                    if fixup_type == ImageRelBasedType::Dir64 as u8 {
                        let fixup_address = phys_addr.saturating_add(virtual_address as usize).saturating_add(fixup_offset);
                        if fixup_address + 8 > phys_addr + pages_needed * 4096 {
                            unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                            return Err(BellowsError::PeParse("Relocation fixup address is out of bounds."));
                        }
                        let fixup_address_ptr = fixup_address as *mut u64;
                        unsafe { *fixup_address_ptr = (*fixup_address_ptr).wrapping_add(image_base_delta); }
                    } else if fixup_type != ImageRelBasedType::Absolute as u8 {
                        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                        return Err(BellowsError::PeParse("Unsupported relocation type."));
                    }
                    fixup_ptr = unsafe { fixup_ptr.add(1) };
                }

                current_reloc_block_ptr = end_of_block_ptr as *const ImageBaseRelocation;
            }
        }
    }

    let entry_point_addr = phys_addr.saturating_add(address_of_entry_point);
    if entry_point_addr >= phys_addr + pages_needed * 4096 || entry_point_addr < phys_addr {
        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
        return Err(BellowsError::PeParse("Entry point address is outside allocated memory."));
    }

    log::info!("PE: EFI image loaded. Entry: 0x{:x}", entry_point_addr);
    let entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> ! = unsafe { mem::transmute(entry_point_addr) };
    log::info!("PE: load_efi_image completed successfully.");
    Ok(entry)
}
