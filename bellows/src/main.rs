// bellows/src/main.rs
#![no_std]
#![no_main]
extern crate alloc; // Add this line to import the alloc crate

use core::{ptr, slice};
use core::ffi::c_void;
use alloc::vec::Vec; // Add this line to bring Vec into scope

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

// Minimal UEFI structs and enums
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

#[repr(C)]
pub struct EfiSystemTable {
    _hdr: [u8; 24],
    pub con_out: *mut EfiSimpleTextOutput, // Made public
    _con_in: *mut c_void,
    pub boot_services: *mut EfiBootServices, // Made public
}

#[repr(C)]
pub struct EfiBootServices {
    _pad: [usize; 24],
    pub allocate_pages: extern "efiapi" fn(usize, EfiMemoryType, usize, *mut usize) -> usize, // Made public
    _pad_2: [usize; 5],
    pub locate_protocol: extern "efiapi" fn(
        *const u8,
        *mut c_void,
        *mut *mut c_void,
    ) -> usize, // Made public
}

#[repr(C)]
pub struct EfiSimpleTextOutput {
    _pad: [usize; 4], // Skip the header and QueryMode, SetMode, SetAttribute
    pub output_string: extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> usize,
}

#[repr(C)]
pub struct EfiSimpleFileSystem {
    _revision: u64,
    open_volume: extern "efiapi" fn(
        *mut EfiSimpleFileSystem,
        *mut *mut EfiFile,
    ) -> usize,
}

#[repr(C)]
pub struct EfiFile {
    read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    open: extern "efiapi" fn(
        *mut EfiFile,
        *mut *mut EfiFile,
        *const u16,
        u64,
        u64,
    ) -> usize,
    close: extern "efiapi" fn(*mut EfiFile) -> usize,
    _reserved: [usize; 13],
}

// NEW: Function to print to UEFI console via SimpleTextOutputProtocol
fn uefi_print(st: &EfiSystemTable, s: &str) {
    // Convert Rust string to UEFI's UCS-2 (UTF-16) null-terminated string
    let mut ucs2_str: Vec<u16> = s.encode_utf16().collect();
    ucs2_str.push(0); // Null terminator

    unsafe {
        // Ensure con_out is not null before dereferencing
        if !st.con_out.is_null() {
            ((*st.con_out).output_string)(st.con_out, ucs2_str.as_ptr());
        }
    }
}

// OLD: Removed debug_print that wrote to VGA
// fn debug_print(s: &[u8]) { ... }

// ELF header
#[repr(C)]
struct ElfHeader {
    magic: [u8; 4],
    _rest: [u8; 12],
    entry: u64,
}

// Load kernel ELF
fn load_kernel(kernel: &[u8]) -> Option<extern "C" fn() -> !> {
    if &kernel[0..4] != b"\x7FELF" {
        // Use uefi_print here if you had access to st, or just loop for now
        // uefi_print(st, "Not an ELF file!\n");
        return None;
    }
    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(header.entry) };
    Some(entry)
}

// SimpleFileSystem GUID
const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x95, 0x76, 0x6e, 0x91, 0x3f, 0x6d, 0xd2, 0x11,
    0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b
];

// Read kernel.efi from FAT32
unsafe fn read_kernel(bs: &EfiBootServices) -> &'static [u8] {
    // Locate SimpleFileSystem protocol
    let mut fs_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.locate_protocol)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut fs_ptr,
    );
    // Removed debug_print calls here, as they won't have `st`
    // You might want to add error handling or a way to print here if needed
    if status != 0 {
        loop {} // Or return an error
    }

    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    // Open root volume
    let mut root: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs).open_volume)(fs, &mut root) };
    if status != 0 {
        loop {}
    }

    // Open KERNEL.EFI
    let kernel_name: [u16; 12] = [
        'K' as u16, 'E' as u16, 'R' as u16, 'N' as u16, 'E' as u16, 'L' as u16,
        '.' as u16, 'E' as u16, 'F' as u16, 'I' as u16, 0, 0
    ];
    let mut kernel_file: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*root).open)(root, &mut kernel_file, kernel_name.as_ptr(), 0x1, 0) };
    if status != 0 {
        loop {}
    }

    // Allocate buffer for the kernel
    let mut kernel_pages: usize = 0;
    let size_in_pages = 2 * 1024 * 1024 / 4096; // 2MB in 4KB pages
    let status = (bs.allocate_pages)(
        0, // AllocateAnyPages
        EfiMemoryType::EfiLoaderData,
        size_in_pages,
        &mut kernel_pages,
    );
    if status != 0 {
        loop {}
    }

    let kernel_buf = kernel_pages as *mut u8;
    let mut size: u64 = (size_in_pages * 4096) as u64;

    // Read file
    let status = unsafe { ((*kernel_file).read)(kernel_file, &mut size, kernel_buf) };
    if status != 0 {
        loop {}
    }

    // Close file
    unsafe { ((*kernel_file).close)(kernel_file) };

    unsafe { slice::from_raw_parts(kernel_buf, size as usize) }
}

// Helper: integer to hex (kept for potential future use, not used in this print fix)
fn int_to_hex(mut n: usize) -> [u8; 16] {
    const HEX_CHARS: &[u8] = b"0123456789abcdef";
    let mut buf = [b'0'; 16];
    let mut i = 15;
    if n == 0 { buf[i] = HEX_CHARS[0]; return buf; }
    while n > 0 {
        buf[i] = HEX_CHARS[n % 16];
        n /= 16;
        if i == 0 { break; }
        i -= 1;
    }
    buf
}

// Entry point
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(_image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    // Use the new uefi_print function for bootloader messages
    uefi_print(st, "bellows: bootloader started\n");

    let kernel_image = unsafe { read_kernel(bs) };

    if let Some(kernel_entry) = load_kernel(kernel_image) {
        uefi_print(st, "Jumping to kernel...\n");
        kernel_entry();
    } else {
        uefi_print(st, "Failed to load kernel\n");
    }

    loop {}
}
