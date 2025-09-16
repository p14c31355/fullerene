// bellows/src/main.rs
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

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

/// Minimal subset of UEFI memory types (only those we need)
#[repr(usize)]
pub enum EfiMemoryType {
    EfiReservedMemoryType = 0,
    EfiLoaderCode = 1,
    EfiLoaderData = 2,
    EfiBootServicesCode = 3,
    EfiBootServicesData = 4,
    EfiRuntimeServicesCode = 5,
    EfiRuntimeServicesData = 6,
    EfiConventionalMemory = 7,
    EfiUnusableMemory = 8,
    EfiACPIReclaimMemory = 9,
    EfiACPIMemoryNVS = 10,
    EfiMemoryMappedIO = 11,
    EfiMemoryMappedIOPortSpace = 12,
    EfiPalCode = 13,
    EfiPersistentMemory = 14,
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
    delete: extern "efiapi" fn(*mut EfiFile) -> usize,
    read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    write: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _reserved: usize,
    get_position: extern "efiapi" fn(*mut EfiFile, *mut u64) -> usize,
    set_position: extern "efiapi" fn(*mut EfiFile, u64) -> usize,
    get_info: extern "efiapi" fn(*mut EfiFile, *const u8, *mut usize, *mut c_void) -> usize,
    set_info: extern "efiapi" fn(*mut EfiFile, *const u8, usize, *mut c_void) -> usize,
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
    e_magic: u16, // Magic number
    _pad: [u8; 58],
    e_lfanew: i32, // File address of new exe header
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
    _virtual_address: u32,
    _size: u32,
}

#[repr(C, packed)]
struct ImageOptionalHeader64 {
    _magic: u16,
    _major_linker_version: u8,
    _minor_linker_version: u8,
    _size_of_code: u32,
    size_of_initialized_data: u32,
    size_of_uninitialized_data: u32,
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
    _data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
struct ImageSectionHeader {
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
unsafe fn read_efi_file(st: &EfiSystemTable) -> Option<(usize, usize)> {
    let bs = &*st.boot_services;

    let mut fs_ptr: *mut c_void = ptr::null_mut();
    if (bs.locate_protocol)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut fs_ptr,
    ) != 0
    {
        uefi_print(st, "Failed to locate SimpleFileSystem protocol.\n");
        return None;
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    let mut root: *mut EfiFile = ptr::null_mut();
    if ((*fs).open_volume)(fs, &mut root) != 0 {
        uefi_print(st, "Failed to open volume.\n");
        return None;
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
        uefi_print(st, "Failed to open KERNEL.EFI or kernel.efi.\n");
        return None;
    }

    // Get file size
    let mut file_info_size: usize = 0;
    // First call to get the required buffer size
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
        uefi_print(st, "Failed to get file info.\n");
        ((*efi_file).close)(efi_file);
        return None;
    }

    let file_info: &EfiFileInfo = &*(file_info_ptr as *const EfiFileInfo);
    let file_size = file_info.file_size as usize;

