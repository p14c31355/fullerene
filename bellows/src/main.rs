#![no_std]
#![no_main]

use core::{slice, ptr};
use core::ffi::c_void;

// panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
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

// Minimal UEFI structs
#[repr(C)]
pub struct EfiSystemTable {
    _hdr: [u8; 24],
    con_out: *mut EfiSimpleTextOutput,
    _con_in: *mut c_void,
    boot_services: *mut EfiBootServices,
}

#[repr(C)]
pub struct EfiBootServices {
    _pad: [usize; 24],
    locate_protocol: extern "efiapi" fn(
        *const u8,
        *mut c_void,
        *mut *mut c_void,
    ) -> usize,
}

#[repr(C)]
pub struct EfiSimpleTextOutput;

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

// Simple debug output with basic bounds checking
fn debug_print(s: &[u8]) {
    let vga_buffer = 0xb8000 as *mut u8;
    // Limit the output to prevent writing past the buffer end
    let len = core::cmp::min(s.len(), 80 * 25);
    for (i, &b) in s[..len].iter().enumerate() {
        unsafe {
            let offset = i as isize * 2;
            if offset < (80 * 25) as isize * 2 {
                *vga_buffer.offset(offset) = b;
                *vga_buffer.offset(offset + 1) = 0x0f;
            } else {
                break; // Stop if we reach the end
            }
        }
    }
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
        debug_print(b"Not an ELF file!\n");
        return None;
    }
    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(header.entry) };
    Some(entry)
}

// Read kernel.efi from FAT32
#[allow(static_mut_refs)]
unsafe fn read_kernel(bs: &EfiBootServices) -> &'static [u8] {
    // Locate SimpleFileSystem protocol
    let mut fs_ptr: *mut c_void = ptr::null_mut();
    let sfsp_guid: [u8; 16] = [
        0x10,0x32,0x11,0x3e,0x9e,0x23,0x11,0xd4,0x9a,0x5b,0x00,0x90,0x27,0x3d,0x49,0x38
    ];
    let status = (bs.locate_protocol)(sfsp_guid.as_ptr(), ptr::null_mut(), &mut fs_ptr);
    debug_print(b"locate_protocol status: "); debug_print(&int_to_hex(status)); debug_print(b"\n");
    if status != 0 { loop {} }

    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    // Open root volume
    let mut root: *mut EfiFile = ptr::null_mut();
    let status = ((*fs).open_volume)(fs, &mut root);
    debug_print(b"open_volume status: "); debug_print(&int_to_hex(status)); debug_print(b"\n");
    if status != 0 { loop {} }

    // Open kernel.efi
    let kernel_name: [u16; 11] = [
        'k' as u16,'e' as u16,'r' as u16,'n' as u16,'e' as u16,'l' as u16,
        '.' as u16,'e' as u16,'f' as u16,'i' as u16,0
    ];
    let mut kernel_file: *mut EfiFile = ptr::null_mut();
    let status = ((*root).open)(root, &mut kernel_file, kernel_name.as_ptr(), 0x0000000000000001, 0);
    debug_print(b"open kernel.efi status: "); debug_print(&int_to_hex(status)); debug_print(b"\n");
    if status != 0 { loop {} }

    // Allocate buffer
    static mut KERNEL_BUF: [u8; 1024*1024] = [0; 1024*1024]; // 1MB
    let mut size: u64 = KERNEL_BUF.len() as u64;

    // Read file
    let status = ((*kernel_file).read)(kernel_file, &mut size, KERNEL_BUF.as_mut_ptr());
    debug_print(b"read kernel.efi status: "); debug_print(&int_to_hex(status)); debug_print(b"\n");
    if status != 0 { loop {} }

    // Close file
    ((*kernel_file).close)(kernel_file);

    slice::from_raw_parts(KERNEL_BUF.as_ptr(), size as usize)
}

// Entry point
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(_image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    debug_print(b"bellows: bootloader started\n");

    let kernel_image = unsafe { read_kernel(bs) };

    if let Some(kernel_entry) = load_kernel(kernel_image) {
        debug_print(b"Jumping to kernel...\n");
        kernel_entry();
    } else {
        debug_print(b"Failed to load kernel\n");
    }

    loop {}
}


