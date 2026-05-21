use crate::compositor::RenderTarget;

/// A software‑only framebuffer that stores pixels in a `Vec<u32>`.
///
/// Used for the PPM export example and for testing.
pub struct VecFramebuffer {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
}

impl VecFramebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            pixels: vec![0u32; len],
            width,
            height,
        }
    }

    /// Export the pixels as a [PPM P6](https://netpbm.sourceforge.net/doc/ppm.html) file.
    pub fn to_ppm_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes());

        for &pixel in &self.pixels {
            let r = ((pixel >> 16) & 0xFF) as u8;
            let g = ((pixel >> 8) & 0xFF) as u8;
            let b = (pixel & 0xFF) as u8;
            buf.push(r);
            buf.push(g);
            buf.push(b);
        }
        buf
    }
}

impl RenderTarget for VecFramebuffer {
    fn buffer(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}