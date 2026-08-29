//! Dirty-rectangle framebuffer for the 320x240 SPI panel.

use alloc::vec::Vec;

/// A permanent RGB565 surface is 150 KiB; on this profile it is cheaper and
/// simpler than double-buffering to SPI. All transfers use dirty clips.
pub struct Esp32Compositor {
    width: u16,
    height: u16,
    pixels: Vec<u16>,
}

impl Esp32Compositor {
    pub const fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pixels: Vec::new(),
        }
    }

    /// Allocates the bounded RGB565 surface after the kernel heap is ready.
    pub fn allocate(&mut self) -> bool {
        let count = usize::from(self.width) * usize::from(self.height);
        self.pixels = alloc::vec![0u16; count];
        self.pixels.len() == count
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    pub fn clear(&mut self, color: u16) {
        self.pixels.fill(color);
    }

    pub fn mark_and_flush(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        color: u16,
    ) -> Option<DirtyClip> {
        if x >= self.width || y >= self.height || self.pixels.is_empty() {
            return None;
        }
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);
        for row in 0..height {
            let start = usize::from(y + row) * usize::from(self.width) + usize::from(x);
            self.pixels[start..start + usize::from(width)].fill(color);
        }
        Some(DirtyClip {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyClip {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_dirty_rectangles_to_the_panel() {
        let mut target = Esp32Compositor::new(320, 240);
        assert!(target.allocate());
        let clip = target.mark_and_flush(300, 200, 40, 40, 0x1234).unwrap();
        assert_eq!(
            (clip.x, clip.y, clip.width, clip.height),
            (300, 200, 20, 40)
        );
        assert!(target.mark_and_flush(320, 0, 1, 1, 0).is_none());
    }

    #[test]
    fn fills_only_visible_pixels() {
        let mut target = Esp32Compositor::new(2, 2);
        assert!(target.allocate());
        target.mark_and_flush(1, 1, 4, 4, 0x1234);
        assert_eq!(target.pixels(), &[0, 0, 0, 0x1234]);
    }
}
