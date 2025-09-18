// fullerene-kernel/src/uefi.rs

/// GUID for FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID
pub const FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID: [u8; 16] = [
    0x3c, 0x23, 0x88, 0x3f, 0x27, 0x4d, 0x78, 0x4d, 0x91, 0x2c, 0x73, 0x49, 0x3a, 0x0c, 0x23, 0x75,
];

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

/// The structure passed from the bootloader to the kernel.
#[repr(C)]
pub struct FullereneFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

#[repr(C)]
pub struct EfiSystemTable {
    _hdr: [u8; 24], // EfiTableHeader
    _firmware_vendor: *mut u16,
    _firmware_revision: u32,
    _console_in_handle: usize,
    _con_in: *mut core::ffi::c_void,
    _console_out_handle: usize,
    _con_out: *mut core::ffi::c_void,
    _standard_error_handle: usize,
    _std_err: *mut core::ffi::c_void,
    _runtime_services: *mut core::ffi::c_void,
    _boot_services: *mut core::ffi::c_void,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut EfiConfigurationTable,
}

#[repr(C)]
pub struct EfiConfigurationTable {
    pub vendor_guid: [u8; 16],
    pub vendor_table: usize,
}
