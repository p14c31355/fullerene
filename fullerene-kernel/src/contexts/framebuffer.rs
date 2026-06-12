//! FramebufferContext — replaces PRIMARY_RENDERER, VIRTIO_GPU, VGA_CONSOLE.
use alloc::boxed::Box;
use core::fmt::Write;
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::graphics::color::FramebufferInfo;
use petroleum::graphics::framebuffer::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;
use spin::Mutex;

pub struct FramebufferContext {
    pub renderer: Option<UefiFramebufferWriter>,
    pub gpu: Option<Box<VirtioGpu>>,
    pub vga_console: Option<VgaBuffer>,
    pub bpp: u32,
}

impl FramebufferContext {
    pub const fn new() -> Self {
        Self {
            renderer: None,
            gpu: None,
            vga_console: None,
            bpp: 32,
        }
    }
    pub fn info(&self) -> Option<FramebufferInfo> {
        self.renderer.as_ref().map(|r| *r.get_info())
    }
    pub fn width(&self) -> u32 {
        self.info().map(|i| i.width).unwrap_or(0)
    }
    pub fn height(&self) -> u32 {
        self.info().map(|i| i.height).unwrap_or(0)
    }
    pub fn stride(&self) -> u32 {
        self.info().map(|i| i.stride).unwrap_or(0)
    }
    pub fn base_ptr(&self) -> *mut u32 {
        self.info()
            .map(|i| i.address as *mut u32)
            .unwrap_or(core::ptr::null_mut())
    }
    #[inline]
    pub fn pixel_offset(x: u32, y: u32, stride: u32) -> usize {
        (y * stride + x) as usize
    }
    pub fn pixels_mut(&mut self) -> Option<&mut [u32]> {
        let info = self.info()?;
        Some(unsafe {
            core::slice::from_raw_parts_mut(
                info.address as *mut u32,
                info.width as usize * info.height as usize,
            )
        })
    }
    pub fn write_str(&mut self, s: &str) {
        if let Some(ref mut r) = self.renderer {
            let _ = r.write_str(s);
            return;
        }
        if let Some(ref mut v) = self.vga_console {
            let _ = core::fmt::write(v, format_args!("{}", s));
        }
    }
    pub fn write_fmt(&mut self, args: core::fmt::Arguments) {
        if let Some(ref mut r) = self.renderer {
            let _ = core::fmt::write(r, args);
            return;
        }
        if let Some(ref mut v) = self.vga_console {
            let _ = core::fmt::write(v, args);
        }
    }
    pub fn flush(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            if let Some(ref r) = self.renderer {
                let i = r.get_info();
                let (w, h) = (i.width, i.height);
                drop(i);
                gpu.flush(w, h);
            }
        } else {
            unsafe { core::arch::x86_64::_mm_mfence() };
        }
        nitrogen::hda::HdaController::tick_vm_exit();
    }
    pub fn has_virtio_gpu(&self) -> bool {
        self.gpu.is_some()
    }
    pub fn is_available(&self) -> bool {
        self.renderer.is_some() || self.vga_console.is_some()
    }
}

static FRAMEBUFFER: Mutex<Option<FramebufferContext>> = Mutex::new(None);
pub fn init_framebuffer() {
    *FRAMEBUFFER.lock() = Some(FramebufferContext::new());
}
pub fn init_framebuffer_with(ctx: FramebufferContext) {
    *FRAMEBUFFER.lock() = Some(ctx);
}
pub fn get_framebuffer() -> &'static Mutex<Option<FramebufferContext>> {
    &FRAMEBUFFER
}
pub fn with_framebuffer_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut FramebufferContext) -> R,
{
    FRAMEBUFFER.lock().as_mut().map(f)
}
pub fn with_framebuffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&FramebufferContext) -> R,
{
    FRAMEBUFFER.lock().as_ref().map(f)
}
