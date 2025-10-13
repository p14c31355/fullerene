// Common definitions for UEFI and BIOS modes.

use core::ffi::c_void;

/// GUID for FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID (UEFI only)
pub const FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID: [u8; 16] = [
    0x3c, 0x23, 0x88, 0x3f, 0x27, 0x4d, 0x78, 0x4d, 0x91, 0x2c, 0x73, 0x49, 0x3a, 0x0c, 0x23, 0x75,
];

/// GUID for FULLERENE_MEMORY_MAP_CONFIG_TABLE_GUID (UEFI only)
pub const FULLERENE_MEMORY_MAP_CONFIG_TABLE_GUID: [u8; 16] = [
    0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
];

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EfiGraphicsPixelFormat {
    PixelRedGreenBlueReserved8BitPerColor = 0,
    PixelBlueGreenRedReserved8BitPerColor = 1,
    PixelBitMask = 2,
    PixelBltOnly = 3,
    PixelFormatMax = 4,
}

/// The structure passed from the bootloader to the kernel (UEFI).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FullereneFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
    pub bpp: u32, // Bits per pixel
    pub stride: u32,
}

/// The structure passed from the bootloader to the kernel (UEFI) for memory map.
#[repr(C)]
pub struct FullereneMemoryMap {
    pub physical_address: u64,
    pub size: usize,
}

#[repr(usize)]
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum EfiStatus {
    Success = 0,
    LoadError = 1,
    InvalidParameter = 2,
    Unsupported = 3,
    BadBufferSize = 4,
    BufferTooSmall = 5,
    NotInReadyState = 6,
    DeviceError = 7,
    WriteProtected = 8,
    OutOfResources = 9,
    VolumeCorrupted = 10,
    VolumeFull = 11,
    NoMedia = 12,
    MediaChanged = 13,
    NotFound = 14,
    AccessDenied = 15,
    NoResponse = 16,
    NoMapping = 17,
    Timeout = 18,
    NotStarted = 19,
    AlreadyStarted = 20,
    Aborted = 21,
    IcalFailed = 22,
    // ... more can be added as needed
}

impl From<usize> for EfiStatus {
    fn from(value: usize) -> Self {
        // EFI status: bit 63 set for errors, clear code bits for specific errors
        let code = value & 0x7FFFFFFFFFFFFFFF;
        match code {
            0 => EfiStatus::Success,
            1 => EfiStatus::LoadError,
            2 => EfiStatus::InvalidParameter,
            3 => EfiStatus::Unsupported,
            4 => EfiStatus::BadBufferSize,
            5 => EfiStatus::BufferTooSmall,
            6 => EfiStatus::NotInReadyState,
            7 => EfiStatus::DeviceError,
            8 => EfiStatus::WriteProtected,
            9 => EfiStatus::OutOfResources,
            10 => EfiStatus::VolumeCorrupted,
            11 => EfiStatus::VolumeFull,
            12 => EfiStatus::NoMedia,
            13 => EfiStatus::MediaChanged,
            14 => EfiStatus::NotFound,
            15 => EfiStatus::AccessDenied,
            16 => EfiStatus::NoResponse,
            17 => EfiStatus::NoMapping,
            18 => EfiStatus::Timeout,
            19 => EfiStatus::NotStarted,
            20 => EfiStatus::AlreadyStarted,
            21 => EfiStatus::Aborted,
            22 => EfiStatus::IcalFailed,
            _ => EfiStatus::Unsupported, // Fallback for unknown status codes
        }
    }
}

/// Minimal subset of UEFI memory types (only those we need)
#[repr(usize)]
#[derive(Clone, Copy, PartialEq)]
pub enum EfiMemoryType {
    EfiReservedMemoryType = 0,
    EfiLoaderCode = 1,
    EfiLoaderData = 2,
    EfiBootServicesCode = 3,
    EfiBootServicesData = 4,
    EfiRuntimeServicesCode = 5,
    EfiRuntimeServicesData = 6,
    EfiConventionalMemory = 7,
    EfiMaxMemoryType = 15,
}

