//! FramebufferContext — Primary framebuffer and display management.
//!
//! Consolidates:
//! - `crate::graphics::PRIMARY_RENDERER`
//! - `crate::graphics::VIRTIO_GPU`
//! - `crate::graphics::VGA_CONSOLE` (fallback)
//!
//! # Design
//!
//! Instead of multiple static `Mutex<Option<...>>` globals scattered
//! across `graphics/mod.rs` and `gui.rs`, this context holds them in
//! a single struct so callers write:
//!
//! ```rust,ignore
//! fb.draw_text("hello", x, y);
//! fb.flush();
//! ```
//!
//! The `stride != width` problem becomes obvious:
//! ```rust,ignore
//! offset = y * fb.stride + x
//! ```

use alloc::boxed::Box;
use core::fmt::Write;
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::graphics::color::FramebufferInfo;
use petroleum::graphics::framebuffer::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;
use spin::Mutex;

/// Wraps the primary framebuffer renderer, optional VirtIO-GPU device,
/// and VGA fallback into a single context.
pub struct FramebufferContext {
    /// Primary framebuffer writer (GOP or VirtIO-GPU backed).
    pub renderer: Option<UefiFramebufferWriter>,

    /// VirtIO-GPU device handle.  `None` on systems without VirtIO-GPU.
    pub gpu: Option<Box<VirtioGpu>>,

    /// Fallback VGA text console.
    pub vga_console: Option<VgaBuffer>,

    /// Pixel format (BPP).
    pub bpp: u32,
}

impl FramebufferContext {
    /// Create an empty context.  Call `init()` to populate it.
    pub const fn new() -> Self {
        Self {
            renderer: None,
            gpu: None,
            vga_console: None,
            bpp: 32,
        }
    }

    /// Return the framebuffer info (address, dimensions, stride, format).
    pub fn info(&self) -> Option<FramebufferInfo> {
        self.renderer.as_ref().map(|r| *r.get_info())
    }

    /// Width in pixels, or 0 if no framebuffer.
    pub fn width(&self) -> u32 {
        self.info().map(|i| i.width).unwrap_or(0)
    }

    /// Height in pixels, or 0 if no framebuffer.
    pub fn height(&self) -> u32 {
        self.info().map(|i| i.height).unwrap_or(0)
    }

    /// Stride in pixels (may differ from width!).
    pub fn stride(&self) -> u32 {
        self.info().map(|i| i.stride).unwrap_or(0)
    }

    /// Framebuffer base virtual address (as `*mut u32`).
    pub fn base_ptr(&self) -> *mut u32 {
        self.info()
            .map(|i| i.address as *mut u32)
            .unwrap_or(core::ptr::null_mut())
    }

    /// Compute the pixel offset at (x, y) using stride.
    /// This is where `width != stride` becomes visible.
    #[inline]
    pub fn pixel_offset(x: u32, y: u32, stride: u32) -> usize {
        (y * stride + x) as usize
    }

    /// Get a mutable slice of the framebuffer pixels.
    pub fn pixels_mut(&mut self) -> Option<&mut [u32]> {
        let info = self.info()?;
        let ptr = info.address as *mut u32;
        let len = (info.width as usize) * (info.height as usize);
        Some(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
    }

    /// Write a string to the renderer (or VGA fallback).
    pub fn write_str(&mut self, s: &str) {
        if let Some(ref mut r) = self.renderer {
            let _ = r.write_str(s);
        } else if let Some(ref mut vga) = self.vga_console {
            let _ = core::fmt::write(vga, format_args!("{}", s));
        }
    }

    /// Write formatted text.
    pub fn write_fmt(&mut self, args: core::fmt::Arguments) {
        if let Some(ref mut r) = self.renderer {
            let _ = core::fmt::write(r, args);
        } else if let Some(ref mut vga) = self.vga_console {
            let _ = core::fmt::write(vga, args);
        }
    }

    /// Signal hardware present (VirtIO-GPU flush, or mfence for GOP).
    pub fn flush(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            if let Some(ref r) = self.renderer {
                let info = r.get_info();
                let w = info.width;
                let h = info.height;
                drop(info);
                gpu.flush(w, h);
            }
        } else {
            unsafe { core::arch::x86_64::_mm_mfence() };
        }
        // Force VM exit for KVM device model.
        nitrogen::hda::HdaController::tick_vm_exit();
    }

    /// Returns `true` if the framebuffer is backed by VirtIO-GPU.
    pub fn has_virtio_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// Returns `true` if any framebuffer is available.
    pub fn is_available(&self) -> bool {
        self.renderer.is_some() || self.vga_console.is_some()
    }
}

/// Global framebuffer context.  Replaces `PRIMARY_RENDERER`,
/// `VIRTIO_GPU`, and `VGA_CONSOLE`.
static FRAMEBUFFER: Mutex<Option<FramebufferContext>> = Mutex::new(None);

/// Initialise the global framebuffer context.
pub fn init_framebuffer_context(ctx: FramebufferContext) {
    *FRAMEBUFFER.lock() = Some(ctx);
}

/// Get a reference to the global framebuffer context.
pub fn get_framebuffer() -> &'static Mutex<Option<FramebufferContext>> {
    &FRAMEBUFFER
}

/// Convenience: execute a closure with a mutable reference.
pub fn with_framebuffer_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut FramebufferContext) -> R,
{
    FRAMEBUFFER.lock().as_mut().map(f)
}

/// Convenience: execute a closure with a shared reference.
pub fn with_framebuffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&FramebufferContext) -> R,
{
    FRAMEBUFFER.lock().as_ref().map(f)
}