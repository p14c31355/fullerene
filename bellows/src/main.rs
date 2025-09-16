// bellows/src/main.rs
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::c_void;
use core::{mem, ptr, slice};
use linked_list_allocator::LockedHeap;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// A simple Result type for our bootloader,
/// returning a static string on error.
type Result<T> = core::result::Result<T, &'static str>;

/// Minimal subset of UEFI memory types (only those we need)
#[repr(usize)]
pub enum EfiMemoryType {
    EfiLoaderData = 2,
    EfiMaxMemoryType = 15,
}

/// Minimal UEFI System Table and protocols used by this loader
#[repr(C)]
pub struct EfiSystemTable {
    _hdr: [u8; 24],
    pub con_out: *mut EfiSimpleTextOutput,
    _con_in: *mut c_void,
    pub boot_services: *mut EfiBootServices,
}

/// Very small subset of Boot Services we call
#[repr(C)]
pub struct EfiBootServices {
    _pad: [usize; 24],
    /// allocate_pages(AllocateType, MemoryType, Pages, *mut PhysicalAddress) -> EFI_STATUS
    pub allocate_pages: extern "efiapi" fn(usize, EfiMemoryType, usize, *mut usize) -> usize,
    /// free_pages(PhysicalAddress, Pages) -> EFI_STATUS
    pub free_pages: extern "efiapi" fn(usize, usize) -> usize,
    _pad_2: [usize; 4],
    /// locate_protocol(ProtocolGUID, Registration, *mut *Interface) -> EFI_STATUS
    pub locate_protocol: extern "efiapi" fn(*const u8, *mut c_void, *mut *mut c_void) -> usize,
    _pad_3: [usize; 3],
    /// get_memory_map(MemoryMapSize, *MemoryMap, *MapKey, *DescriptorSize, *DescriptorVersion) -> EFI_STATUS
    pub get_memory_map:
        extern "efiapi" fn(*mut usize, *mut c_void, *mut usize, *mut usize, *mut u32) -> usize,
    _pad_4: [usize; 2],
    /// exit_boot_services(ImageHandle, MapKey) -> EFI_STATUS
    pub exit_boot_services: extern "efiapi" fn(usize, usize) -> usize,
}

/// SimpleTextOutput protocol (we only use OutputString)
#[repr(C)]
pub struct EfiSimpleTextOutput {
    _pad: [usize; 4], // skip many fields; we only use output_string
    pub output_string: extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> usize,
}

/// Simple FileSystem and File prototypes (very small subset)
#[repr(C)]
pub struct EfiSimpleFileSystem {
    _revision: u64,
    open_volume: extern "efiapi" fn(*mut EfiSimpleFileSystem, *mut *mut EfiFile) -> usize,
}

/// GUID for EFI_FILE_INFO protocol
const EFI_FILE_INFO_GUID: [u8; 16] = [
    0x0d, 0x95, 0xde, 0x05, 0x93, 0x31, 0xd2, 0x11, 0x8a, 0x41, 0x00, 0xa0, 0xc9, 0x3e, 0xc7, 0xea,
];

#[repr(C)]
pub struct EfiFile {
    _revision: u64,
    open: extern "efiapi" fn(*mut EfiFile, *mut *mut EfiFile, *const u16, u64, u64) -> usize,
    close: extern "efiapi" fn(*mut EfiFile) -> usize,
    _delete: extern "efiapi" fn(*mut EfiFile) -> usize,
    read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _write: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _reserved: usize,
    _get_position: extern "efiapi" fn(*mut EfiFile, *mut u64) -> usize,
    _set_position: extern "efiapi" fn(*mut EfiFile, u64) -> usize,
    get_info: extern "efiapi" fn(*mut EfiFile, *const u8, *mut usize, *mut c_void) -> usize,
    _set_info: extern "efiapi" fn(*mut EfiFile, *const u8, usize, *mut c_void) -> usize,
    flush: extern "efiapi" fn(*mut EfiFile) -> usize,
}

#[repr(C)]
pub struct EfiFileInfo {
    _size: u64,
    file_size: u64,
    _physical_size: u64,
    _create_time: u64,
    _last_access_time: u64,
    _modification_time: u64,
    _attribute: u64,
    _file_name: [u16; 1],
}

