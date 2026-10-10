//! FramebufferManager — unified framebuffer state and GPU present/flush.
//!
//! Encapsulates all unsafe volatile framebuffer access and GPU presentation
//! into a single struct.  Higher-level code (compositor, GUI) operates through
//! safe methods.
//!
//! This module belongs to Nitrogen because it owns the hardware mechanism:
//! framebuffer MMIO access and GPU command submission.  Policy types
//! (FullereneFramebufferConfig, UefiFramebufferWriter) remain in petroleum.
//!
//! # Architecture
//!
//! ```text
//! FramebufferManager
//!  ├── framebuffer: sealant::FramebufferRegion
//!  ├── width, height, stride, bpp     (dimensions)
//!  ├── fb_byte_size: usize
//!  └── gpu: Option<VirtioGpu>         (for present/flush)
//! ```

use crate::virtio::gpu::VirtioGpu;
use alloc::boxed::Box;
use sealant::{FramebufferRegion, Permissions};

/// Unified framebuffer manager — owns the hardware framebuffer mechanism.
///
/// After construction, all framebuffer access goes through safe methods.
/// The caller provides the virtual base pointer, dimensions, and optional
/// GPU handle.
pub struct FramebufferManager {
    /// Framebuffer virtual base address (WC-mapped by the caller).
    framebuffer: FramebufferRegion<'static>,
    /// Width in pixels.
    width: u32,
    /// Height in pixels.
    height: u32,
    /// Stride in pixels.
    stride: u32,
    /// Bytes-per-pixel.
    bpp: u32,
    /// Total framebuffer size in bytes.
    fb_byte_size: usize,
    /// VirtIO-GPU handle (None = GOP/VGA fallback, present is no-op).
    gpu: Option<Box<VirtioGpu>>,
}

unsafe impl Send for FramebufferManager {}

impl FramebufferManager {
    /// Create a new FramebufferManager without a GPU.
    ///
    /// # Safety
    ///
    /// `fb_virt_base` must point to a valid, mapped framebuffer region of
    /// at least `fb_byte_size` bytes.
    ///
    /// # Panics
    ///
    /// Panics if framebuffer layout invariants are violated.
    pub unsafe fn new(
        fb_virt_base: usize,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        fb_byte_size: usize,
    ) -> Self {
        // Validate framebuffer layout invariants
        assert!(
            bpp == 32 || bpp == 24 || bpp == 16 || bpp == 8,
            "Invalid bpp value: {}",
            bpp
        );
        let bytes_per_pixel = (bpp + 7) / 8;
        let required_size = (stride as usize) * (height as usize) * (bytes_per_pixel as usize);
        assert!(
            required_size <= fb_byte_size,
            "Framebuffer size {} is insufficient for {}x{} at stride {} and bpp {} (requires {})",
            fb_byte_size,
            width,
            height,
            stride,
            bpp,
            required_size
        );
        let framebuffer = unsafe {
            FramebufferRegion::from_address(fb_virt_base, fb_byte_size, Permissions::READ_WRITE)
                .expect("invalid framebuffer region")
        };
        Self {
            framebuffer,
            width,
            height,
            stride,
            bpp,
            fb_byte_size,
            gpu: None,
        }
    }

    /// Create a new FramebufferManager with a VirtIO-GPU handle.
    ///
    /// # Safety
    ///
    /// `fb_virt_base` must point to a valid, mapped framebuffer region.
    /// `gpu` must be a fully initialised VirtIO-GPU with display negotiated.
    ///
    /// # Panics
    ///
    /// Panics if framebuffer layout invariants are violated.
    pub unsafe fn with_gpu(
        fb_virt_base: usize,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        fb_byte_size: usize,
        gpu: Box<VirtioGpu>,
    ) -> Self {
        // Validate framebuffer layout invariants
        assert!(
            bpp == 32 || bpp == 24 || bpp == 16 || bpp == 8,
            "Invalid bpp value: {}",
            bpp
        );
        let bytes_per_pixel = (bpp + 7) / 8;
        let required_size = (stride as usize) * (height as usize) * (bytes_per_pixel as usize);
        assert!(
            required_size <= fb_byte_size,
            "Framebuffer size {} is insufficient for {}x{} at stride {} and bpp {} (requires {})",
            fb_byte_size,
            width,
            height,
            stride,
            bpp,
            required_size
        );
        let framebuffer = unsafe {
            FramebufferRegion::from_address(fb_virt_base, fb_byte_size, Permissions::READ_WRITE)
                .expect("invalid framebuffer region")
        };
        Self {
            framebuffer,
            width,
            height,
            stride,
            bpp,
            fb_byte_size,
            gpu: Some(gpu),
        }
    }

    // ── Dimensions ────────────────────────────────────────────────

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stride(&self) -> u32 {
        self.stride
    }
    pub fn bpp(&self) -> u32 {
        self.bpp
    }
    pub fn base_address(&self) -> usize {
        self.framebuffer.start()
    }
    pub fn byte_size(&self) -> usize {
        self.fb_byte_size
    }

    // ── Pixel access ──────────────────────────────────────────────

    /// Write a single pixel at (x, y).  Bounds are checked.
    pub fn write_pixel(&self, x: u32, y: u32, color: u32) {
        if x < self.width && y < self.height {
            let offset = (y * self.stride + x) as usize;
            let _ = self
                .framebuffer
                .write_volatile_at(offset * core::mem::size_of::<u32>(), color);
        }
    }

    /// Fill the entire framebuffer with a single color.
    pub fn fill(&self, color: u32) {
        let pixels = (self.fb_byte_size / 4) as usize;
        for i in 0..pixels {
            let _ = self
                .framebuffer
                .write_volatile_at(i * core::mem::size_of::<u32>(), color);
        }
    }

    /// Copy a rectangular region from a source buffer into the framebuffer.
    ///
    /// Silently fails if `src` is too small for the rectangle.
    pub fn copy_rect(&self, x: u32, y: u32, w: u32, h: u32, src: &[u32]) {
        if src.len() < (w as usize) * (h as usize) {
            return;
        }
        let clip_w = w.min(self.width.saturating_sub(x));
        let clip_h = h.min(self.height.saturating_sub(y));
        for row in 0..clip_h {
            let src_offset = (row * w) as usize;
            let dst_offset = ((y + row) * self.stride + x) as usize;
            for col in 0..clip_w {
                let _ = self.framebuffer.write_volatile_at(
                    (dst_offset + col as usize) * core::mem::size_of::<u32>(),
                    src[src_offset + col as usize],
                );
            }
        }
    }

    /// Retrieve a mutable slice of the framebuffer pixels.
    ///
    /// # Safety
    ///
    /// The returned slice must not outlive this `FramebufferManager`.
    pub unsafe fn as_slice_mut(&mut self) -> &mut [u32] {
        let len = (self.fb_byte_size / 4) as usize;
        unsafe {
            self.framebuffer
                .as_mut_slice(len)
                .expect("invalid framebuffer slice")
        }
    }

    // ── GPU present ───────────────────────────────────────────────

    /// Signal a present (page flip / flush) to the GPU.
    ///
    /// For VirtIO-GPU this sends a RESOURCE_FLUSH command.
    /// For GOP/VGA this is a no-op.
    pub fn present(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            gpu.flush(self.width, self.height);
        }
    }

    /// Check whether a GPU is attached.
    pub fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }
}
