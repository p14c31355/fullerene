#![no_std]
#![no_main]

use core::{ptr, slice};

// panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

// VGA debug
fn vga_print(s: &[u8]) {
    let vga_buffer = 0xb8000 as *mut u8;
    for (i, &b) in s.iter().enumerate() {
        unsafe {
            *vga_buffer.offset(i as isize * 2) = b;
            *vga_buffer.offset(i as isize * 2 + 1) = 0x0f;
        }
    }
}

// Helper: integer to hex
fn int_to_hex(mut n: usize) -> [u8; 16] {
    const HEX_CHARS: &[u8] = b"0123456789abcdef";
    let mut buf = [b'0'; 16];
    let mut i = 15;
    if n == 0 { buf[i] = HEX_CHARS[0]; return buf; }
    while n > 0 && i > 0 {
        buf[i] = HEX_CHARS[n % 16];
        n /= 16;
        i -= 1;
    }
    buf
}

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
        vga_print(b"Not an ELF file!\n");
        return None;
    }
    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(header.entry) };
    Some(entry)
}

// Minimal UEFI structs
#[repr(C)]
struct EfiSystemTable {
    _hdr: [u8; 24],
    _con_in: *mut (),
    con_out: *mut (),
    boot_services: *mut EfiBootServices,
}

#[repr(C)]
struct EfiBootServices {
    _pad: [u8; 24],
    locate_protocol: extern "efiapi" fn(
        *const u8,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> usize,
}

#[repr(C)]
struct EfiSimpleFileSystem {
    _revision: u64,
    open_volume: extern "efiapi" fn(
        *mut EfiSimpleFileSystem,
        *mut *mut EfiFile,
    ) -> usize,
}

#[repr(C)]
struct EfiFile {
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

unsafe extern "C" {
    unsafe static EFI_SYSTEM_TABLE: EfiSystemTable;
}

// Read kernel.efi from FAT32
#[allow(static_mut_refs)]
unsafe fn read_kernel() -> &'static [u8] {
    let bs = &*EFI_SYSTEM_TABLE.boot_services;

    // Locate SimpleFileSystem protocol
    let mut fs_ptr: *mut core::ffi::c_void = ptr::null_mut();
    let sfsp_guid: [u8; 16] = [
        0x10,0x32,0x11,0x3e,0x9e,0x23,0x11,0xd4,0x9a,0x5b,0x00,0x90,0x27,0x3d,0x49,0x38
    ];
    let status = (bs.locate_protocol)(sfsp_guid.as_ptr(), ptr::null_mut(), &mut fs_ptr);
    vga_print(b"locate_protocol: "); vga_print(&int_to_hex(status)); vga_print(b"\n");
    if status != 0 { vga_print(b"Error: locate_protocol failed\n"); loop {} }

    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    // Open root volume
    let mut root: *mut EfiFile = ptr::null_mut();
    let status = ((*fs).open_volume)(fs, &mut root);
    vga_print(b"open_volume: "); vga_print(&int_to_hex(status)); vga_print(b"\n");
    if status != 0 { vga_print(b"Error: open_volume failed\n"); loop {} }

    // Open kernel.efi
    let kernel_name: [u16; 11] = [
        'k' as u16,'e' as u16,'r' as u16,'n' as u16,'e' as u16,'l' as u16,
        '.' as u16,'e' as u16,'f' as u16,'i' as u16,0
    ];
    let mut kernel_file: *mut EfiFile = ptr::null_mut();
    let status = ((*root).open)(root, &mut kernel_file, kernel_name.as_ptr(), 0x0000000000000001, 0);
    vga_print(b"open kernel.efi: "); vga_print(&int_to_hex(status)); vga_print(b"\n");
    if status != 0 { vga_print(b"Error: open kernel.efi failed\n"); loop {} }

    // Allocate buffer
    static mut KERNEL_BUF: [u8; 1024*1024] = [0; 1024*1024]; // 1MB
    let mut size: u64 = KERNEL_BUF.len() as u64;

    // Read file
    let status = ((*kernel_file).read)(kernel_file, &mut size, KERNEL_BUF.as_mut_ptr());
    vga_print(b"read kernel.efi: "); vga_print(&int_to_hex(status)); vga_print(b"\n");
    if status != 0 { vga_print(b"Error: read kernel.efi failed\n"); loop {} }

    // Close file
    ((*kernel_file).close)(kernel_file);

    slice::from_raw_parts(KERNEL_BUF.as_ptr(), size as usize)
}

// Entry point
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    vga_print(b"bellows: bootloader started\n");

    let kernel_image = unsafe { read_kernel() };

    if let Some(kernel_entry) = load_kernel(kernel_image) {
        vga_print(b"Jumping to kernel...\n");
        kernel_entry();
    } else {
        vga_print(b"Failed to load kernel\n");
    }

    loop {}
}
