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
    _number_of_rva_and_sizes: u32,
    _data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
struct ImageNtHeaders64 {
    signature: u32,
    file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader64,
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

pub fn load_efi_image(
    st: &EfiSystemTable,
    image_data: &[u8],
) -> Result<extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !> {
    let bs = unsafe { &*st.boot_services };
    let dos_header: &ImageDosHeader = unsafe { &*(image_data.as_ptr() as *const ImageDosHeader) };
    if dos_header.e_magic != 0x5a4d {
        return Err("Invalid MZ header.");
    }
    let nt_headers_offset = dos_header.e_lfanew as usize;
    let nt_headers: &ImageNtHeaders64 =
        unsafe { &*(image_data.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64) };
    if nt_headers.signature != 0x4550 {
        return Err("Invalid PE signature.");
    }

    // Allocate memory for the image
    let pages_needed = (nt_headers.optional_header.size_of_image as usize).div_ceil(4096);
    let mut phys_addr: usize = 0;
    // Safety:
    // The `allocate_pages` function is a UEFI boot service. Its function pointer
    // is assumed to be valid. The arguments are correct.
    if {
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

    if phys_addr == 0 {
        return Err("Allocated image address is null.");
    }

    let headers_size = nt_headers.optional_header.size_of_headers as usize;
    if headers_size > 0 {
        if phys_addr.saturating_add(headers_size) > (phys_addr.saturating_add(pages_needed * 4096))
        {
            (bs.free_pages)(phys_addr, pages_needed);
            return Err("Header size is too large for allocated memory.");
        }
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
    let sections: &[SectionHeader] = unsafe {
        slice::from_raw_parts(
            sections_ptr as *const SectionHeader,
            nt_headers.file_header.number_of_sections as usize,
        )
    };

    for section in sections.iter() {
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
            || (dest as usize) < phys_addr
            || unsafe { dest.add(copy_size) as usize }
                > (phys_addr.saturating_add(pages_needed * 4096))
        {
            {
                (bs.free_pages)(phys_addr, pages_needed);
            }
            return Err("Section destination is outside allocated memory.");
        }

        // Safety:
        // We have bounds-checked the source and destination pointers and the size.
        // `copy_nonoverlapping` is now safe to call.
        unsafe {
            ptr::copy_nonoverlapping(src, dest, copy_size);
        }

        if section.size_of_raw_data < section.virtual_size {
            let zero_fill_start = unsafe { dest.add(section.size_of_raw_data as usize) };
            let zero_fill_size = (section.virtual_size - section.size_of_raw_data) as usize;
            if unsafe { zero_fill_start.add(zero_fill_size) as usize }
                > (phys_addr.saturating_add(pages_needed * 4096))
            {
                {
                    (bs.free_pages)(phys_addr, pages_needed);
                }
                return Err("Zero-fill region is outside allocated memory.");
            }
            unsafe {
                ptr::write_bytes(zero_fill_start, 0, zero_fill_size);
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
        if (relocs_start_ptr as usize).saturating_add(reloc_size)
            > (phys_addr.saturating_add(pages_needed * 4096))
        {
            (bs.free_pages)(phys_addr, pages_needed);

            return Err("Relocation table is outside allocated memory.");
        }

        let mut current_reloc_block = relocs_start_ptr;
        let relocs_end_ptr = unsafe { relocs_start_ptr.add(reloc_size) };
        let image_base_delta =
            (phys_addr as u64).wrapping_sub(nt_headers.optional_header.image_base);

        while (current_reloc_block as usize) < (relocs_end_ptr as usize) {
            // Safety:
            // The pointer is checked against the end of the relocation table.
            let reloc_block_header =
                unsafe { &*(current_reloc_block as *const ImageBaseRelocation) };
            let reloc_block_size = reloc_block_header.size_of_block as usize;
            if reloc_block_size == 0 {
                break;
            }

            if (current_reloc_block as usize).saturating_add(reloc_block_size)
                > (relocs_end_ptr as usize)
            {
                (bs.free_pages)(phys_addr, pages_needed);
                return Err("Relocation block size is out of bounds.");
            }

            let num_entries = (reloc_block_size - mem::size_of::<ImageBaseRelocation>()) / 2;
            let fixup_list_ptr =
                unsafe { current_reloc_block.add(mem::size_of::<ImageBaseRelocation>()) };
            let fixup_list_end = unsafe { fixup_list_ptr.add(num_entries * 2) };

            if (fixup_list_end as usize) > (relocs_end_ptr as usize) {
                (bs.free_pages)(phys_addr, pages_needed);

                return Err("Relocation fixup list is malformed or out of bounds.");
            }

            for i in 0..num_entries {
                let fixup_entry_ptr = unsafe { fixup_list_ptr.add(i * 2) };
                let fixup_entry = unsafe { *(fixup_entry_ptr as *const u16) };
                let fixup_type = (fixup_entry & 0xf000) >> 12;
                let fixup_offset = (fixup_entry & 0x0fff) as usize;

                if fixup_type == 10 {
                    let fixup_address = phys_addr
                        .saturating_add(reloc_block_header.virtual_address as usize)
                        .saturating_add(fixup_offset);

                    if fixup_address < phys_addr
                        || fixup_address.saturating_add(8)
                            > (phys_addr.saturating_add(pages_needed * 4096))
                    {
                        (bs.free_pages)(phys_addr, pages_needed);

                        return Err("Relocation fixup address is out of bounds.");
                    }
                    let fixup_address_ptr = fixup_address as *mut u64;
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

    let entry_point_addr =
        phys_addr.saturating_add(nt_headers.optional_header.address_of_entry_point as usize);

    if entry_point_addr >= phys_addr.saturating_add(pages_needed * 4096)
        || entry_point_addr < phys_addr
    {
        (bs.free_pages)(phys_addr, pages_needed);
        return Err("Entry point address is outside allocated memory.");
    }

    // Safety:
    // We are converting a memory address to a function pointer.
    // The `entry_point_addr` is calculated from the PE headers and has been checked to be
    // within the allocated memory. The `efiapi` calling convention is also assumed.
    Ok(unsafe { core::mem::transmute(entry_point_addr) })
}
