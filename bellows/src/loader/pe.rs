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

    // Safety:
    // The image_data slice is assumed to be valid and large enough to contain
    // the DOS header. The pointer is checked to be non-null and correctly aligned.
    let dos_header: &ImageDosHeader = if image_data.len() < mem::size_of::<ImageDosHeader>() {
        return Err("Image data is too small to contain DOS header.");
    } else {
        unsafe { &*(image_data.as_ptr() as *const ImageDosHeader) }
    };

    if dos_header.e_magic != 0x5a4d {
        return Err("Invalid DOS signature.");
    }

    // Safety:
    // The NT headers location is calculated from the DOS header.
    // The length of the image_data is checked to ensure it's large enough to
    // contain the NT headers.
    let nt_headers_offset = dos_header.e_lfanew as usize;
    if image_data.len() < nt_headers_offset + mem::size_of::<ImageNtHeaders64>() {
        return Err("Image data is too small to contain NT headers.");
    }
    let nt_headers: &ImageNtHeaders64 =
        unsafe { &*(image_data.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64) };

    if nt_headers.signature != 0x4550 {
        return Err("Invalid PE signature.");
    }

    // Allocate memory for the image
    let pages_needed = nt_headers.optional_header.size_of_image.div_ceil(4096) as usize;
    let mut phys_addr: usize = 0;
    // Safety:
    // The `allocate_pages` function is a UEFI boot service. Its function pointer
    // is assumed to be valid. The arguments are correct.
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
    // Safety:
    // We have allocated a valid memory region `phys_addr` of sufficient size.
    // The source pointers (`image_data.as_ptr()`) are valid.
    let headers_size = nt_headers.optional_header.size_of_headers as usize;
    if headers_size > 0 {
        unsafe {
            ptr::copy_nonoverlapping(image_data.as_ptr(), phys_addr as *mut u8, headers_size);
        }
    }

    let sections_ptr = unsafe {
        image_data
            .as_ptr()
            .add(nt_headers_offset)
            .add(mem::size_of::<ImageNtHeaders64>())
    };

    // Safety:
    // We have checked that the image data contains the file header, and the
    // number of sections is derived from a trusted source (the file header).
    // The memory pointed to by `sections_ptr` is valid for creating the slice.
    let sections: &[SectionHeader] = unsafe {
        slice::from_raw_parts(
            sections_ptr as *const SectionHeader,
            nt_headers.file_header.number_of_sections as usize,
        )
    };

    for section in sections.iter() {
        // Safety:
        // We are copying data from the source image file to the newly allocated
        // memory. The pointers are checked to be within the bounds of the allocated
        // memory and the source data.
        let dest = phys_addr.saturating_add(section.virtual_address as usize) as *mut u8;
        let src = unsafe {
            image_data
                .as_ptr()
                .add(section.pointer_to_raw_data as usize)
        };
        let copy_size = core::cmp::min(
            section.size_of_raw_data as usize,
            image_data
                .len()
                .saturating_sub(section.pointer_to_raw_data as usize),
        );

        // Ensure the destination is within our allocated memory
        if dest.is_null()
            || dest < phys_addr as *mut u8
            || (dest as usize).saturating_add(copy_size) as *mut u8
                > (phys_addr.saturating_add(pages_needed * 4096)) as *mut u8
        {
            unsafe {
                (bs.free_pages)(phys_addr, pages_needed);
            }
            return Err("Section destination is outside allocated memory.");
        }

        unsafe {
            ptr::copy_nonoverlapping(src, dest, copy_size);
        }

        // After ptr::copy_nonoverlapping(...)
        let raw_size = section.size_of_raw_data as usize;
        let virtual_size = section.virtual_size as usize;
        if virtual_size > raw_size {
            let remaining_size = virtual_size - raw_size;
            // Safety: `dest` is a valid pointer within allocated memory.
            // `raw_size` is checked to be within bounds by `copy_nonoverlapping` above.
            // We need to ensure `remaining_dest` and `remaining_size` do not exceed
            // the allocated image memory.
            unsafe {
                let remaining_dest = dest.add(raw_size);

                // Bounds check for `remaining_dest` before writing.
                if remaining_dest.is_null()
                    || remaining_dest < phys_addr as *mut u8
                    || (remaining_dest as usize).saturating_add(remaining_size) as *mut u8
                        > (phys_addr.saturating_add(pages_needed * 4096)) as *mut u8
                {
                    (bs.free_pages)(phys_addr, pages_needed);
                    return Err("Zero-fill destination is outside allocated memory.");
                }
                ptr::write_bytes(remaining_dest, 0, remaining_size);
            }
        }
    }

    // Handle relocations
    const IMAGE_DIRECTORY_ENTRY_BASERELOC: usize = 5;
    let data_dir_reloc =
        &nt_headers.optional_header._data_directory[IMAGE_DIRECTORY_ENTRY_BASERELOC];
    let reloc_base_va = data_dir_reloc.virtual_address as usize;
    let reloc_size = data_dir_reloc.size as usize;
    if reloc_base_va > 0 && reloc_size > 0 {
        let relocs_start_ptr = unsafe { (phys_addr as *mut u8).add(reloc_base_va) };
        let mut current_reloc_block = relocs_start_ptr;
        let relocs_end_ptr = unsafe { relocs_start_ptr.add(reloc_size) };
        let image_base_delta = phys_addr as u64 - nt_headers.optional_header.image_base;

        if relocs_end_ptr > (phys_addr.saturating_add(pages_needed * 4096)) as *mut u8 {
            unsafe {
                (bs.free_pages)(phys_addr, pages_needed);
            }
            return Err("Relocation table is outside allocated memory.");
        }

        while current_reloc_block < relocs_end_ptr {
            // Safety:
            // The pointer is checked against the end of the relocation table.
            let reloc_block_header =
                unsafe { &*(current_reloc_block as *const ImageBaseRelocation) };
            let reloc_block_size = reloc_block_header.size_of_block as usize;
            if reloc_block_size == 0 {
                break;
            }
            let num_entries = (reloc_block_size - mem::size_of::<ImageBaseRelocation>()) / 2;
            let fixup_list_ptr =
                unsafe { current_reloc_block.add(mem::size_of::<ImageBaseRelocation>()) };
            let fixup_list_end = unsafe { fixup_list_ptr.add(num_entries * 2) };

            // Bounds checking for the fixup list itself.
            if fixup_list_end > relocs_end_ptr {
                unsafe {
                    (bs.free_pages)(phys_addr, pages_needed);
                }
                return Err("Relocation fixup list is malformed or out of bounds.");
            }

            // Safety:
            // We've verified the pointer and number of entries.
            let fixup_list =
                unsafe { slice::from_raw_parts(fixup_list_ptr as *const u16, num_entries) };
            let reloc_page_va = phys_addr + reloc_block_header.virtual_address as usize;

            for &fixup in fixup_list {
                let fixup_type = (fixup >> 12) & 0xF;
                let fixup_offset = fixup & 0xFFF;
                const IMAGE_REL_BASED_DIR64: u16 = 10;
                if fixup_type == IMAGE_REL_BASED_DIR64 {
                    // IMAGE_REL_BASED_DIR64
                    let fixup_address_ptr =
                        (reloc_page_va.saturating_add(fixup_offset as usize)) as *mut u64;
                    // Additional check to prevent out-of-bounds writes
                    if fixup_address_ptr < phys_addr as *mut u64
                        || fixup_address_ptr as usize
                            >= phys_addr.saturating_add(pages_needed * 4096)
                    {
                        unsafe {
                            (bs.free_pages)(phys_addr, pages_needed);
                        }
                        return Err("Relocation fixup address is out of bounds.");
                    }
                    unsafe {
                        *fixup_address_ptr = (*fixup_address_ptr).wrapping_add(image_base_delta);
                    }
                } else if fixup_type != 0 {
                    unsafe {
                        (bs.free_pages)(phys_addr, pages_needed);
                    }
                    return Err("Unsupported relocation type.");
                }
            }
            current_reloc_block = unsafe { current_reloc_block.add(reloc_block_size) };
        }
    }

    let entry_point_addr =
        phys_addr.saturating_add(nt_headers.optional_header.address_of_entry_point as usize);

    // Safety:
    // We are converting a memory address to a function pointer.
    // The `entry_point_addr` is calculated from the PE headers, which are
    // assumed to be correct. The `efiapi` calling convention is also assumed.
    Ok(unsafe { core::mem::transmute(entry_point_addr) })
}
