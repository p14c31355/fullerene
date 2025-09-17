// bellows/src/loader/pe.rs

use crate::uefi::{EfiMemoryType, EfiSystemTable, Result};
use core::{ffi::c_void, mem, ptr, slice};

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
    size_of_headers: u32,
    _checksum: u32,
    _subsystem: u16,
    _dll_characteristics: u16,
    _size_of_stack_reserve: u64,
    _size_of_stack_commit: u64,
    _size_of_heap_reserve: u64,
    _size_of_heap_commit: u64,
    _loader_flags: u32,
    number_of_rva_and_sizes: u32,
    _data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
struct SectionHeader {
    _name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
    _pointer_to_relocations: u32,
    _pointer_to_linenumbers: u32,
    _number_of_relocations: u16,
    _number_of_linenumbers: u16,
    _characteristics: u32,
}

#[repr(C, packed)]
struct ImageBaseRelocation {
    virtual_address: u32,
    size_of_block: u32,
}

#[repr(C, packed)]
struct ImageNtHeaders64 {
    signature: u32,
    file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader64,
}

pub fn load_efi_image(
    st: &EfiSystemTable,
    image_data: &[u8],
) -> Result<extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !> {
    let bs = unsafe { &*st.boot_services };

    let dos_header: &ImageDosHeader = unsafe { &*(image_data.as_ptr() as *const ImageDosHeader) };
    if dos_header.e_magic != 0x5a4d {
        return Err("Invalid DOS signature.");
    }

    let nt_headers: &ImageNtHeaders64 = unsafe {
        &*(image_data.as_ptr().add(dos_header.e_lfanew as usize) as *const ImageNtHeaders64)
    };
    if nt_headers.signature != 0x4550 {
        return Err("Invalid PE signature.");
    }

    // Allocate memory for the image
    let pages_needed = nt_headers.optional_header.size_of_image as usize / 4096 + 1;
    let mut phys_addr: usize = 0;
    if unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        )
    } != 0
    {
        return Err("Failed to allocate pages for kernel image.");
    }

    // Copy headers and sections
    unsafe {
        ptr::copy_nonoverlapping(
            image_data.as_ptr(),
            phys_addr as *mut u8,
            nt_headers.optional_header.size_of_headers as usize,
        );
    }
    let sections_ptr = unsafe {
        image_data
            .as_ptr()
            .add(dos_header.e_lfanew as usize)
            .add(mem::size_of::<ImageNtHeaders64>())
    };
    let sections: &[SectionHeader] = unsafe {
        slice::from_raw_parts(
            sections_ptr as *const SectionHeader,
            nt_headers.file_header.number_of_sections as usize,
        )
    };

    for section in sections.iter() {
        let dest = unsafe { (phys_addr as *mut u8).add(section.virtual_address as usize) };
        let src = unsafe {
            image_data
                .as_ptr()
                .add(section.pointer_to_raw_data as usize)
        };
        unsafe {
            ptr::copy_nonoverlapping(src, dest, section.size_of_raw_data as usize);
        }
    }

    // Handle relocations
    let data_dir_reloc = &nt_headers.optional_header._data_directory[5];
    let reloc_base_va = data_dir_reloc.virtual_address as usize;
    let reloc_size = data_dir_reloc.size as usize;
    if reloc_base_va > 0 && reloc_size > 0 {
        let relocs_start_ptr = unsafe { (phys_addr as *mut u8).add(reloc_base_va as usize) };
        let mut current_reloc_block = relocs_start_ptr as *const u8;
        let relocs_end_ptr = unsafe { relocs_start_ptr.add(reloc_size) };
        let image_base_delta = phys_addr as u64 - nt_headers.optional_header.image_base;

        while current_reloc_block < relocs_end_ptr {
            let reloc_block_header =
                unsafe { &*(current_reloc_block as *const ImageBaseRelocation) };
            let reloc_block_size = reloc_block_header.size_of_block as usize;
            let num_entries = (reloc_block_size - mem::size_of::<ImageBaseRelocation>()) / 2;
            let fixup_list_ptr =
                unsafe { current_reloc_block.add(mem::size_of::<ImageBaseRelocation>()) };
            let fixup_list =
                unsafe { slice::from_raw_parts(fixup_list_ptr as *const u16, num_entries) };
            let reloc_page_va = phys_addr + reloc_block_header.virtual_address as usize;

            for &fixup in fixup_list {
                let fixup_type = (fixup >> 12) & 0xF;
                let fixup_offset = fixup & 0xFFF;
                if fixup_type == 10 {
                    // IMAGE_REL_BASED_DIR64
                    let fixup_address_ptr = (reloc_page_va + fixup_offset as usize) as *mut u64;
                    unsafe {
                        *fixup_address_ptr = (*fixup_address_ptr).wrapping_add(image_base_delta);
                    }
                } else if fixup_type != 0 {
                    (bs.free_pages)(phys_addr, pages_needed);
                    return Err("Unsupported relocation type.");
                }
            }
            current_reloc_block = unsafe { current_reloc_block.add(reloc_block_size) };
        }
    }

    let entry_point_addr = phys_addr + nt_headers.optional_header.address_of_entry_point as usize;

    Ok(unsafe { core::mem::transmute(entry_point_addr) })
}
