//! FramebufferContext — replaces PRIMARY_RENDERER, VIRTIO_GPU, VGA_CONSOLE.
use alloc::boxed::Box;
use core::fmt::Write;
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::common::EfiGraphicsPixelFormat;
use petroleum::graphics::color::FramebufferInfo;
use petroleum::graphics::framebuffer::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;

pub struct FramebufferContext {
    pub renderer: Option<UefiFramebufferWriter>,
    pub gpu: Option<Box<VirtioGpu>>,
    pub vga_console: Option<VgaBuffer>,
    pub bpp: u32,
    pub fb_phys: u64,
    pub fb_width_px: u32,
    pub fb_height_px: u32,
    pub fb_stride_bytes: u32,
    pub fb_pixel_format: EfiGraphicsPixelFormat,
}

impl FramebufferContext {
    pub const fn new() -> Self {
        Self {
            renderer: None,
            gpu: None,
            vga_console: None,
            bpp: 32,
            fb_phys: 0,
            fb_width_px: 0,
            fb_height_px: 0,
            fb_stride_bytes: 0,
            fb_pixel_format: EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        }
    }
    pub fn store_raw_params(
        &mut self,
        phys: u64,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        pixel_format: EfiGraphicsPixelFormat,
    ) {
        self.fb_phys = phys;
        self.fb_width_px = width;
        self.fb_height_px = height;
        self.fb_stride_bytes = stride;
        self.bpp = bpp;
        self.fb_pixel_format = pixel_format;
    }
    pub fn build_renderer_from_stored(&mut self) -> bool {
        if self.renderer.is_some() {
            return true;
        }
        if self.fb_phys < 0x100000
            || self.fb_width_px == 0
            || self.fb_width_px > 16384
            || self.fb_height_px == 0
            || self.fb_height_px > 16384
            || self.fb_stride_bytes == 0
            || self.bpp != 32
        {
            return false;
        }
        if self.fb_pixel_format != EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor
            && self.fb_pixel_format != EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
        {
            return false;
        }

        // Use the bootloader's identity mapping (phys + offset) instead of
        // creating new 4 KiB WC pages.  Creating a second mapping for the
        // same physical memory with a conflicting cache type (WC vs WB) is
        // architecturally undefined on Intel and causes stale-cache /
        // read-modify-write corruption on real hardware like InsydeH2O.
        //
        // The boot‑time huge‑page WB mapping is preserved from
        // efi_main_stage2 and works correctly with the firmware MTRR UC
        // setting.  Grey‑fill tests on InsydeH2O confirm that writes
        // through the identity‑mapped VA are visible on screen.

        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let fb_va = self.fb_phys + off;

        petroleum::serial::serial_log(format_args!(
            "[fb] identity mapping: phys=0x{:x} → va=0x{:x}\n",
            self.fb_phys, fb_va
        ));

        let info = FramebufferInfo {
            address: fb_va,
            width: self.fb_width_px,
            height: self.fb_height_px,
            stride: self.fb_stride_bytes,
            pixel_format: Some(self.fb_pixel_format),
            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };
        let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
        self.renderer = Some(UefiFramebufferWriter::Uefi32(writer));
        true
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
    pub fn pixels_mut(&mut self) -> Option<&mut [u32]> {
        if self.bpp != 32 {
            return None;
        }
        let info = self.info()?;
        Some(unsafe {
            core::slice::from_raw_parts_mut(
                info.address as *mut u32,
                (info.stride as usize / 4) * info.height as usize,
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
                gpu.flush(w, h);
            }
        } else if let Some(ref r) = self.renderer {
            // The framebuffer is accessed via the bootloader's identity
            // mapping (WB huge-page).  CLFLUSH each cache line (64 B)
            // to force writeback, then MFENCE to order subsequent writes.
            // Safe even when MTRR sets UC (CLFLUSH is no‑op on UC lines).
            let info = r.get_info();
            let fb_ptr = info.address as *mut u8;
            let fb_byte_len = info.stride as usize * info.height as usize;
            unsafe {
                let mut addr = fb_ptr;
                let end = fb_ptr.add(fb_byte_len);
                while addr < end {
                    core::arch::x86_64::_mm_clflush(addr);
                    addr = addr.add(64);
                }
                core::arch::x86_64::_mm_mfence();
            }
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

crate::define_context!(FramebufferContext, framebuffer, FRAMEBUFFER);
