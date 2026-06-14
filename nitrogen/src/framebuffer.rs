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
//!  ├── fb_base: *mut u32              (virtual base of framebuffer)
//!  ├── width, height, stride, bpp     (dimensions)
//!  ├── fb_byte_size: usize
//!  └── gpu: Option<VirtioGpu>         (for present/flush)
//! ```

use crate::virtio::gpu::VirtioGpu;
use alloc::boxed::Box;

/// Unified framebuffer manager — owns the hardware framebuffer mechanism.
///
/// After construction, all framebuffer access goes through safe methods.
/// The caller provides the virtual base pointer, dimensions, and optional
/// GPU handle.
pub struct FramebufferManager {
    /// Framebuffer virtual base address (WC-mapped by the caller).
    fb_base: *mut u32,
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
    pub unsafe fn new(
        fb_virt_base: *mut u32,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        fb_byte_size: usize,
    ) -> Self {
        Self {
            fb_base: fb_virt_base,
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
    pub unsafe fn with_gpu(
        fb_virt_base: *mut u32,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        fb_byte_size: usize,
        gpu: Box<VirtioGpu>,
    ) -> Self {
        Self {
            fb_base: fb_virt_base,
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
    pub fn base_ptr(&self) -> *mut u32 {
        self.fb_base
    }
    pub fn byte_size(&self) -> usize {
        self.fb_byte_size
    }

    // ── Pixel access ──────────────────────────────────────────────

    /// Write a single pixel at (x, y).  Bounds are checked.
    pub fn write_pixel(&self, x: u32, y: u32, color: u32) {
        if x < self.width && y < self.height {
            let offset = (y * self.stride + x) as usize;
            unsafe {
                core::ptr::write_volatile(self.fb_base.add(offset), color);
            }
        }
    }

    /// Fill the entire framebuffer with a single color.
    pub fn fill(&self, color: u32) {
        let pixels = (self.fb_byte_size / 4) as usize;
        for i in 0..pixels {
            unsafe {
                core::ptr::write_volatile(self.fb_base.add(i), color);
            }
        }
    }

    /// Copy a rectangular region from a source buffer into the framebuffer.
    ///
    /// # Panics
    ///
    /// Panics if `src` is too small for the rectangle.
    pub fn copy_rect(&self, x: u32, y: u32, w: u32, h: u32, src: &[u32]) {
        assert!(src.len() >= (w as usize) * (h as usize));
        let clip_w = w.min(self.width.saturating_sub(x));
        let clip_h = h.min(self.height.saturating_sub(y));
        for row in 0..clip_h {
            let src_offset = (row * w) as usize;
            let dst_offset = ((y + row) * self.stride + x) as usize;
            for col in 0..clip_w {
                unsafe {
                    core::ptr::write_volatile(
                        self.fb_base.add(dst_offset + col as usize),
                        src[src_offset + col as usize],
                    );
                }
            }
        }
    }

    /// Retrieve a mutable slice of the framebuffer pixels.
    ///
    /// # Safety
    ///
    /// The returned slice must not outlive this `FramebufferManager`.
    pub unsafe fn as_slice_mut(&self) -> &mut [u32] {
        let len = (self.fb_byte_size / 4) as usize;
        unsafe { core::slice::from_raw_parts_mut(self.fb_base, len) }
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