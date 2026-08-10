//! UEFI Graphics Output Protocol discovery.

use crate::common::memory::create_framebuffer_config;
use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputModeInformation, EfiGraphicsOutputProtocol,
    EfiGraphicsPixelFormat, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
};
use core::{ffi::c_void, mem::MaybeUninit, ptr};
use sealant::{FramebufferRegion, Permissions};
use spin::Mutex;

macro_rules! log_uefi {
    ($($arg:tt)*) => { crate::serial::_print(format_args!($($arg)*)) };
}

fn locate_gop(system_table: &EfiSystemTable) -> Result<*mut EfiGraphicsOutputProtocol, EfiStatus> {
    let services =
        unsafe { system_table.boot_services.as_ref() }.ok_or(EfiStatus::InvalidParameter)?;
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
        // PixelBitMask allows arbitrary bit assignments per channel.
        // Only 8-bit-per-channel 32bpp layouts with known channel order are
        // handled here; unrecognised masks fall through to None so the
        // system continues headless rather than rendering with wrong colours.
        EfiGraphicsPixelFormat::PixelBitMask => match masks {
            // R at byte0, G at byte1, B at byte2 (common Intel GOP)
            [0x0000_00FF, 0x0000_FF00, 0x00FF_0000, _] => {
                Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor)
            }
            // B at byte0, G at byte1, R at byte2 (common AMD/NVIDIA GOP)
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
    let region = unsafe {
        FramebufferRegion::from_address(
            config.address as usize,
            (config.stride as usize)
                .checked_mul(config.height as usize)
                .unwrap_or(0),
            Permissions::READ_WRITE,
        )
    };
    let Ok(region) = region else { return };
    for index in 0..pixels {
        let _ = region.write_volatile_at(index * 4, GRAY);
    }
}

fn query_mode_info(
    gop_ptr: *mut EfiGraphicsOutputProtocol,
    mode_number: u32,
) -> Option<EfiGraphicsOutputModeInformation> {
    let mut info = MaybeUninit::<EfiGraphicsOutputModeInformation>::uninit();
    let mut info_size = core::mem::size_of::<EfiGraphicsOutputModeInformation>();
    let status = EfiStatus::from(unsafe {
        ((*gop_ptr).query_mode)(
            gop_ptr,
            mode_number,
            &mut info_size,
            info.as_mut_ptr().cast::<c_void>(),
        )
    });
    if status != EfiStatus::Success
        || info_size < core::mem::size_of::<EfiGraphicsOutputModeInformation>()
    {
        return None;
    }
    Some(unsafe { info.assume_init() })
}

fn mode_score(info: &EfiGraphicsOutputModeInformation) -> Option<(u8, u64, u64)> {
    normalize_pixel_format(info.pixel_format, info.pixel_information)?;
    let width = info.horizontal_resolution;
    let height = info.vertical_resolution;
    if width == 0 || height == 0 || info.pixels_per_scan_line < width {
        return None;
    }
    let area = u64::from(width) * u64::from(height);
    // Prefer the common 1080p mode. If it is not available, prefer the
    // largest mode that is still practical for the desktop's software UI.
    let class = if width == 1920 && height == 1080 {
        3
    } else if width <= 1920 && height <= 1200 {
        2
    } else {
        1
    };
    let aspect_error = (i64::from(width) * 9 - i64::from(height) * 16).unsigned_abs();
    Some((class, area, u64::MAX - aspect_error))
}

fn preferred_mode(
    gop_ptr: *mut EfiGraphicsOutputProtocol,
    max_mode: u32,
    current_mode: u32,
) -> Option<u32> {
    // Do not unexpectedly downscale a display that is already at a normal
    // desktop resolution. The mode switch is specifically for firmware or
    // Ventoy fallback modes such as 640x480 and 800x600.
    if let Some(current_info) = query_mode_info(gop_ptr, current_mode)
        && let Some((class, area, _)) = mode_score(&current_info)
        && class >= 2
        && area >= 1280 * 720
    {
        return None;
    }

    let mut best: Option<(u8, u64, u64, u32)> = None;
    for mode_number in 0..max_mode {
        let Some(info) = query_mode_info(gop_ptr, mode_number) else {
            continue;
        };
        let Some((class, area, aspect)) = mode_score(&info) else {
            continue;
        };
        let candidate = (class, area, aspect, mode_number);
        if best.is_none_or(|previous| candidate > previous) {
            best = Some(candidate);
        }
    }
    best.map(|(_, _, _, mode)| mode)
        .filter(|&mode| mode != current_mode)
}

/// Select a usable GOP mode before capturing the framebuffer.
///
/// Ventoy can leave the firmware GOP at a fallback 640x480/800x600 mode even
/// when the panel supports 1080p. The kernel inherits that mode, so enumerate
/// the GOP modes and request the best practical one before installing the
/// framebuffer configuration. If a firmware rejects `SetMode`, the current
/// mode remains a safe fallback.
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
    let current_mode = mode.mode;
    if let Some(preferred) = preferred_mode(gop_ptr, mode.max_mode, current_mode) {
        let status = EfiStatus::from((gop.set_mode)(gop_ptr, preferred));
        if status == EfiStatus::Success {
            log_uefi!("GOP: switched mode {} -> {}\n", current_mode, preferred);
        } else {
            log_uefi!(
                "GOP: SetMode({}) failed ({:#x}), keeping mode {}\n",
                preferred,
                status as u32,
                current_mode
            );
        }
    }

    // SetMode may replace the mode-info allocation, so reacquire both
    // pointers after the attempted switch.
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
