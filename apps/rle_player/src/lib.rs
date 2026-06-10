//! RLE Player — RLE-encoded animation player library.
//!
//! Decodes the Fullerene RLE format ("BARL" files) and provides
//! frame-by-frame access for rendering into a `Surface` or any
//! pixel buffer.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Magic bytes for Fullerene RLE files.
pub const RLE_MAGIC: &[u8; 4] = b"BARL";
/// Header size: magic(4) + version(4) + frame_count(4) + width(2) + height(2)
pub const RLE_HDR_SIZE: usize = 16;

/// Errors returned by RLE parsing / decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RleError {
    /// File too short to contain a valid header.
    TooShort,
    /// Magic bytes don't match "BARL".
    BadMagic,
    /// Unsupported format version.
    BadVersion(u32),
    /// Frame count is zero.
    ZeroFrames,
    /// Frame offset / data out of bounds.
    Truncated,
    /// Frame index out of range.
    FrameOutOfRange,
}

/// Parsed RLE file header and frame index.
pub struct RleFile {
    /// Number of frames.
    pub frame_count: u32,
    /// Frame width in pixels.
    pub frame_width: u16,
    /// Frame height in pixels.
    pub frame_height: u16,
    /// Byte offset into `data` for each frame.
    frame_offsets: Vec<u64>,
    /// Total pixel count per frame (width × height).
    total_pixels: usize,
    /// Reference to the original data slice.
    data: &'static [u8],
}

impl RleFile {
    /// Parse an RLE file from raw bytes.
    ///
    /// `data` must be `&'static` because the file is typically embedded
    /// via `include_bytes!` at compile time.
    pub fn parse(data: &'static [u8]) -> Result<Self, RleError> {
        if data.len() < RLE_HDR_SIZE {
            return Err(RleError::TooShort);
        }
        if &data[..4] != RLE_MAGIC {
            return Err(RleError::BadMagic);
        }
        let ver = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if ver != 1 {
            return Err(RleError::BadVersion(ver));
        }
        let fc = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        if fc == 0 {
            return Err(RleError::ZeroFrames);
        }
        let fw = u16::from_le_bytes([data[12], data[13]]);
        let fh = u16::from_le_bytes([data[14], data[15]]);
        let n = fc as usize;

        let data_start = RLE_HDR_SIZE + n * 2;
        if data_start >= data.len() {
            return Err(RleError::Truncated);
        }

    let mut frame_offsets = Vec::with_capacity(n);
    let mut off: u64 = data_start as u64;
    for i in 0..n {
        let cs = u16::from_le_bytes([
            data[RLE_HDR_SIZE + i * 2],
            data[RLE_HDR_SIZE + i * 2 + 1],
        ]) as u64;
        frame_offsets.push(off);
        off = off.saturating_add(cs);
    }

    Ok(Self {
            frame_count: fc,
            frame_width: fw,
            frame_height: fh,
            frame_offsets,
            total_pixels: fw as usize * fh as usize,
            data,
        })
    }

    /// Total pixels per decoded frame (width × height).
    pub fn total_pixels(&self) -> usize {
        self.total_pixels
    }

    /// Decode a single frame into `buf`.
    ///
    /// `buf` must be at least `self.total_pixels()` bytes.
    /// Each byte is a greyscale value (0=black, 255=white).
    pub fn decode_frame(&self, frame_idx: usize, buf: &mut [u8]) -> Result<(), RleError> {
        if frame_idx >= self.frame_offsets.len() {
            return Err(RleError::FrameOutOfRange);
        }
        let fo = self.frame_offsets[frame_idx] as usize;
        let no = if frame_idx + 1 < self.frame_offsets.len() {
            self.frame_offsets[frame_idx + 1] as usize
        } else {
            self.data.len()
        };
        if fo >= self.data.len() || no > self.data.len() {
            return Err(RleError::Truncated);
        }
        let chunk = &self.data[fo..no];
        decode_rle_inner(chunk, buf, self.total_pixels);
        Ok(())
    }
}

/// Decode a single RLE-encoded chunk into a pixel buffer.
///
/// RLE format: 3 bytes per run — `[fill_byte, run_len_lo, run_len_hi]`.
/// Each run fills `run_len` pixels with `fill_byte`.
#[inline]
fn decode_rle_inner(data: &[u8], buf: &mut [u8], total: usize) {
    let mut p = 0usize;
    let mut c = 0usize;
    while c + 3 <= data.len() && p < total {
        let fill = data[c];
        let rl = u16::from_le_bytes([data[c + 1], data[c + 2]]) as usize;
        c += 3;
        let end = (p + rl).min(total);
        buf[p..end].fill(fill);
        p = end;
    }
}

/// Draw a decoded greyscale frame into a 32-bit RGBA pixel buffer
/// (typically a window `Surface`) with nearest-neighbour scaling and
/// letterbox/pillarbox preservation.
///
/// Hard threshold: pixel ≥ `threshold` → white, else → black (silhouette).
///
/// The frame is drawn at (`off_x`, `off_y`) with size (`draw_w`, `draw_h`),
/// stretching the source `decode` buffer (of dimensions `fw`×`fh`) to fit.
pub fn draw_decoded_frame(
    pixels: &mut [u32],
    buf_stride: u32,
    fw: u32,
    fh: u32,
    decode: &[u8],
    off_x: u32,
    off_y: u32,
    draw_w: u32,
    draw_h: u32,
    threshold: u8,
) {
    let stride = buf_stride as usize;
    let fw_u = fw as usize;
    let fh_u = fh as usize;
    let draw_w_u = draw_w as usize;
    let draw_h_u = draw_h as usize;
    let off_x_u = off_x as usize;
    let off_y_u = off_y as usize;

    for dy in 0..draw_h_u {
        let sy = dy * fh_u / draw_h_u;
        if sy >= fh_u {
            continue;
        }
        let src_row = &decode[sy * fw_u..];
        let row_off = (off_y_u + dy) * stride + off_x_u;
        if row_off + draw_w_u > pixels.len() {
            continue;
        }
        for dx in 0..draw_w_u {
            let sx = dx * fw_u / draw_w_u;
            if sx >= fw_u {
                continue;
            }
            let g = if src_row[sx] >= threshold { 255u32 } else { 0u32 };
            let pixel = 0xFF00_0000u32 | (g << 16) | (g << 8) | g;
            pixels[row_off + dx] = pixel;
        }
    }
}

/// Compute a letterbox/pillarbox draw region that preserves the
/// source aspect ratio within the destination rectangle.
///
/// Returns `(draw_w, draw_h, off_x, off_y)`.
pub fn compute_letterbox(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> (u32, u32, u32, u32) {
    let src_aspect = src_w as f64 / src_h as f64;
    let dst_aspect = dst_w as f64 / dst_h as f64;

    if dst_aspect > src_aspect {
        // Destination is wider → pillarbox (black bars left/right)
        let h = dst_h;
        let w = ((dst_h as f64 * src_aspect) as u32).max(1);
        (w, h, (dst_w.saturating_sub(w)) / 2, 0)
    } else {
        // Destination is taller → letterbox (black bars top/bottom)
        let w = dst_w;
        let h = ((dst_w as f64 / src_aspect) as u32).max(1);
        (w, h, 0, (dst_h.saturating_sub(h)) / 2)
    }
}