    // Allocate buffer for file content based on actual size
    let pages = (file_size + 4095) / 4096;
    let mut phys_addr: usize = 0;
    if (bs.allocate_pages)(0usize, EfiMemoryType::EfiLoaderData, pages, &mut phys_addr) != 0 {
        uefi_print(st, "Failed to allocate pages for kernel file.\n");
        ((*efi_file).close)(efi_file);
        return None;
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size: u64 = file_size as u64;

    if ((*efi_file).read)(efi_file, &mut read_size, buf_ptr) != 0 {
        uefi_print(st, "Failed to read kernel file.\n");
        (bs.free_pages)(phys_addr, pages);
        ((*efi_file).close)(efi_file);
        return None;
    }

    ((*efi_file).close)(efi_file);

    Some((phys_addr, read_size as usize))
}

/// Load an EFI image (PE/COFF file) and return the entry point
fn load_efi_image(
    st: &EfiSystemTable,
    image_base_addr: usize,
    image_size: usize,
    image_file: &[u8],
) -> Option<extern "efiapi" fn(usize, *mut EfiSystemTable) -> !> {
    unsafe {
        let bs = &*st.boot_services;

        // Parse PE/COFF headers
        let dos_header = ptr::read_unaligned(image_file.as_ptr() as *const ImageDosHeader);
        if dos_header.e_magic != 0x5a4d {
            uefi_print(st, "Invalid PE/COFF file: Missing DOS header.\n");
            return None;
        }

        let pe_header_offset = dos_header.e_lfanew as usize;
        let pe_signature =
            ptr::read_unaligned(image_file.as_ptr().add(pe_header_offset) as *const u32);
        if pe_signature != 0x00004550 {
            uefi_print(st, "Invalid PE/COFF file: Missing PE signature.\n");
            return None;
        }

        let file_header_ptr = image_file.as_ptr().add(pe_header_offset + 4);
        let file_header = ptr::read_unaligned(file_header_ptr as *const ImageFileHeader);

        let optional_header_ptr = file_header_ptr.add(mem::size_of::<ImageFileHeader>());
        let optional_header =
            ptr::read_unaligned(optional_header_ptr as *const ImageOptionalHeader64);

        let image_entry_point_rva = optional_header.address_of_entry_point as usize;
        let preferred_image_base = optional_header.image_base as usize;
        let preferred_image_size = optional_header.size_of_image as usize;

        // Allocate memory for the image at its preferred base address
        let pages_needed = (preferred_image_size + 4095) / 4096;
        let mut phys_addr: usize = preferred_image_base;

        if (bs.allocate_pages)(
            1usize, // Allocate at a specific address (AllocateAddress)
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        ) != 0
        {
            uefi_print(
                st,
                "Failed to allocate pages for kernel image at preferred address.\n",
            );
            return None;
        }

        // Zero out the allocated memory region for BSS sections
        let image_ptr = phys_addr as *mut u8;
        ptr::write_bytes(image_ptr, 0, preferred_image_size);

        // Copy the headers
        let headers_size = optional_header._size_of_headers as usize;
        ptr::copy_nonoverlapping(image_file.as_ptr(), image_ptr, headers_size);

        // Iterate through sections and copy them
        let mut section_header_ptr =
            optional_header_ptr.add(file_header.size_of_optional_header as usize);
        for _ in 0..file_header.number_of_sections as usize {
            let section_header =
                ptr::read_unaligned(section_header_ptr as *const ImageSectionHeader);

            // Check if section has raw data
            if section_header.size_of_raw_data > 0 {
                let raw_data_ptr = image_file
                    .as_ptr()
                    .add(section_header.pointer_to_raw_data as usize);
                let virtual_address = phys_addr + section_header.virtual_address as usize;

                ptr::copy_nonoverlapping(
                    raw_data_ptr,
                    virtual_address as *mut u8,
                    section_header.size_of_raw_data as usize,
                );
            }
            section_header_ptr = section_header_ptr.add(mem::size_of::<ImageSectionHeader>());
        }

        let entry_point_addr = phys_addr + image_entry_point_rva;
        Some(mem::transmute(entry_point_addr))
    }
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    uefi_print(&st, "bellows: bootloader started\n");

    // 1) Allocate heap pages and initialize the global allocator.
    let heap_pages = (HEAP_SIZE + 4095) / 4096;
    let mut heap_phys: usize = 0;
    if unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            heap_pages,
            &mut heap_phys,
        )
    } != 0
    {
        uefi_print(&st, "Failed to allocate heap memory.\n");
        loop {}
    }

    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }

    // 2) Read KERNEL.EFI into a buffer
    let (efi_image_phys, efi_image_size) = match unsafe { read_efi_file(&st) } {
        Some(info) => info,
        None => {
            uefi_print(&st, "Failed to read kernel file. Halting.\n");
            loop {}
        }
    };
    let efi_image_file =
        unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) };

    // 3) Parse PE/COFF header and get image properties
    let (image_base_addr, image_size, entry_point_rva) = unsafe {
        let dos_header = ptr::read_unaligned(efi_image_file.as_ptr() as *const ImageDosHeader);
        let pe_header_offset = dos_header.e_lfanew as usize;
        let file_header_ptr = efi_image_file.as_ptr().add(pe_header_offset + 4);
        let optional_header_ptr = file_header_ptr.add(mem::size_of::<ImageFileHeader>());
        let optional_header =
            ptr::read_unaligned(optional_header_ptr as *const ImageOptionalHeader64);

        (
            optional_header.image_base as usize,
            optional_header.size_of_image as usize,
            optional_header.address_of_entry_point as usize,
        )
    };

    // 4) Load the image into its preferred location
    let entry = match load_efi_image(&st, image_base_addr, image_size, efi_image_file) {
        Some(e) => e,
        None => {
            uefi_print(
                &st,
                "Failed to load the PE/COFF image into memory. Halting.\n",
            );
            unsafe {
                (bs.free_pages)(efi_image_phys, (efi_image_size + 4095) / 4096);
            }
            loop {}
        }
    };

    // 5) Free the file buffer
    let file_pages = (efi_image_size + 4095) / 4096;
    unsafe {
        (bs.free_pages)(efi_image_phys, file_pages);
    }

    uefi_print(&st, "bellows: Exiting Boot Services...\n");

    // 6) Get Memory Map and Exit Boot Services
    let mut map_size = 0;
    let mut map_key = 0;
    let mut descriptor_size = 0;
    let mut descriptor_version = 0;

    // First call to get the required buffer size
    unsafe {
        let status = (bs.get_memory_map)(
            &mut map_size,
            ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );
        if status != 0 {
            uefi_print(&st, "Failed to get memory map size.\n");
            loop {}
        }
    }

    map_size += 4096;
    let map_pages = (map_size + 4095) / 4096;
    let mut map_phys_addr: usize = 0;
    if unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            map_pages,
            &mut map_phys_addr,
        )
    } != 0
    {
        uefi_print(&st, "Failed to allocate memory map buffer.\n");
        loop {}
    }

    let map_ptr = map_phys_addr as *mut c_void;

    if unsafe {
        (bs.get_memory_map)(
            &mut map_size,
            map_ptr,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    } != 0
    {
        uefi_print(&st, "Failed to get memory map on second attempt.\n");
        unsafe {
            (bs.free_pages)(map_phys_addr, map_pages);
        }
        loop {}
    }

    if unsafe { (bs.exit_boot_services)(image_handle, map_key) } != 0 {
        uefi_print(&st, "Failed to exit boot services.\n");
        unsafe {
            (bs.free_pages)(map_phys_addr, map_pages);
        }
        loop {}
    }

    // Now we are in the kernel environment, without UEFI services.
    // The kernel is responsible for setting up its own environment.
    entry(image_handle, system_table); // should not return
}
