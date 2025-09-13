// bellows/src/main.rs
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::c_void;
use core::{ptr, slice};
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

/// ELF header (very small subset)
#[repr(C)]
struct ElfHeader {
    magic: [u8; 4],
    _rest: [u8; 12],
    entry: u64,
}

/// Load kernel ELF and return an entry point function pointer (very naive)
fn load_kernel(kernel: &[u8]) -> Option<extern "C" fn() -> !> {
    if kernel.len() < 24 {
        return None;
    }
    if &kernel[0..4] != b"\x7fELF" {
        return None;
    }
    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(header.entry) };
    Some(entry)
}

/// SimpleFileSystem GUID (EFI_SIMPLE_FILE_SYSTEM_PROTOCOL)
const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x95, 0x76, 0x6e, 0x91, 0x3f, 0x6d, 0xd2, 0x11, 0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// Read `KERNEL.EFI` from the volume using UEFI SimpleFileSystem protocol.
/// Allocates pages for the kernel buffer via BootServices.allocate_pages.
unsafe fn read_kernel(bs: &EfiBootServices) -> Option<&'static [u8]> {
    // locate SimpleFileSystem protocol
    let mut fs_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.locate_protocol)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut fs_ptr,
    );
    if status != 0 {
        return None;
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    // open volume (root)
    let mut root: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs).open_volume)(fs, &mut root) };
    if status != 0 {
        return None;
    }

    // File name must be UCS-2 (UTF-16) and often expected uppercase for many firmwares
    let kernel_name: [u16; 12] = [
        'K' as u16, 'E' as u16, 'R' as u16, 'N' as u16, 'E' as u16, 'L' as u16, '.' as u16,
        'E' as u16, 'F' as u16, 'I' as u16, 0, 0,
    ];
    let mut kernel_file: *mut EfiFile = ptr::null_mut();
    let status = unsafe {
        ((*root).open)(
            root,
            &mut kernel_file,
            kernel_name.as_ptr(),
            0x1, /* READ */
            0,
        )
    };
    if status != 0 {
        return None;
    }

    // Allocate pages for kernel. Use AllocateAnyPages = 0
    // We'll allocate 2 MiB for kernel (adjust if needed)
    let pages = (2 * 1024 * 1024) / 4096;
    let mut phys_addr: usize = 0;
    let status = (bs.allocate_pages)(0usize, EfiMemoryType::EfiLoaderData, pages, &mut phys_addr);
    if status != 0 {
        return None;
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut size: u64 = (pages * 4096) as u64;

    // Read file into allocated buffer
    let status = unsafe { ((*kernel_file).read)(kernel_file, &mut size, buf_ptr) };
    if status != 0 {
        return None;
    }

    // Close the file
    unsafe { ((*kernel_file).close)(kernel_file) };

    Some(unsafe { slice::from_raw_parts(buf_ptr, size as usize) })
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(_image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    // SAFETY: UEFI provides a valid pointer for system_table when called by firmware.
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    // 1) Allocate heap pages and initialize the global allocator.
    //    AllocateAnyPages = 0
    let heap_pages = (HEAP_SIZE + 4095) / 4096;
    let mut heap_phys: usize = 0;
    let status = (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        heap_pages,
        &mut heap_phys,
    );
    if status != 0 {
        // Allocation failed: we cannot continue safely
        loop {}
    }

    unsafe {
        // init allocator with pointer and size in bytes
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }

    // Now we can use alloc-based data structures and printing via uefi_print
    uefi_print(&st, "bellows: bootloader started\n");

    // 2) Read kernel from filesystem
    let kernel_opt = unsafe { read_kernel(bs) };
    let kernel_image = match kernel_opt {
        Some(k) => k,
        None => {
            uefi_print(&st, "bellows: failed to read fullerene.efi\n");
            loop {}
        }
    };

    // 3) Parse ELF and jump to entry
    if let Some(entry) = load_kernel(kernel_image) {
        uefi_print(&st, "bellows: jumping to kernel...\n");
        entry(); // should not return
    } else {
        uefi_print(&st, "bellows: kernel is not a valid ELF\n");
        loop {}
    }
}
