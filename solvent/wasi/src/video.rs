extern crate alloc;

use alloc::vec::Vec;

use z264::Frame;

const MAX_WIDTH: u32 = 800;
const MAX_HEIGHT: u32 = 600;

/// Convert a decoded YUV 4:2:0 frame to the RGB layout used by Fullerene
/// windows. This is shared by the native WASI path and its standalone
/// benchmark so conversion behavior cannot diverge.
pub fn yuv420_to_rgb(frame: &Frame, rgb: &mut Vec<u8>) -> Option<(u32, u32, bool)> {
    let source_width = usize::try_from(frame.width).ok()?;
    let source_height = usize::try_from(frame.height).ok()?;
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (width, height) = fit_video_dimensions(frame.width, frame.height);
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let y_len = source_width.checked_mul(source_height)?;
    let uv_width = source_width.div_ceil(2);
    let uv_height = source_height.div_ceil(2);
    let uv_len = uv_width.checked_mul(uv_height)?;
    if frame.y.len() < y_len || frame.u.len() < uv_len || frame.v.len() < uv_len {
        return None;
    }
    let rgb_len = width.checked_mul(height)?.checked_mul(3)?;
    rgb.resize(rgb_len, 0);
    for output_y in 0..height {
        let source_y = output_y * source_height / height;
        let y_row = source_y * source_width;
        let uv_row = (source_y / 2) * uv_width;
        for output_x in 0..width {
            let source_x = output_x * source_width / width;
            let yi = y_row + source_x;
            let ui = uv_row + source_x / 2;
            let dst = (output_y * width + output_x) * 3;
            let yv = frame.y[yi] as i32;
            let uv = frame.u[ui] as i32 - 128;
            let vv = frame.v[ui] as i32 - 128;
            rgb[dst] = (yv + (359 * vv) / 256).clamp(0, 255) as u8;
            rgb[dst + 1] = (yv - (88 * uv + 183 * vv) / 256).clamp(0, 255) as u8;
            rgb[dst + 2] = (yv + (454 * uv) / 256).clamp(0, 255) as u8;
        }
    }
    Some((
        width as u32,
        height as u32,
        width != source_width || height != source_height,
    ))
}

/// Fit a video frame to the maximum window surface while preserving aspect
/// ratio. Zero-sized input propagates as zero dimensions.
pub fn fit_video_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    if width <= MAX_WIDTH && height <= MAX_HEIGHT {
        return (width, height);
    }
    if u64::from(width) * u64::from(MAX_HEIGHT) <= u64::from(height) * u64::from(MAX_WIDTH) {
        (
            (u64::from(width) * u64::from(MAX_HEIGHT) / u64::from(height)).max(1) as u32,
            MAX_HEIGHT,
        )
    } else {
        (
            MAX_WIDTH,
            (u64::from(height) * u64::from(MAX_WIDTH) / u64::from(width)).max(1) as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;
    use alloc::vec;

    use super::*;

    #[test]
    fn zero_dimensions_are_rejected_without_division() {
        assert_eq!(fit_video_dimensions(0, 100), (0, 0));
        assert_eq!(fit_video_dimensions(100, 0), (0, 0));
        let frame = Frame {
            width: 0,
            height: 1,
            y: Rc::new(vec![0]),
            u: Rc::new(vec![128]),
            v: Rc::new(vec![128]),
            pic_order_cnt: 0,
        };
        assert!(yuv420_to_rgb(&frame, &mut Vec::new()).is_none());
    }

    #[test]
    fn converts_neutral_chroma() {
        let frame = Frame {
            width: 2,
            height: 2,
            y: Rc::new(vec![100; 4]),
            u: Rc::new(vec![128]),
            v: Rc::new(vec![128]),
            pic_order_cnt: 0,
        };
        let mut rgb = Vec::new();
        assert_eq!(yuv420_to_rgb(&frame, &mut rgb), Some((2, 2, false)));
        assert_eq!(rgb, vec![100; 12]);
    }
}
