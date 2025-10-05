// bellows/src/loader/pe.rs

use core::{ffi::c_void, mem, ptr, mem::offset_of};
use alloc::format;
use petroleum::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable};
use x86_64::instructions::port::Port; // Import Port for direct I/O
use core::fmt::{self, Write}; // Import fmt module for format_args! and Write
/// Writes a single byte to the COM1 serial port (0x3F8).
/// This is a very basic, early debug function that doesn't rely on any complex initialization.
fn debug_print_byte(byte: u8) {
    let mut port = Port::new(0x3F8);
    unsafe {
        // Wait until the transmit buffer is empty
        while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
        port.write(byte);
    }
}

/// Writes a string to the COM1 serial port.
fn debug_print_str(s: &str) {
    for byte in s.bytes() {
        debug_print_byte(byte);
    }
}

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

pub fn load_efi_image(
    st: &EfiSystemTable,
    file: &[u8],
) -> petroleum::common::Result<
    extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
> {
    let bs = unsafe { &*st.boot_services };

    // Safety:
    // This is safe because we check the file size to ensure there's enough data
    // to read the headers. The pointer is valid within the bounds of `file`.
    if file.len() < mem::size_of::<ImageDosHeader>() {
        return Err(BellowsError::PeParse("File too small for DOS header."));
    }
    if file.len() < mem::size_of::<ImageDosHeader>() {
        return Err(BellowsError::PeParse("File too small for DOS header."));
    }
    let dos_header_ptr = file.as_ptr() as *const ImageDosHeader;
    let e_magic = unsafe { ptr::read_unaligned(dos_header_ptr as *const u16) };
    if e_magic != 0x5a4d {
        return Err(BellowsError::PeParse("Invalid DOS signature (MZ)."));
    }
    let e_lfanew = unsafe { ptr::read_unaligned((dos_header_ptr as *const u8).add(offset_of!(ImageDosHeader, e_lfanew)) as *const i32) };
    petroleum::println!("DOS header parsed. e_lfanew: {:#x}", e_lfanew);

    let nt_headers_offset = e_lfanew as usize;
    if nt_headers_offset + mem::size_of::<ImageNtHeaders64>() > file.len() {
        return Err(BellowsError::PeParse("Invalid NT headers offset."));
    }
    let nt_headers_ptr = unsafe { file.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64 };
    let optional_header_magic = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header)).add(offset_of!(ImageOptionalHeader64, _magic)) as *const u16) };
    petroleum::println!("NT headers parsed. Optional Header Magic: {:#x}", optional_header_magic);

    if optional_header_magic != 0x20b {
        return Err(BellowsError::PeParse("Invalid PE32+ magic number."));
    }

    let image_size = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header)).add(offset_of!(ImageOptionalHeader64, size_of_image)) as *const u32) } as usize;
    let pages_needed = image_size.div_ceil(4096);
    let mut phys_addr: usize = 0;
    let status = unsafe {
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
    petroleum::println!("Memory allocated for PE image. Phys Addr: {:#x}, Pages: {}", phys_addr, pages_needed);

    let image_base = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header)).add(offset_of!(ImageOptionalHeader64, image_base)) as *const u64) } as usize;
    petroleum::println!("Image Base from header: {:#x}", image_base);

    petroleum::println!("Copying headers...");
    let size_of_headers = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header)).add(offset_of!(ImageOptionalHeader64, _size_of_headers)) as *const u32) } as usize;
    unsafe {
        ptr::copy_nonoverlapping(
            file.as_ptr(),
            phys_addr as *mut u8,
            size_of_headers,
        );
    }
    petroleum::println!("Headers copied.");

    let number_of_sections = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, _file_header)).add(offset_of!(ImageFileHeader, number_of_sections)) as *const u16) } as usize;
    let size_of_optional_header = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, _file_header)).add(offset_of!(ImageFileHeader, size_of_optional_header)) as *const u16) } as usize;

    let section_headers_offset = nt_headers_offset
        + mem::size_of::<u32>() // Signature
        + mem::size_of::<ImageFileHeader>()
        + size_of_optional_header;
    let section_headers_size =
        number_of_sections * mem::size_of::<ImageSectionHeader>();
    if section_headers_offset + section_headers_size > file.len() {
        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
        return Err(BellowsError::PeParse("Section headers out of bounds."));
    }
    petroleum::println!("Section headers offset: {:#x}, size: {}", section_headers_offset, section_headers_size);
    petroleum::println!("Copying sections...");

    for i in 0..number_of_sections {
        let section_header_base_ptr = unsafe {
            file.as_ptr()
                .add(section_headers_offset + i * mem::size_of::<ImageSectionHeader>())
        };
        let virtual_address = unsafe {
            ptr::read_unaligned(
                section_header_base_ptr.add(offset_of!(ImageSectionHeader, virtual_address))
                    as *const u32,
            )
        };
        let size_of_raw_data = unsafe {
            ptr::read_unaligned(
                section_header_base_ptr.add(offset_of!(ImageSectionHeader, size_of_raw_data))
                    as *const u32,
            )
        };
        let pointer_to_raw_data = unsafe {
            ptr::read_unaligned(
                section_header_base_ptr.add(offset_of!(ImageSectionHeader, pointer_to_raw_data))
                    as *const u32,
            )
        };

        petroleum::println!(
            "  Section {}: VirtualAddress={:#x}, SizeOfRawData={:#x}, PointerToRawData={:#x}",
            i,
            virtual_address,
            size_of_raw_data,
            pointer_to_raw_data
        );

        let src_addr = unsafe {
            file.as_ptr()
                .add(pointer_to_raw_data as usize)
        };
        let dst_addr =
            unsafe { (phys_addr as *mut u8).add(virtual_address as usize) };

        if (src_addr as usize).saturating_add(size_of_raw_data as usize)
            > (file.as_ptr() as usize).saturating_add(file.len())
            || (dst_addr as usize).saturating_add(size_of_raw_data as usize)
                > ((phys_addr as *mut u8) as usize).saturating_add(pages_needed * 4096)
        {
            unsafe { (bs.free_pages)(phys_addr, pages_needed) };
            return Err(BellowsError::PeParse("Section data out of bounds."));
        }

        unsafe {
            ptr::copy_nonoverlapping(src_addr, dst_addr, size_of_raw_data as usize);
        }
    }
    petroleum::println!("Sections copied.");

    let image_base_delta = (phys_addr as u64).wrapping_sub(image_base as u64);
    petroleum::println!("Image Base Delta: {:#x}", image_base_delta);

    if image_base_delta != 0 {
        let reloc_data_dir_ptr = unsafe {
            (nt_headers_ptr as *const u8)
                .add(offset_of!(ImageNtHeaders64, optional_header))
                .add(offset_of!(ImageOptionalHeader64, data_directory))
                .add(mem::size_of::<ImageDataDirectory>() * 5) // Index 5 for relocation table
                as *const ImageDataDirectory
        };
        let reloc_data_dir = unsafe { ptr::read_unaligned(reloc_data_dir_ptr) };

        let reloc_virtual_address = reloc_data_dir.virtual_address;
        let reloc_size = reloc_data_dir.size;

        petroleum::println!(
            "Relocation Table: VirtualAddress={:#x}, Size={:#x}",
            reloc_virtual_address,
            reloc_size
        );

        if reloc_virtual_address != 0 {
            let reloc_table_ptr =
                unsafe { (phys_addr as *mut u8).add(reloc_virtual_address as usize) };
            if (reloc_table_ptr as usize).saturating_add(reloc_size as usize)
                > phys_addr.saturating_add(pages_needed * 4096)
            {
                unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                return Err(BellowsError::PeParse("Relocation table out of bounds."));
            }

            let mut current_reloc_block_ptr = reloc_table_ptr as *const ImageBaseRelocation;
            let end_reloc_table_ptr = unsafe { reloc_table_ptr.add(reloc_size as usize) };

            let mut reloc_count = 0;
            while (current_reloc_block_ptr as *const u8) < end_reloc_table_ptr {
                reloc_count += 1;
                if reloc_count > 10000 {
                    unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                    return Err(BellowsError::PeParse("Too many relocations, possible infinite loop."));
                }

                let virtual_address = unsafe {
                    ptr::read_unaligned(
                        (current_reloc_block_ptr as *const u8)
                            .add(offset_of!(ImageBaseRelocation, virtual_address))
                            as *const u32,
                    )
                };
                let reloc_block_size = unsafe {
                    ptr::read_unaligned(
                        (current_reloc_block_ptr as *const u8)
                            .add(offset_of!(ImageBaseRelocation, size_of_block))
                            as *const u32,
                    )
                } as usize;

                if reloc_block_size == 0 {
                    break;
                }

                let mut fixup_ptr = unsafe {
                    (current_reloc_block_ptr as *const u8).add(mem::size_of::<ImageBaseRelocation>())
                        as *const u16
                };
                let end_of_block_ptr =
                    unsafe { (current_reloc_block_ptr as *const u8).add(reloc_block_size) };

                while (fixup_ptr as *const u8) < end_of_block_ptr {
                    let fixup = unsafe { ptr::read_unaligned(fixup_ptr) };
                    let fixup_type = (fixup >> 12) as u8;
                    let fixup_offset = (fixup & 0xFFF) as usize;

                    if fixup_type == ImageRelBasedType::Dir64 as u8 {
                        let fixup_address = phys_addr
                            .saturating_add(virtual_address as usize)
                            .saturating_add(fixup_offset);
                        if fixup_address.saturating_add(8)
                            > (phys_addr.saturating_add(pages_needed * 4096))
                        {
                            unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                            return Err(BellowsError::PeParse(
                                "Relocation fixup address is out of bounds.",
                            ));
                        }
                        let fixup_address_ptr = fixup_address as *mut u64;
                        unsafe {
                            *fixup_address_ptr =
                                (*fixup_address_ptr).wrapping_add(image_base_delta);
                        }
                    } else if fixup_type != ImageRelBasedType::Absolute as u8 {
                        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
                        return Err(BellowsError::PeParse("Unsupported relocation type."));
                    }
                    unsafe {
                        fixup_ptr = fixup_ptr.add(1);
                    }
                }

                current_reloc_block_ptr = end_of_block_ptr as *const ImageBaseRelocation;
            }
            petroleum::println!("Relocations applied.");
        } else {
            petroleum::println!("No relocation table found or virtual address is 0.");
        }
    } else {
        petroleum::println!("Image base delta is 0, no relocations needed.");
    }

    let address_of_entry_point = unsafe { ptr::read_unaligned((nt_headers_ptr as *const u8).add(offset_of!(ImageNtHeaders64, optional_header)).add(offset_of!(ImageOptionalHeader64, address_of_entry_point)) as *const u32) };
    let entry_point_addr =
        phys_addr.saturating_add(address_of_entry_point as usize);
    petroleum::println!("Calculated Entry Point Address: {:#x}", entry_point_addr);

    if entry_point_addr >= phys_addr.saturating_add(pages_needed * 4096)
        || entry_point_addr < phys_addr
    {
        unsafe { (bs.free_pages)(phys_addr, pages_needed) };
        return Err(BellowsError::PeParse(
            "Entry point address is outside allocated memory.",
        ));
    }

    // Debug print just before transmuting to function pointer
    debug_print_str(&format!("Bellows: Transmuting entry point at {:#x}\n", entry_point_addr));

    let entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> ! =
        unsafe { mem::transmute(entry_point_addr) };

    Ok(entry)
}
