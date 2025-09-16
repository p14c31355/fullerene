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
    _pad_2: [usize; 5],
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

#[repr(C)]
pub struct EfiFile {
    read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    open: extern "efiapi" fn(*mut EfiFile, *mut *mut EfiFile, *const u16, u64, u64) -> usize,
    close: extern "efiapi" fn(*mut EfiFile) -> usize,
    _reserved: [usize; 13],
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
    _number_of_sections: u16,
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
    _data_directory: [ImageDataDirectory; 16],
}

/// Print a &str to the UEFI console via SimpleTextOutput (OutputString)
fn uefi_print(st: &EfiSystemTable, s: &str) {
    // Convert to UTF-16 (UCS-2) with NUL terminator
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

/// Read `KERNEL.EFI` from the volume using UEFI SimpleFileSystem protocol.
/// Allocates pages for the kernel buffer via BootServices.allocate_pages.
unsafe fn read_efi_file(bs: &EfiBootServices) -> Option<&'static [u8]> {
    // locate SimpleFileSystem protocol
    let mut fs_ptr: *mut c_void = ptr::null_mut();
    if (bs.locate_protocol)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut fs_ptr,
    ) != 0
    {
        return None;
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    let mut root: *mut EfiFile = ptr::null_mut();
    if ((*fs).open_volume)(fs, &mut root) != 0 {
        return None;
    }

    let file_name: [u16; 11] = [
        'K' as u16, 'E' as u16, 'R' as u16, 'N' as u16, 'E' as u16, 'L' as u16, '.' as u16,
        'E' as u16, 'F' as u16, 'I' as u16, 0,
    ];

    let mut efi_file: *mut EfiFile = ptr::null_mut();
    if ((*root).open)(root, &mut efi_file, file_name.as_ptr(), 0x1, 0) != 0 {
        return None;
    }

    let pages = (2 * 1024 * 1024) / 4096;
    let mut phys_addr: usize = 0;
    if (bs.allocate_pages)(0usize, EfiMemoryType::EfiLoaderData, pages, &mut phys_addr) != 0 {
        return None;
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut size: u64 = (pages * 4096) as u64;

    if ((*efi_file).read)(efi_file, &mut size, buf_ptr) != 0 {
        return None;
    }

    ((*efi_file).close)(efi_file);

    Some(slice::from_raw_parts(buf_ptr, size as usize))
}

/// Load an EFI image (PE/COFF file) and return the entry point
fn load_efi_image(
    bs: &EfiBootServices,
    image: &[u8],
) -> Option<extern "efiapi" fn(usize, *mut EfiSystemTable) -> !> {
    unsafe {
        // Check for DOS header signature 'MZ'
        let dos_header = &*(image.as_ptr() as *const ImageDosHeader);
        if dos_header.e_magic != 0x5a4d {
            return None;
        }

        // Check for PE signature 'PE\0\0'
        let pe_header_offset = dos_header.e_lfanew as usize;
        let pe_signature = &*(image.as_ptr().add(pe_header_offset) as *const u32);
        if *pe_signature != 0x00004550 {
            return None;
        }

        let file_header_ptr = image.as_ptr().add(pe_header_offset + 4);
        let file_header = &*(file_header_ptr as *const ImageFileHeader);
        let optional_header_ptr = file_header_ptr.add(mem::size_of::<ImageFileHeader>());

        // Check if it's a 64-bit optional header
        if file_header.size_of_optional_header < mem::size_of::<ImageOptionalHeader64>() as u16 {
            return None;
        }
        let optional_header = &*(optional_header_ptr as *const ImageOptionalHeader64);

        let image_base_addr = optional_header.image_base;
        let image_size = optional_header.size_of_image as usize;
        let entry_point_rva = optional_header.address_of_entry_point as usize;

        // Allocate memory for the image
        let pages_needed = (image_size + 4095) / 4096;
        let mut phys_addr: usize = image_base_addr as usize;
        if (bs.allocate_pages)(
            1usize,
            EfiMemoryType::EfiLoaderData,
            pages_needed,
            &mut phys_addr,
        ) != 0
        {
            return None;
        }

        let image_ptr = phys_addr as *mut u8;

        // Copy the headers
        ptr::copy_nonoverlapping(
            image.as_ptr(),
            image_ptr,
            optional_header._size_of_headers as usize,
        );

        // This is a simplified loader. For a complete solution, we would iterate through sections
        // and copy each section to its correct virtual address (relative to the image base).
        // Since we are loading at the preferred base address, a simple copy is sufficient for this example.
        // A full loader would also need to handle relocations.

        let entry_point_addr = phys_addr + entry_point_rva;
        Some(mem::transmute(entry_point_addr))
    }
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    // SAFETY: UEFI provides a valid pointer for system_table when called by firmware.
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    // 1) Allocate heap pages and initialize the global allocator.
    let heap_pages = (HEAP_SIZE + 4095) / 4096;
    let mut heap_phys: usize = 0;
    if (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        heap_pages,
        &mut heap_phys,
    ) != 0
    {
        loop {}
    }

    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }

    // Now we can use alloc-based data structures and printing via uefi_print
    uefi_print(&st, "bellows: bootloader started\n");

    // Change: Reading KERNEL.EFI instead of BOOTX64.EFI
    let efi_image_file = unsafe { read_efi_file(bs) }.unwrap_or_else(|| {
        uefi_print(&st, "bellows: failed to read KERNEL.EFI\n");
        loop {}
    });

    let entry = load_efi_image(bs, efi_image_file).unwrap_or_else(|| {
        uefi_print(&st, "bellows: KERNEL.EFI is not a valid PE/COFF image\n");
        loop {}
    });

    uefi_print(&st, "bellows: Exiting Boot Services...\n");

    // Get Memory Map and Exit Boot Services
    let mut map_size = 0;
    let mut map_key = 0;
    let mut descriptor_size = 0;
    let mut descriptor_version = 0;

    // First call to get the required buffer size
    unsafe {
        (bs.get_memory_map)(
            &mut map_size,
            ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );
    }

    // Add a buffer for potential map size changes
    map_size += 4096;
    let map_pages = (map_size + 4095) / 4096;
    let mut map_phys_addr: usize = 0;
    unsafe {
        if (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            map_pages,
            &mut map_phys_addr,
        ) != 0
        {
            loop {}
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    // Second call to get the actual memory map
    unsafe {
        if (bs.get_memory_map)(
            &mut map_size,
            map_ptr,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        ) != 0
        {
            loop {}
        }
    }

    // Exit Boot Services
    unsafe {
        if (bs.exit_boot_services)(image_handle, map_key) != 0 {
            loop {}
        }
    }

    // Now we are in the kernel environment, without UEFI services.
    // Jump to the kernel entry point.
    // The kernel is responsible for setting up its own environment (GDT, IDT, etc.).
    entry(image_handle, system_table); // should not return
}