/// GUID for EFI_LOADED_IMAGE_PROTOCOL (UEFI)
pub const EFI_LOADED_IMAGE_PROTOCOL_GUID: [u8; 16] = [
    0xa1, 0x31, 0x1b, 0x5b, 0x62, 0x95, 0xd2, 0x11, 0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// GUID for EFI_SIMPLE_FILE_SYSTEM_PROTOCOL (UEFI)
pub const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x22, 0x5b, 0x4e, 0x96, 0x59, 0x64, 0xd2, 0x11, 0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

pub const EFI_FILE_INFO_GUID: [u8; 16] = [
    0x0d, 0x95, 0xde, 0x05, 0x93, 0x31, 0xd2, 0x11, 0x8a, 0x41, 0x00, 0xa0, 0xc9, 0x3e, 0xc7, 0xea,
];

/// GUID for EFI_GRAPHICS_OUTPUT_PROTOCOL (UEFI)
pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: [u8; 16] = [
    0xde, 0xa9, 0x42, 0x90, 0x4c, 0x23, 0x38, 0x4a, 0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a,
];

/// GUID for EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL (UEFI)
pub const EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID: [u8; 16] = [
    0x98, 0x2c, 0x29, 0x8b, 0xf4, 0xfa, 0x41, 0xcb, 0xb8, 0x38, 0x77, 0x7b, 0xa2, 0x48, 0x21, 0x13,
];

/// EFI_STATUS code for EFI_BUFFER_TOO_SMALL (UEFI)
pub const EFI_BUFFER_TOO_SMALL: usize = 0x8000000000000005;

/// Minimal EFI_FILE_INFO (UEFI)
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

#[repr(C)]
pub struct EfiConfigurationTable {
    pub vendor_guid: [u8; 16],
    pub vendor_table: usize,
}

/// Minimal UEFI System Table and protocols used by this loader (UEFI)
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
    pub number_of_table_entries: usize,
    pub configuration_table: *mut EfiConfigurationTable,
}

/// Very small subset of Boot Services we call (UEFI)
#[repr(C)]
pub struct EfiBootServices {
    pub hdr: [u64; 3], // EFI_TABLE_HEADER
    _pad0: [usize; 2], // fn0,1
    pub allocate_pages: extern "efiapi" fn(usize, EfiMemoryType, usize, *mut usize) -> usize, // fn2
    pub free_pages: extern "efiapi" fn(usize, usize) -> usize, // fn3
    pub get_memory_map:
        extern "efiapi" fn(*mut usize, *mut c_void, *mut usize, *mut usize, *mut u32) -> usize, // fn4
    _unused5: usize,                                         // fn5
    pub free_pool: extern "efiapi" fn(*mut c_void) -> usize, // fn6
    _unused7: usize,                                         // fn7
    _unused8: usize,                                         // fn8
    _unused9: usize,                                         // fn9
    _unused10: usize,                                        // fn10
    _unused11: usize,                                        // fn11
    _unused12: usize,                                        // fn12
    _unused13: usize,                                        // fn13
    _unused14: usize,                                        // fn14
    _unused15: usize,                                        // fn15
    pub handle_protocol: extern "efiapi" fn(usize, *const u8, *mut *mut c_void) -> usize, // fn16
    _unused17: usize,                                        // fn17
    pub locate_handle:
        extern "efiapi" fn(u32, *const u8, *mut c_void, *mut usize, *mut usize) -> usize, // fn18
    _unused19: usize,                                        // fn19
    pub install_configuration_table: extern "efiapi" fn(*const u8, *mut c_void) -> usize, // fn20
    _unused21: usize,                                        // fn21
    _unused22: usize,                                        // fn22
    _unused23: usize,                                        // fn23
    _unused24: usize,                                        // fn24
    pub exit_boot_services: extern "efiapi" fn(usize, usize) -> usize, // fn25
    _unused26: usize,                                        // fn26
    pub stall: extern "efiapi" fn(usize) -> usize,           // fn27
    _unused28: usize,                                        // fn28
    _unused29: usize,                                        // fn29
    _unused30: usize,                                        // fn30
    pub open_protocol:
        extern "efiapi" fn(usize, *const u8, *mut *mut c_void, usize, usize, u32) -> usize, // fn31
    pub close_protocol:
        extern "efiapi" fn(usize, *const u8, usize, usize) -> usize, // fn37
    _unused32: usize,                                        // fn32
    _unused33: usize,                                        // fn33
    _unused34: usize,                                        // fn34
    pub locate_handle_buffer:
        extern "efiapi" fn(u32, *const u8, *mut c_void, *mut usize, *mut *mut usize) -> usize, // fn35
    pub locate_protocol: extern "efiapi" fn(*const u8, *mut c_void, *mut *mut c_void) -> usize, // fn36
}