/// PE/COFF structures (minimal subset)
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

/// Print a &str to the UEFI console via SimpleTextOutput (OutputString)
fn uefi_print(st: &EfiSystemTable, s: &str) {
    let mut ucs2: Vec<u16> = s.encode_utf16().collect();
    ucs2.push(0);
    unsafe {
        if !st.con_out.is_null() {
            ((*st.con_out).output_string)(st.con_out, ucs2.as_ptr());
        }
    }
}

/// SimpleFileSystem GUID (EFI_SIMPLE_FILE_SYSTEM_PROTOCOL)
const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x95, 0x76, 0x6e, 0x91, 0x3f, 0x6d, 0xd2, 0x11, 0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// Read `KERNEL.EFI` or `kernel.efi` from the volume using UEFI SimpleFileSystem protocol.
unsafe fn read_efi_file(st: &EfiSystemTable) -> Result<(usize, usize)> {
    unsafe {
        let bs = &*st.boot_services;

        let mut fs_ptr: *mut c_void = ptr::null_mut();
        if (bs.locate_protocol)(
            EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
            ptr::null_mut(),
            &mut fs_ptr,
        ) != 0
        {
            return Err("Failed to locate SimpleFileSystem protocol.");
        }
        let fs = fs_ptr as *mut EfiSimpleFileSystem;

        let mut root: *mut EfiFile = ptr::null_mut();
        if ((*fs).open_volume)(fs, &mut root) != 0 {
            return Err("Failed to open volume.");
        }

        let file_names = [
            "KERNEL.EFI\0".encode_utf16().collect::<Vec<u16>>(),
            "kernel.efi\0".encode_utf16().collect::<Vec<u16>>(),
        ];
        let mut efi_file: *mut EfiFile = ptr::null_mut();
        let mut found = false;
        for file_name in &file_names {
            if ((*root).open)(root, &mut efi_file, file_name.as_ptr(), 0x1, 0) == 0 {
                found = true;
                break;
            }
        }
        if !found {
            return Err("Failed to open KERNEL.EFI or kernel.efi.");
        }

        let mut file_info_size: usize = 0;
        ((*efi_file).get_info)(
            efi_file,
            EFI_FILE_INFO_GUID.as_ptr(),
            &mut file_info_size,
            ptr::null_mut(),
        );

        let mut file_info_buf: Vec<u8> = Vec::with_capacity(file_info_size);
        let file_info_ptr = file_info_buf.as_mut_ptr() as *mut c_void;
        if ((*efi_file).get_info)(
            efi_file,
            EFI_FILE_INFO_GUID.as_ptr(),
            &mut file_info_size,
            file_info_ptr,
        ) != 0
        {
            ((*efi_file).close)(efi_file);
            return Err("Failed to get file info.");
        }
        let file_info: &EfiFileInfo = &*(file_info_ptr as *const EfiFileInfo);
        let file_size = file_info.file_size as usize;

        let pages = file_size.div_ceil(4096);
        let mut phys_addr: usize = 0;
        if (bs.allocate_pages)(0usize, EfiMemoryType::EfiLoaderData, pages, &mut phys_addr) != 0 {
            ((*efi_file).close)(efi_file);
            return Err("Failed to allocate pages for kernel file.");
        }

        let buf_ptr = phys_addr as *mut u8;
        let mut read_size: u64 = file_size as u64;
        if ((*efi_file).read)(efi_file, &mut read_size, buf_ptr) != 0 {
            (bs.free_pages)(phys_addr, pages);
            ((*efi_file).close)(efi_file);
            return Err("Failed to read kernel file.");
        }

        ((*efi_file).close)(efi_file);
        Ok((phys_addr, read_size as usize))
    }
}

