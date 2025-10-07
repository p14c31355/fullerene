// petroleum/src/common.rs

use core::ffi::c_void;

// Common definitions for UEFI and BIOS modes.

/// GUID for FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID (UEFI only)
pub const FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID: [u8; 16] = [
    0x3c, 0x23, 0x88, 0x3f, 0x27, 0x4d, 0x78, 0x4d, 0x91, 0x2c, 0x73, 0x49, 0x3a, 0x0c, 0x23, 0x75,
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
pub struct FullereneFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

/// BIOS VGA config (fixed for mode 13h).
#[repr(C)]
pub struct VgaFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32, // Bits per pixel
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
    0x5b, 0x1b, 0x31, 0xa1, 0x62, 0x95, 0xd2, 0x11, 0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// GUID for EFI_SIMPLE_FILE_SYSTEM_PROTOCOL (UEFI)
pub const EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: [u8; 16] = [
    0x96, 0x4e, 0x5b, 0x09, 0x21, 0x42, 0x06, 0x4f, 0x85, 0x3d, 0x05, 0x22, 0x22, 0x0b, 0xa2, 0x19,
];

pub const EFI_FILE_INFO_GUID: [u8; 16] = [
    0x0d, 0x95, 0xde, 0x05, 0x93, 0x31, 0xd2, 0x11, 0x8a, 0x41, 0x00, 0xa0, 0xc9, 0x3e, 0xc7, 0xea,
];

/// GUID for EFI_GRAPHICS_OUTPUT_PROTOCOL (UEFI)
pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: [u8; 16] = [
    0xde, 0xa9, 0x42, 0x90, 0x4c, 0x23, 0x38, 0x4a, 0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a,
];

/// EFI_STATUS code for EFI_BUFFER_TOO_SMALL (UEFI)
pub const EFI_BUFFER_TOO_SMALL: usize = 0x8000000000000005;

/// Minimal EFI_FILE_INFO (UEFI)
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
    _pad0: [usize; 2], // raise_tpl, restore_tpl
    pub allocate_pages: extern "efiapi" fn(usize, EfiMemoryType, usize, *mut usize) -> usize, // idx 3
    pub free_pages: extern "efiapi" fn(usize, usize) -> usize, // idx 4
    pub get_memory_map:
        extern "efiapi" fn(*mut usize, *mut c_void, *mut usize, *mut usize, *mut u32) -> usize, // idx 5
    _pad1: [usize; 1], // 6
    pub free_pool: extern "efiapi" fn(*mut c_void) -> usize, // idx 7
    _pad2: [usize; 9], // 8-16
    pub handle_protocol: extern "efiapi" fn(usize, *const u8, *mut *mut c_void) -> usize, // idx 17
    _pad3: [usize; 1],  // 18
    pub locate_handle:
        extern "efiapi" fn(u32, *const u8, *mut c_void, *mut usize, *mut usize) -> usize, // idx 19
    _pad4: [usize; 1],  // 20
    pub install_configuration_table: extern "efiapi" fn(*const u8, *mut c_void) -> usize, // idx 21
    _pad5: [usize; 4],  // 22-25
    pub exit_boot_services: extern "efiapi" fn(usize, usize) -> usize, // idx 26
    _pad6: [usize; 1],  // 27
    pub stall: extern "efiapi" fn(usize) -> usize, // idx 28
    _pad7: [usize; 3],  // 29-31
    pub open_protocol:
        extern "efiapi" fn(usize, *const u8, *mut *mut c_void, usize, usize, u32) -> usize, // idx 32
    _pad8: [usize; 4], // 33-36
    pub locate_handle_buffer: extern "efiapi" fn(u32, *const u8, *mut c_void, *mut usize, *mut *mut usize) -> usize, // idx 37
    pub locate_protocol: extern "efiapi" fn(*const u8, *mut c_void, *mut *mut c_void) -> usize, // idx 38
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
    _pad: [usize; 3],
    pub mode: *mut EfiGraphicsOutputProtocolMode,
}

/// Minimal EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE (UEFI)
#[repr(C)]
pub struct EfiGraphicsOutputProtocolMode {
    _pad: [usize; 2],
    pub info: *mut EfiGraphicsOutputModeInformation,
    pub size_of_info: usize,
    _pad2: [usize; 1],
    pub frame_buffer_base: u64,
    pub frame_buffer_size: u64,
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

/// A custom error type for the bootloader (UEFI/BIOS).
#[derive(Debug, Clone, Copy)]
pub enum BellowsError {
    Efi { status: EfiStatus },
    FileIo(&'static str),
    PeParse(&'static str),
    AllocationFailed(&'static str),
    InvalidState(&'static str),
    ProtocolNotFound(&'static str),
}

impl From<EfiStatus> for BellowsError {
    fn from(status: EfiStatus) -> Self {
        Self::Efi { status }
    }
}

pub type Result<T> = core::result::Result<T, BellowsError>;