/// Minimal UEFI Simple Text Output Protocol (UEFI)
#[repr(C)]
pub struct EfiSimpleTextOutput {
    _pad: [usize; 2],
    /// output_string(This, *mut u16) -> EFI_STATUS
    pub output_string: extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> usize,
}

/// A minimal subset of EFI_FILE_PROTOCOL (UEFI)
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

/// Minimal EFI_SIMPLE_FILE_SYSTEM_PROTOCOL (UEFI)
#[repr(C)]
pub struct EfiSimpleFileSystem {
    _pad: [usize; 1],
    /// open_volume(This, *mut EfiSimpleFileSystem, *mut *mut EfiFile) -> EFI_STATUS
    pub open_volume: extern "efiapi" fn(*mut EfiSimpleFileSystem, *mut *mut EfiFile) -> usize,
}

/// Minimal EFI_LOADED_IMAGE_PROTOCOL (UEFI)
#[repr(C)]
pub struct EfiLoadedImageProtocol {
    pub revision: u32,
    pub parent_handle: usize,
    pub device_handle: usize,
    // more fields, but we only need these
}

/// Minimal EFI_GRAPHICS_OUTPUT_PROTOCOL (UEFI)
#[repr(C)]
pub struct EfiGraphicsOutputProtocol {
    /// query_mode(This, ModeNumber, SizeOfInfo, *mut Info) -> EFI_STATUS
    pub query_mode:
        extern "efiapi" fn(*mut EfiGraphicsOutputProtocol, u32, *mut usize, *mut c_void) -> usize,
    /// set_mode(This, ModeNumber) -> EFI_STATUS
    pub set_mode: extern "efiapi" fn(*mut EfiGraphicsOutputProtocol, u32) -> usize,
    /// blt(This, BltBuffer, BltOperation, SourceX, SourceY, DestinationX, DestinationY, Width, Height, Delta)
    pub blt: usize, // We don't need this function, so we can ignore it or use usize
    pub mode: *mut EfiGraphicsOutputProtocolMode,
}

/// Minimal EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE (UEFI)
#[repr(C)]
pub struct EfiGraphicsOutputProtocolMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut EfiGraphicsOutputModeInformation,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

/// Minimal EFI_GRAPHICS_OUTPUT_MODE_INFORMATION (UEFI)
#[repr(C)]
pub struct EfiGraphicsOutputModeInformation {
    _version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
    _pad: [u8; 12],
    pub pixels_per_scan_line: u32,
}

/// Get bits per pixel from UEFI graphics pixel format.
/// Returns 32 for RGB/BGR formats, 0 for unsupported formats.
pub fn get_bpp_from_pixel_format(pixel_format: EfiGraphicsPixelFormat) -> u32 {
    match pixel_format {
        EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
        | EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => 32,
        EfiGraphicsPixelFormat::PixelBitMask => {
            // For PixelBitMask, we would need to parse the mask, but for now assume 32
            // as it's not commonly used and the format is complex
            32
        }
        EfiGraphicsPixelFormat::PixelBltOnly => 0, // Software rendering only
        _ => 0,
    }
}