/// Load an EFI image (PE/COFF file) and return the entry point
unsafe fn load_efi_image(
    st: &EfiSystemTable,
    image_file: &[u8],
) -> Result<extern "efiapi" fn(usize, *mut EfiSystemTable) -> !> {
    unsafe {
        let bs = &*st.boot_services;

        if image_file.len() < mem::size_of::<ImageDosHeader>() {
            return Err("Image file too small for DOS header.");
        }
        let dos_header = ptr::read_unaligned(image_file.as_ptr() as *const ImageDosHeader);
        if dos_header.e_magic != 0x5a4d {
            return Err("Invalid PE/COFF file: Missing DOS header.");
        }

        let pe_header_offset = dos_header.e_lfanew as usize;
        if image_file.len() < pe_header_offset + 4 {
            return Err("Image file too small for PE signature.");
        }
        let pe_signature =
            ptr::read_unaligned(image_file.as_ptr().add(pe_header_offset) as *const u32);
        if pe_signature != 0x00004550 {
            return Err("Invalid PE/COFF file: Missing PE signature.");
        }

        let file_header_ptr = image_file.as_ptr().add(pe_header_offset + 4);
        if image_file.len() < pe_header_offset + 4 + mem::size_of::<ImageFileHeader>() {
            return Err("Image file too small for file header.");
        }
        let file_header = ptr::read_unaligned(file_header_ptr as *const ImageFileHeader);

        let optional_header_ptr = file_header_ptr.add(mem::size_of::<ImageFileHeader>());
        if image_file.len()
            < pe_header_offset
                + 4
                + mem::size_of::<ImageFileHeader>()
                + file_header.size_of_optional_header as usize
        {
            return Err("Image file too small for optional header.");
        }
        let optional_header =
            ptr::read_unaligned(optional_header_ptr as *const ImageOptionalHeader64);

        let image_entry_point_rva = optional_header.address_of_entry_point as usize;
        let preferred_image_base = optional_header.image_base as usize;
        let preferred_image_size = optional_header.size_of_image as usize;

        let pages_needed = preferred_image_size.div_ceil(4096);
        let mut phys_addr: usize = preferred_image_base;
        let status = (bs.allocate_pages)(
            1usize,
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        );
        if status != 0 {
            return Err("Failed to allocate pages for kernel image at preferred address.");
        }
        if phys_addr != preferred_image_base {
            (bs.free_pages)(phys_addr, pages_needed);
            return Err("Allocation did not return preferred address.");
        }

        let image_ptr = phys_addr as *mut u8;
        let headers_size = optional_header._size_of_headers as usize;
        if image_file.len() < headers_size {
            return Err("Image file headers size is invalid.");
        }
        ptr::copy_nonoverlapping(image_file.as_ptr(), image_ptr, headers_size);

        let mut section_header_ptr =
            optional_header_ptr.add(file_header.size_of_optional_header as usize);
        for _ in 0..file_header.number_of_sections as usize {
            let section_header =
                ptr::read_unaligned(section_header_ptr as *const ImageSectionHeader);
            if section_header.size_of_raw_data > 0 {
                let raw_data_ptr = image_file
                    .as_ptr()
                    .add(section_header.pointer_to_raw_data as usize);
                let virtual_address = phys_addr + section_header.virtual_address as usize;
                if image_file.len()
                    < section_header.pointer_to_raw_data as usize
                        + section_header.size_of_raw_data as usize
                {
                    (bs.free_pages)(phys_addr, pages_needed);
                    return Err("Invalid section data size.");
                }
                ptr::copy_nonoverlapping(
                    raw_data_ptr,
                    virtual_address as *mut u8,
                    section_header.size_of_raw_data as usize,
                );
            }
            section_header_ptr = section_header_ptr.add(mem::size_of::<ImageSectionHeader>());
        }

        let reloc_data_dir = &optional_header.data_directory[5];
        if reloc_data_dir.size > 0 {
            let reloc_table_offset = image_file
                .as_ptr()
                .add(reloc_data_dir.virtual_address as usize)
                as *const u8;
            let mut current_reloc_block = reloc_table_offset;
            while (current_reloc_block as usize - reloc_table_offset as usize)
                < reloc_data_dir.size as usize
            {
                let reloc_block_header: &ImageBaseRelocation =
                    &*(current_reloc_block as *const ImageBaseRelocation);
                let reloc_block_size = reloc_block_header.size_of_block as usize;
                let num_entries = (reloc_block_size - mem::size_of::<ImageBaseRelocation>()) / 2;
                let fixup_list_ptr = current_reloc_block.add(mem::size_of::<ImageBaseRelocation>());
                let fixup_list = slice::from_raw_parts(fixup_list_ptr as *const u16, num_entries);
                let offset = phys_addr as u64 - preferred_image_base as u64;
                let reloc_page_va = phys_addr + reloc_block_header.virtual_address as usize;

                for &fixup in fixup_list {
                    let fixup_type = (fixup >> 12) & 0xF;
                    let fixup_offset = fixup & 0xFFF;
                    if fixup_type == 10 {
                        let fixup_address_ptr = (reloc_page_va + fixup_offset as usize) as *mut u64;
                        *fixup_address_ptr = (*fixup_address_ptr).wrapping_add(offset);
                    } else if fixup_type != 0 {
                        (bs.free_pages)(phys_addr, pages_needed);
                        return Err("Unsupported relocation type.");
                    }
                }
                current_reloc_block = current_reloc_block.add(reloc_block_size);
            }
        }

        let entry_point_addr = phys_addr + image_entry_point_rva;
        Ok(mem::transmute(entry_point_addr))
    }
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };
    uefi_print(st, "bellows: bootloader started\n");

    if let Err(msg) = init_heap(bs) {
        uefi_print(st, msg);
        loop {}
    }

    let (efi_image_phys, efi_image_size) = unsafe {
        match read_efi_file(st) {
            Ok(info) => info,
            Err(err) => {
                uefi_print(st, err);
                uefi_print(st, "\nHalting.\n");
                loop {}
            }
        }
    };
    let efi_image_file =
        unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) };

    let entry = unsafe {
        match load_efi_image(st, efi_image_file) {
            Ok(e) => e,
            Err(err) => {
                uefi_print(st, err);
                uefi_print(st, "\nHalting.\n");
                (bs.free_pages)(efi_image_phys, efi_image_size.div_ceil(4096));
                loop {}
            }
        }
    };

    let file_pages = efi_image_size.div_ceil(4096);
    unsafe {
        (bs.free_pages)(efi_image_phys, file_pages);
    }

    uefi_print(st, "bellows: Exiting Boot Services...\n");
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => unreachable!(),
        Err(msg) => {
            uefi_print(st, msg);
            loop {}
        }
    }
    unreachable!();
}

