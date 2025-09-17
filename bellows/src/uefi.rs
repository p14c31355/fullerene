// bellows/src/uefi.rs

use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

/// A simple Result type for our bootloader,
/// returning a static string on error.
pub type Result<T> = core::result::Result<T, &'static str>;

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
    _firmware_vendor: *mut u16,
    _firmware_revision: u32,
    _console_in_handle: usize,
    _con_in: *mut c_void,
    _console_out_handle: usize,
    pub con_out: *mut EfiSimpleTextOutput,
    _standard_error_handle: usize,
    _std_err: *mut EfiSimpleTextOutput,
    _runtime_services: *mut c_void,
    pub boot_services: *mut EfiBootServices,
    // The rest of the table is not needed for the bootloader
}

/// Very small subset of Boot Services we call
#[repr(C)]
pub struct EfiBootServices {
    _pad0: [usize; 2],
    /// allocate_pages(AllocateType, MemoryType, Pages, *mut PhysicalAddress) -> EFI_STATUS
    pub allocate_pages: extern "efiapi" fn(usize, EfiMemoryType, usize, *mut usize) -> usize,
    /// free_pages(PhysicalAddress, Pages) -> EFI_STATUS
    pub free_pages: extern "efiapi" fn(usize, usize) -> usize,
    /// get_memory_map(MemoryMapSize, *MemoryMap, *MapKey, *DescriptorSize, *DescriptorVersion) -> EFI_STATUS
    pub get_memory_map:
        extern "efiapi" fn(*mut usize, *mut c_void, *mut usize, *mut usize, *mut u32) -> usize,
    _pad1: [usize; 8],
    /// locate_protocol(ProtocolGUID, Registration, *mut *Interface) -> EFI_STATUS
    pub locate_protocol: extern "efiapi" fn(*const u8, *mut c_void, *mut *mut c_void) -> usize,
    _pad2: [usize; 8],
    /// exit_boot_services(ImageHandle, MapKey) -> EFI_STATUS
    pub exit_boot_services: extern "efiapi" fn(usize, usize) -> usize,
    _pad3: [usize; 4],
    /// install_configuration_table(Guid, *Table) -> EFI_STATUS
    pub install_configuration_table: extern "efiapi" fn(*const u8, *mut c_void) -> usize,
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
    pub open_volume: extern "efiapi" fn(*mut EfiSimpleFileSystem, *mut *mut EfiFile) -> usize,
}

/// GUID for EFI_FILE_INFO protocol
pub const EFI_FILE_INFO_GUID: [u8; 16] = [
    0x0d, 0x95, 0xde, 0x05, 0x93, 0x31, 0xd2, 0x11, 0x8a, 0x41, 0x00, 0xa0, 0xc9, 0x3e, 0xc7, 0xea,
];

/// GUID for EFI_SIMPLE_FILE_SYSTEM_PROTOCOL
pub const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x22, 0x5b, 0x4e, 0x96, 0x59, 0x64, 0xd2, 0x11, 0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// GUID for EFI_GRAPHICS_OUTPUT_PROTOCOL
pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: [u8; 16] = [
    0xde, 0xa9, 0x42, 0x90, 0xdc, 0x23, 0x38, 0x4a, 0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a,
];

#[repr(C)]
pub struct EfiGraphicsOutputProtocol {
    pub query_mode: extern "efiapi" fn(
        *mut EfiGraphicsOutputProtocol,
        u32,
        *mut usize,
        *mut *mut EfiGraphicsOutputModeInformation,
    ) -> usize,
    pub set_mode: extern "efiapi" fn(*mut EfiGraphicsOutputProtocol, u32) -> usize,
    _blt: *mut c_void,
    pub mode: *mut EfiGraphicsOutputProtocolMode,
}

#[repr(C)]
pub struct EfiGraphicsOutputProtocolMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut EfiGraphicsOutputModeInformation,
    pub size_of_info: u64,
    pub frame_buffer_base: usize,
    pub frame_buffer_size: usize,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct EfiGraphicsOutputModeInformation {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
    pub pixel_information: EfiPixelBitmask,
    pub pixels_per_scan_line: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
#[allow(dead_code)]
pub enum EfiGraphicsPixelFormat {
    PixelRedGreenBlueReserved8BitPerColor,
    PixelBlueGreenRedReserved8BitPerColor,
    PixelBitMask,
    PixelBltOnly,
    PixelFormatMax,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct EfiPixelBitmask {
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
}

#[repr(C)]
pub struct EfiFile {
    _revision: u64,
    pub open: extern "efiapi" fn(*mut EfiFile, *mut *mut EfiFile, *const u16, u64, u64) -> usize,
    pub close: extern "efiapi" fn(*mut EfiFile) -> usize,
    _delete: extern "efiapi" fn(*mut EfiFile) -> usize,
    pub read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _write: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _reserved: usize,
    _get_position: extern "efiapi" fn(*mut EfiFile, *mut u64) -> usize,
    _set_position: extern "efiapi" fn(*mut EfiFile, u64) -> usize,
    pub get_info: extern "efiapi" fn(*mut EfiFile, *const u8, *mut usize, *mut c_void) -> usize,
    _set_info: extern "efiapi" fn(*mut EfiFile, *const u8, usize, *mut c_void) -> usize,
    pub flush: extern "efiapi" fn(*mut EfiFile) -> usize,
}

#[repr(C)]
pub struct EfiFileInfo {
    _size: u64,
    pub file_size: u64,
    _physical_size: u64,
    _create_time: u64,
    _last_access_time: u64,
    _modification_time: u64,
    _attribute: u64,
    _file_name: [u16; 1],
}

/// GUID for FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID
pub const FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID: [u8; 16] = [
    0x3c, 0x23, 0x88, 0x3f, 0x27, 0x4d, 0x78, 0x4d, 0x91, 0x2c, 0x73, 0x49, 0x3a, 0x0c, 0x23, 0x75,
];

/// The structure passed from the bootloader to the kernel.
#[repr(C)]
pub struct FullereneFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

/// Print a &str to the UEFI console via SimpleTextOutput (OutputString)
pub fn uefi_print(st: &EfiSystemTable, s: &str) {
    let mut ucs2: Vec<u16> = s.encode_utf16().collect();
    ucs2.push(0);
    // Safety:
    // The EfiSystemTable and its con_out pointer are provided by the UEFI firmware
    // at the bootloader entry point and are assumed to be valid for the duration
    // of boot services.
    // The ucs2 vector is valid and contains a null-terminated UTF-16 string.
    // The call to output_string is safe because we check that con_out is not null
    // and the data is correctly formatted.
    if !st.con_out.is_null() {
        unsafe {
            let _ = ((*st.con_out).output_string)(st.con_out, ucs2.as_ptr());
        }
    }
}
