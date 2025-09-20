// bellows/src/loader/pe.rs

use crate::uefi::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable, Result};
use core::{ffi::c_void, mem, ptr};

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
    size_of_stack_reserve: u64,
    size_of_stack_commit: u64,
    size_of_heap_reserve: u64,
    size_of_heap_commit: u64,
    _loader_flags: u32,
    number_of_rva_and_sizes: u32,
    data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
struct ImageNtHeaders64 {
    _signature: u32,
    _file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader64,
}

#[repr(C, packed)]
struct ImageSectionHeader {
    _name: [u8; 8],
    _virtual_size: u32,
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

#[repr(u16)]
enum ImageRelBasedType {
    Absolute = 0,
    High = 1,
    Low = 2,
    HighLow = 3,
    HighAdj = 4,
    MachineSpecific1 = 5,
    Reserved = 6,
    Dir64 = 10,
}

/// Loads a PE image from a buffer into memory using UEFI Boot Services.
pub fn load_efi_image(
    st: &EfiSystemTable,
    file: &[u8],
) -> Result<extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !> {
    let bs = unsafe { &*st.boot_services };

    // Safety:
    // This is safe because we check the file size to ensure there's enough data
    // to read the headers. The pointer is valid within the bounds of `file`.
    if file.len() < mem::size_of::<ImageDosHeader>() {
        return Err(BellowsError::PeParse("File too small for DOS header."));
    }
    let dos_header: &ImageDosHeader = unsafe { &*(file.as_ptr() as *const ImageDosHeader) };
    if dos_header.e_magic != 0x5a4d {
        return Err(BellowsError::PeParse("Invalid DOS signature (MZ)."));
    }

    // Safety:
    // The offset `e_lfanew` is checked to be within the file bounds before dereferencing.
    let nt_headers_offset = dos_header.e_lfanew as usize;
    if nt_headers_offset + mem::size_of::<ImageNtHeaders64>() > file.len() {
        return Err(BellowsError::PeParse("Invalid NT headers offset."));
    }
    let nt_headers: &ImageNtHeaders64 =
        unsafe { &*(file.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64) };

    if nt_headers.optional_header._magic != 0x20b {
        return Err(BellowsError::PeParse("Invalid PE32+ magic number."));
    }

    let image_size = nt_headers.optional_header.size_of_image as usize;
    let pages_needed = image_size.div_ceil(4096);
    let mut phys_addr: usize = 0;
    let status = {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate memory for PE image.",
        ));
    }

    let _image_base_addr = nt_headers.optional_header.image_base as usize;

    // Safety:
    // We have allocated memory for the image and `phys_addr` is a valid pointer to it.
    // The `copy_nonoverlapping` is safe because `file.as_ptr()` is a valid source
    // and `phys_addr` is a valid destination, and we only copy the header size, which is a known good size.
    unsafe {
        ptr::copy_nonoverlapping(
            file.as_ptr(),
            phys_addr as *mut u8,
            nt_headers.optional_header._size_of_headers as usize,
        );
    }

    let section_headers_offset = nt_headers_offset
        + mem::size_of::<u32>()
        + mem::size_of::<ImageFileHeader>()
        + nt_headers._file_header.size_of_optional_header as usize;
    let section_headers_size =
        nt_headers._file_header.number_of_sections as usize * mem::size_of::<ImageSectionHeader>();
    if section_headers_offset + section_headers_size > file.len() {
        // Safety:
        // We need to free the previously allocated memory if an error occurs.
        
            (bs.free_pages)(phys_addr, pages_needed);
        
        return Err(BellowsError::PeParse("Section headers out of bounds."));
    }

    for i in 0..nt_headers._file_header.number_of_sections {
        let section_header_ptr = unsafe {
            file.as_ptr()
                .add(section_headers_offset + i as usize * mem::size_of::<ImageSectionHeader>())
        };
        // Safety:
        // We have checked that the pointer is within the bounds of the file buffer.
        let section_header: &ImageSectionHeader =
            unsafe { &*(section_header_ptr as *const ImageSectionHeader) };

        let src_addr = unsafe {
            file.as_ptr()
                .add(section_header.pointer_to_raw_data as usize)
        };
        let dst_addr =
            unsafe { (phys_addr as *mut u8).add(section_header.virtual_address as usize) };

        if (src_addr as usize).saturating_add(section_header.size_of_raw_data as usize)
            > (file.as_ptr() as usize).saturating_add(file.len())
            || (dst_addr as usize).saturating_add(section_header.size_of_raw_data as usize)
                > ((phys_addr as *mut u8) as usize).saturating_add(pages_needed * 4096)
        {
            
                (bs.free_pages)(phys_addr, pages_needed);
            
            return Err(BellowsError::PeParse("Section data out of bounds."));
        }

        // Safety:
        // We have checked the source and destination bounds to prevent buffer overflows.
        // `copy_nonoverlapping` is used to copy the section data.
        unsafe {
            ptr::copy_nonoverlapping(src_addr, dst_addr, section_header.size_of_raw_data as usize);
        }
    }

    let image_base_delta = (phys_addr as u64).wrapping_sub(nt_headers.optional_header.image_base);
    if image_base_delta != 0 {
        let reloc_data_dir = &nt_headers.optional_header.data_directory[5];
        if reloc_data_dir.virtual_address != 0 {
            let reloc_table_ptr =
                unsafe { (phys_addr as *mut u8).add(reloc_data_dir.virtual_address as usize) };
            if (reloc_table_ptr as usize).saturating_add(reloc_data_dir.size as usize)
                > phys_addr.saturating_add(pages_needed * 4096)
            {
                
                    (bs.free_pages)(phys_addr, pages_needed);
            
                return Err(BellowsError::PeParse("Relocation table out of bounds."));
            }

            let mut current_reloc_block_ptr = reloc_table_ptr as *mut ImageBaseRelocation;
            let end_reloc_table_ptr = unsafe { reloc_table_ptr.add(reloc_data_dir.size as usize) };

            // Safety:
            // The loop iterates over relocation blocks. The pointer is advanced by a size
            // provided by the PE file, which is assumed to be correct. The loop terminates
            // when `current_reloc_block_ptr` reaches the end of the relocation table.
            while (current_reloc_block_ptr as *mut u8) < end_reloc_table_ptr {
                let current_reloc_block = unsafe { &*current_reloc_block_ptr };
                let reloc_block_size = current_reloc_block.size_of_block as usize;
                if reloc_block_size == 0 {
                    break;
                }
                let current_reloc_block = unsafe { &*current_reloc_block_ptr };
                let reloc_block_size = current_reloc_block.size_of_block as usize;

                // Safety:
                // We advance the pointer into the block to read the fixup entries.
                // The `add` is safe because we check the pointer bounds against the end of the table.
                let mut fixup_ptr = unsafe {
                    (current_reloc_block_ptr as *mut u8).add(mem::size_of::<ImageBaseRelocation>())
                        as *mut u16
                };
                let end_of_block_ptr =
                    unsafe { (current_reloc_block_ptr as *mut u8).add(reloc_block_size) };

                while (fixup_ptr as *mut u8) < end_of_block_ptr {
                    // Safety:
                    // The pointer `fixup_ptr` is checked against the end of the block.
                    let fixup = unsafe { *fixup_ptr };
                    let fixup_type = (fixup >> 12) as u8;
                    let fixup_offset = (fixup & 0xFFF) as usize;

                    if fixup_type == ImageRelBasedType::Dir64 as u8 {
                        let fixup_address = phys_addr
                            .saturating_add(current_reloc_block.virtual_address as usize)
                            .saturating_add(fixup_offset);
                        if fixup_address.saturating_add(8)
                            > (phys_addr.saturating_add(pages_needed * 4096))
                        {
                            
                                (bs.free_pages)(phys_addr, pages_needed);
                            
                            return Err(BellowsError::PeParse(
                                "Relocation fixup address is out of bounds.",
                            ));
                        }
                        let fixup_address_ptr = fixup_address as *mut u64;
                        // Safety:
                        // The address is validated to be within the allocated memory.
                        // We are performing a 64-bit relocation by adding the image base delta.
                        unsafe {
                            *fixup_address_ptr =
                                (*fixup_address_ptr).wrapping_add(image_base_delta);
                        }
                    } else if fixup_type != ImageRelBasedType::Absolute as u8 {
                        
                            (bs.free_pages)(phys_addr, pages_needed);
                        
                        return Err(BellowsError::PeParse("Unsupported relocation type."));
                    }
                    unsafe {
                        fixup_ptr = fixup_ptr.add(1);
                    }
                }
                
                    current_reloc_block_ptr = end_of_block_ptr as *mut ImageBaseRelocation;
                
            }
        }
    }

    let entry_point_addr =
        phys_addr.saturating_add(nt_headers.optional_header.address_of_entry_point as usize);

    if entry_point_addr >= phys_addr.saturating_add(pages_needed * 4096)
        || entry_point_addr < phys_addr
    {
        
            (bs.free_pages)(phys_addr, pages_needed);
        
        return Err(BellowsError::PeParse(
            "Entry point address is outside allocated memory.",
        ));
    }

    // Safety:
    // We are converting a memory address to a function pointer.
    // The `entry_point_addr` is calculated from the PE headers and has been checked to be
    // within the allocated memory. The `efiapi` calling convention is also assumed.
    let entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> ! =
        unsafe { mem::transmute(entry_point_addr) };

    Ok(entry)
}