fn init_heap(bs: &EfiBootServices) -> Result<()> {
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    let mut heap_phys: usize = 0;
    let status = (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        heap_pages,
        &mut heap_phys,
    );
    if status != 0 {
        return Err("Failed to allocate heap memory.");
    }
    unsafe { ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE); }
    Ok(())
}

fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable) -> !,
) -> Result<!> {
    let bs = unsafe { &*(*system_table).boot_services };
    let mut map_size = 0;
    let mut map_key = 0;
    let mut descriptor_size = 0;
    let mut descriptor_version = 0;

    let status = (bs.get_memory_map)(
        &mut map_size,
        ptr::null_mut(),
        &mut map_key,
        &mut descriptor_size,
        &mut descriptor_version,
    );
    if status != 0 {
        return Err("Failed to get memory map size.");
    }

    map_size += 4096;
    let map_pages = map_size.div_ceil(4096);
    let mut map_phys_addr: usize = 0;
    let status = (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        map_pages,
        &mut map_phys_addr,
    );
    if status != 0 {
        return Err("Failed to allocate memory map buffer.");
    }

    let map_ptr = map_phys_addr as *mut c_void;
    let status = (bs.get_memory_map)(
        &mut map_size,
        map_ptr,
        &mut map_key,
        &mut descriptor_size,
        &mut descriptor_version,
    );
    if status != 0 {
        (bs.free_pages)(map_phys_addr, map_pages);
        return Err("Failed to get memory map on second attempt.");
    }

    let status = (bs.exit_boot_services)(image_handle, map_key);
    if status != 0 {
        (bs.free_pages)(map_phys_addr, map_pages);
        return Err("Failed to exit boot services.");
    }
    entry(image_handle, system_table);
}
