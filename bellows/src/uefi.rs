// bellows/src/uefi.rs

use alloc::vec::Vec;
use core::ffi::c_void;

#[repr(usize)]
#[derive(PartialEq)]
pub enum EfiStatus {
    Success = 0,
    LoadError = 1,
    InvalidParameter = 2,
    Unsupported = 3,
    BadBufferSize = 4,
    BufferTooSmall = 5,
    NotInReadyState = 6,
    DeviceError = 7,
    EndOfMedia = 8,
    NotFound = 9,
    AccessDenied = 10,
    NoResponse = 11,
    NoMapping = 12,
    Timeout = 13,
    NotStarted = 14,
    AlreadyStarted = 15,
    Aborted = 21,
    IcalFailed = 26,
}

impl From<usize> for EfiStatus {
    fn from(status: usize) -> Self {
        match status {
            0 => EfiStatus::Success,
            1 => EfiStatus::LoadError,
            2 => EfiStatus::InvalidParameter,
            3 => EfiStatus::Unsupported,
            4 => EfiStatus::BadBufferSize,
            5 => EfiStatus::BufferTooSmall,
            6 => EfiStatus::NotInReadyState,
            7 => EfiStatus::DeviceError,
            8 => EfiStatus::EndOfMedia,
            9 => EfiStatus::NotFound,
            10 => EfiStatus::AccessDenied,
            11 => EfiStatus::NoResponse,
            12 => EfiStatus::NoMapping,
            13 => EfiStatus::Timeout,
            14 => EfiStatus::NotStarted,
            15 => EfiStatus::AlreadyStarted,
            21 => EfiStatus::Aborted,
            26 => EfiStatus::IcalFailed,
            _ => panic!("Unknown EFI status code: {}", status),
        }
    }
}

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
    _pad1: [usize; 2],
    /// get_memory_map(*mut MapSize, *mut MemoryMap, *mut MapKey, *mut DescriptorSize, *mut DescriptorVersion) -> EFI_STATUS
    pub get_memory_map:
        extern "efiapi" fn(*mut usize, *mut c_void, *mut usize, *mut usize, *mut u32) -> usize,
    _pad2: [usize; 2],
    /// exit_boot_services(ImageHandle, MapKey) -> EFI_STATUS
    pub exit_boot_services: extern "efiapi" fn(usize, usize) -> usize,
    _pad3: [usize; 1],
    /// locate_protocol(Protocol, Registration, *mut Interface) -> EFI_STATUS
    pub locate_protocol: extern "efiapi" fn(*const u8, *mut c_void, *mut *mut c_void) -> usize,
    _pad4: [usize; 3],
    /// install_configuration_table(Guid, Table) -> EFI_STATUS
    pub install_configuration_table: extern "efiapi" fn(*const u8, *mut c_void) -> usize,
}

/// Minimal UEFI Simple Text Output Protocol
#[repr(C)]
pub struct EfiSimpleTextOutput {
    _pad: [usize; 2],
    /// output_string(This, *mut u16) -> EFI_STATUS
    pub output_string: extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> usize,
}

/// GUID for EFI_SIMPLE_FILE_SYSTEM_PROTOCOL
pub const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x96, 0x4e, 0x5b, 0x09, 0x21, 0x42, 0x06, 0x4f, 0x85, 0x3d, 0x05, 0x22, 0x22, 0x0b, 0xa2, 0x19,
];

pub const EFI_FILE_INFO_GUID: [u8; 16] = [
    0x0d, 0x95, 0xde, 0x05, 0x93, 0x31, 0xd2, 0x11, 0x8a, 0x41, 0x00, 0xa0, 0xc9, 0x3e, 0xc7, 0xea,
];


/// GUID for EFI_GRAPHICS_OUTPUT_PROTOCOL
pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: [u8; 16] = [
    0xde, 0xa3, 0x93, 0x90, 0x38, 0x42, 0x5f, 0x47, 0x94, 0x01, 0x7d, 0xe7, 0xe5, 0x15, 0x21, 0xde,
];

/// A minimal subset of EFI_FILE_PROTOCOL
#[repr(C)]
pub struct EfiFile {
    _pad0: [usize; 3],
    /// open(This, *mut EfiFile, *mut u16, OpenMode, Attributes) -> EFI_STATUS
    pub open: extern "efiapi" fn(*mut EfiFile, *mut *mut EfiFile, *const u16, u64, u64) -> usize,
    /// close(This) -> EFI_STATUS
    pub close: extern "efiapi" fn(*mut EfiFile) -> usize,
    _pad1: [usize; 1],
    /// read(This, *mut ReadSize, *mut Buffer) -> EFI_STATUS
    pub read: extern "efiapi" fn(*mut EfiFile, *mut u64, *mut u8) -> usize,
    _pad2: [usize; 2],
    /// get_info(This, *const Guid, *mut BufferSize, *mut Buffer) -> EFI_STATUS
    pub get_info: extern "efiapi" fn(*mut EfiFile, *const u8, *mut usize, *mut c_void) -> usize,
}

/// Minimal EFI_SIMPLE_FILE_SYSTEM_PROTOCOL
#[repr(C)]
pub struct EfiSimpleFileSystem {
    _pad: [usize; 1],
    /// open_volume(This, *mut EfiFile) -> EFI_STATUS
    pub open_volume: extern "efiapi" fn(*mut EfiSimpleFileSystem, *mut *mut EfiFile) -> usize,
}

/// Minimal EFI_GRAPHICS_OUTPUT_PROTOCOL
#[repr(C)]
pub struct EfiGraphicsOutputProtocol {
    _pad: [usize; 3],
    pub mode: *mut EfiGraphicsOutputProtocolMode,
}

/// Minimal EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE
#[repr(C)]
pub struct EfiGraphicsOutputProtocolMode {
    _pad: [usize; 2],
    pub info: *mut EfiGraphicsOutputModeInformation,
    pub size_of_info: usize,
    _pad2: [usize; 1],
    pub frame_buffer_base: u64,
    pub frame_buffer_size: u64,
}

/// Minimal EFI_GRAPHICS_OUTPUT_MODE_INFORMATION
#[repr(C)]
pub struct EfiGraphicsOutputModeInformation {
    _version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
    _pad: [u8; 12],
    pub pixels_per_scan_line: u32,
}

/// Minimal EFI_GRAPHICS_PIXEL_FORMAT
#[repr(u32)]
#[derive(Clone, Copy)] // Add Clone and Copy traits
pub enum EfiGraphicsPixelFormat {
    PixelRedGreenBlueReserved8BitPerColor = 0,
    PixelBlueGreenRedReserved8BitPerColor = 1,
    PixelBitMask = 2,
    PixelBltOnly = 3,
    PixelFormatMax = 4,
}

/// Minimal EFI_FILE_INFO
#[repr(C, packed)]
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

/// EFI_STATUS code for EFI_BUFFER_TOO_SMALL
pub const EFI_BUFFER_TOO_SMALL: usize = 0x8000000000000005;

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
    if st.con_out.is_null() {
        return;
    }

    // Allocate a buffer for the UTF-16 string, including the null terminator.
    let mut s_utf16: Vec<u16> = s.encode_utf16().collect();
    s_utf16.push(0); // Add null terminator

    // Safety:
    // We check that `st.con_out` is not null. The `output_string` function
    // expects a valid, null-terminated UTF-16 string pointer. We have created
    // this buffer and ensure it has a null terminator. `as_ptr` is safe on a `Vec`.
    unsafe {
        ((*st.con_out).output_string)(st.con_out, s_utf16.as_ptr());
    }
}
