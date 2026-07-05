//! UEFI Graphics Output Protocol discovery.

use crate::common::memory::create_framebuffer_config;
use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiGraphicsPixelFormat,
    EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
};
use core::{ffi::c_void, ptr};
use spin::Mutex;

macro_rules! log_uefi {
    ($($arg:tt)*) => { crate::serial::_print(format_args!($($arg)*)) };
}

fn locate_gop(
    system_table: &EfiSystemTable,
) -> Result<*mut EfiGraphicsOutputProtocol, EfiStatus> {
    let services = unsafe { system_table.boot_services.as_ref() }.ok_or(EfiStatus::InvalidParameter)?;
    let mut protocol: *mut c_void = ptr::null_mut();
    let status = EfiStatus::from((services.locate_protocol)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut protocol,
    ));
    if status != EfiStatus::Success || protocol.is_null() {
        Err(status)
    } else {
        Ok(protocol.cast())
    }
}

fn normalize_pixel_format(
    format: EfiGraphicsPixelFormat,
    masks: [u32; 4],
) -> Option<EfiGraphicsPixelFormat> {
    match format {
        EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
        | EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => Some(format),
        EfiGraphicsPixelFormat::PixelBitMask => match masks {
            [0x0000_00FF, 0x0000_FF00, 0x00FF_0000, _] => {
                Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor)
            }
            [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, _] => {
                Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor)
            }
            _ => None,
        },
        _ => None,
    }
}

fn install(config: FullereneFramebufferConfig) {
    crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
    const GRAY: u32 = 0x0080_8080;
    let pixels = usize::try_from(config.stride / 4)
        .ok()
        .and_then(|stride| stride.checked_mul(config.height as usize))
        .unwrap_or(0);
    let base = config.address as *mut u32;
    for index in 0..pixels {
        unsafe { base.add(index).write_volatile(GRAY) };
    }
}

/// Capture the firmware-selected GOP mode without changing display mode.
/// Avoiding `SetMode` preserves compatibility with InsydeH2O firmware that
/// invalidates its mode-info allocation during redundant mode changes.
pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let gop_ptr = match locate_gop(system_table) {
        Ok(gop) => gop,
        Err(status) => {
            log_uefi!("GOP: protocol unavailable ({:#x})\n", status as u32);
            return None;
        }
    };
    let gop = unsafe { gop_ptr.as_ref() }?;
    let mode = unsafe { gop.mode.as_ref() }?;
    let info = unsafe { mode.info.as_ref() }?;
    let format = normalize_pixel_format(info.pixel_format, info.pixel_information)?;
    if mode.frame_buffer_base == 0
        || mode.frame_buffer_size == 0
        || info.horizontal_resolution == 0
        || info.vertical_resolution == 0
        || info.pixels_per_scan_line < info.horizontal_resolution
    {
        return None;
    }

    let stride = info.pixels_per_scan_line.checked_mul(4)?;
    let required = u64::from(stride).checked_mul(u64::from(info.vertical_resolution))?;
    if required > mode.frame_buffer_size as u64 {
        log_uefi!(
            "GOP: mode requires {} bytes but framebuffer exposes {}\n",
            required,
            mode.frame_buffer_size
        );
        return None;
    }
    let config = create_framebuffer_config(
        mode.frame_buffer_base as u64,
        info.horizontal_resolution,
        info.vertical_resolution,
        format,
        32,
        stride,
    );
    install(config);
    log_uefi!(
        "GOP: {}x{} stride={} base={:#x} size={}\n",
        config.width,
        config.height,
        config.stride,
        config.address,
        mode.frame_buffer_size
    );
    Some(config)
}

pub fn init_graphics_protocols(
    system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    init_gop_framebuffer(system_table)
}
