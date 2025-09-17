// bellows/src/loader/pe.rs

use crate::uefi::{EfiMemoryType, EfiSystemTable, Result};
use core::{mem, ptr, slice};

#[repr(C, packed)]
struct ImageDosHeader {
    e_magic: u16,
    _pad: [u8; 58],
    e_lfanew: i32,
}

#[repr(C, packed)]
struct ImageFileHeader {
    _machine: u16,
    number_of_sections: u16,
    _time_date_stamp: u32,
    _pointer_to_symbol_table: u32,
    _number_of_symbols: u32,
    size_of_optional_header: u16,
    _characteristics: u16,
}

#[repr(C, packed)]
struct ImageDataDirectory {
    virtual_address: u32,
    size: u32,
}

#[repr(C, packed)]
struct ImageOptionalHeader64 {
    _magic: u16,
    _major_linker_version: u8,
    _minor_linker_version: u8,
    _size_of_code: u32,
    _size_of_initialized_data: u32,
    _size_of_uninitialized_data: u32,
    address_of_entry_point: u32,
    _base_of_code: u32,
    image_base: u64,
    _section_alignment: u32,
    _file_alignment: u32,
    _major_operating_system_version: u16,
    _minor_operating_system_version: u16,
    _major_image_version: u16,
    _minor_image_version: u16,
    _major_subsystem_version: u16,
    _minor_subsystem_version: u16,
    _win32_version_value: u32,
    size_of_image: u32,
    _size_of_headers: u32,
    _checksum: u32,
    _subsystem: u16,
    _dll_characteristics: u16,
    _size_of_stack_reserve: u64,
    _size_of_stack_commit: u64,
    _size_of_heap_reserve: u64,
    _size_of_heap_commit: u64,
    _loader_flags: u32,
    number_of_rva_and_sizes: u32,
    data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
struct ImageSectionHeader {
    _name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
    _pointer_to_relocations: u32,
    _pointer_to_linenumbers: u16,
    _number_of_relocations: u16,
    _number_of_linenumbers: u16,
    _characteristics: u32,
}

#[repr(C, packed)]
struct ImageBaseRelocation {
    virtual_address: u32,
    size_of_block: u32,
}

/// Load an EFI image (PE/COFF file) and return the entry point
pub fn load_efi_image(
    st: &EfiSystemTable,
    image_file: &[u8],
) -> Result<extern "efiapi" fn(usize, *mut EfiSystemTable, *mut core::ffi::c_void, usize) -> !> {
    let bs = unsafe { &*st.boot_services };

    if image_file.len() < mem::size_of::<ImageDosHeader>() {
        return Err("Image file too small for DOS header.");
    }
    let dos_header = unsafe { ptr::read_unaligned(image_file.as_ptr() as *const ImageDosHeader) };
    if dos_header.e_magic != 0x5a4d {
        return Err("Invalid PE/COFF file: Missing DOS header.");
    }

    let pe_header_offset = dos_header.e_lfanew as usize;
    if image_file.len() < pe_header_offset + 4 {
        return Err("Image file too small for PE signature.");
    }
    let pe_signature =
        unsafe { ptr::read_unaligned(image_file.as_ptr().add(pe_header_offset) as *const u32) };
    if pe_signature != 0x00004550 {
        return Err("Invalid PE/COFF file: Missing PE signature.");
    }

    let file_header_ptr = unsafe { image_file.as_ptr().add(pe_header_offset + 4) };
    if image_file.len() < pe_header_offset + 4 + mem::size_of::<ImageFileHeader>() {
        return Err("Image file too small for file header.");
    }
    let file_header = unsafe { ptr::read_unaligned(file_header_ptr as *const ImageFileHeader) };

    let optional_header_ptr = unsafe { file_header_ptr.add(mem::size_of::<ImageFileHeader>()) };
    if image_file.len()
        < pe_header_offset
            + 4
            + mem::size_of::<ImageFileHeader>()
            + file_header.size_of_optional_header as usize
    {
        return Err("Image file too small for optional header.");
    }
    let optional_header =
        unsafe { ptr::read_unaligned(optional_header_ptr as *const ImageOptionalHeader64) };

    let image_entry_point_rva = optional_header.address_of_entry_point as usize;
    let preferred_image_base = optional_header.image_base as usize;
    let preferred_image_size = optional_header.size_of_image as usize;

    let pages_needed = preferred_image_size.div_ceil(4096);
    let mut phys_addr: usize = preferred_image_base;
    let status = (unsafe {
        (bs.allocate_pages)(
            1usize,
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        )
    });
    if status != 0 {
        return Err("Failed to allocate pages for kernel image at preferred address.");
    }
    if phys_addr != preferred_image_base {
        (unsafe { (bs.free_pages)(phys_addr, pages_needed) });
        return Err("Allocation did not return preferred address.");
    }

    let image_ptr = phys_addr as *mut u8;
    let headers_size = optional_header._size_of_headers as usize;
    if image_file.len() < headers_size {
        return Err("Image file headers size is invalid.");
    }
    unsafe {
        ptr::copy_nonoverlapping(image_file.as_ptr(), image_ptr, headers_size);
    }

    let mut section_header_ptr =
        unsafe { optional_header_ptr.add(file_header.size_of_optional_header as usize) };
    for _ in 0..file_header.number_of_sections as usize {
        let section_header =
            unsafe { ptr::read_unaligned(section_header_ptr as *const ImageSectionHeader) };
        if section_header.size_of_raw_data > 0 {
            let raw_data_ptr = unsafe {
                image_file
                    .as_ptr()
                    .add(section_header.pointer_to_raw_data as usize)
            };
            let virtual_address = phys_addr + section_header.virtual_address as usize;
            if image_file.len()
                < section_header.pointer_to_raw_data as usize
                    + section_header.size_of_raw_data as usize
            {
                (unsafe { (bs.free_pages)(phys_addr, pages_needed) });
                return Err("Invalid section data size.");
            }
            unsafe {
                ptr::copy_nonoverlapping(
                    raw_data_ptr,
                    virtual_address as *mut u8,
                    section_header.size_of_raw_data as usize,
                );
            }
        }
        section_header_ptr =
            unsafe { section_header_ptr.add(mem::size_of::<ImageSectionHeader>()) };
    }

    let reloc_data_dir = &optional_header.data_directory[5];
    if reloc_data_dir.size > 0 {
        let reloc_table_offset = unsafe {
            image_file
                .as_ptr()
                .add(reloc_data_dir.virtual_address as usize)
        };
        let mut current_reloc_block = reloc_table_offset;
        while (current_reloc_block as usize - reloc_table_offset as usize)
            < reloc_data_dir.size as usize
        {
            let reloc_block_header: &ImageBaseRelocation =
                unsafe { &*(current_reloc_block as *const ImageBaseRelocation) };
            let reloc_block_size = reloc_block_header.size_of_block as usize;
            let num_entries = (reloc_block_size - mem::size_of::<ImageBaseRelocation>()) / 2;
            let fixup_list_ptr =
                unsafe { current_reloc_block.add(mem::size_of::<ImageBaseRelocation>()) };
            let fixup_list =
                unsafe { slice::from_raw_parts(fixup_list_ptr as *const u16, num_entries) };
            let offset = phys_addr as u64 - preferred_image_base as u64;
            let reloc_page_va = phys_addr + reloc_block_header.virtual_address as usize;

            for &fixup in fixup_list {
                let fixup_type = (fixup >> 12) & 0xF;
                let fixup_offset = fixup & 0xFFF;
                if fixup_type == 10 {
                    let fixup_address_ptr = (reloc_page_va + fixup_offset as usize) as *mut u64;
                    unsafe {
                        *fixup_address_ptr = (*fixup_address_ptr).wrapping_add(offset);
                    }
                } else if fixup_type != 0 {
                    (unsafe { (bs.free_pages)(phys_addr, pages_needed) });
                    return Err("Unsupported relocation type.");
                }
            }
            current_reloc_block = unsafe { current_reloc_block.add(reloc_block_size) };
        }
    }

    let entry_point_addr = phys_addr + image_entry_point_rva;
    Ok(unsafe { mem::transmute(entry_point_addr) })
}
