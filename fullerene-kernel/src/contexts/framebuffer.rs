//! FramebufferContext — replaces PRIMARY_RENDERER, VIRTIO_GPU, VGA_CONSOLE.
use alloc::boxed::Box;
use core::fmt::Write;
use nitrogen::virtio::gpu::VirtioGpu;
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
    pub fb_pixel_format: petroleum::common::EfiGraphicsPixelFormat,
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
            fb_pixel_format:
                petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        }
    }
    pub fn store_raw_params(
        &mut self,
        phys: u64,
        width: u32,
        height: u32,
        stride: u32,
        bpp: u32,
        pixel_format: petroleum::common::EfiGraphicsPixelFormat,
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
        if self.fb_pixel_format
            != petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor
            && self.fb_pixel_format
                != petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
        {
            return false;
        }

        // Pick a fixed high VA (0xFFFF_FFF0_0000_0000) that is guaranteed
        // unmapped in both QEMU and real hardware.  We create 4 KiB WC/UC
        // mappings there WITHOUT touching the boot‑time huge‑page identity
        // mapping (see README §3 for why huge‑page WB + MTRR UC = #GP).
        const FB_VA_BASE: u64 = 0xFFFF_FFF0_0000_0000;
        let fb_byte_size = self.fb_stride_bytes as u64 * self.fb_height_px as u64;
        let fb_pages = ((fb_byte_size + 0xFFF) / 0x1000) as usize;
        // Sanity: 2 KiB max (~8 GiB framebuffer) — enough for 4K×2K.
        if fb_pages > 2048 {
            return false;
        }

        let fb_va = FB_VA_BASE;

        let flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE
            | x86_64::structures::paging::PageTableFlags::NO_CACHE;

        if let Some(mem) = crate::memory_management::get_memory_manager()
            .lock()
            .as_mut()
        {
            for i in 0..fb_pages {
                let v = (fb_va + (i * 0x1000) as u64) as usize;
                let p = (self.fb_phys + (i * 0x1000) as u64) as usize;
                if mem.safe_map_page(v, p, flags).is_err() {
                    return false;
                }
            }
        } else {
            return false;
        }

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

        petroleum::serial::serial_log(format_args!(
            "[fb] WC mapping: phys=0x{:x} → va=0x{:x}, {} pages\n",
            self.fb_phys, fb_va, fb_pages
        ));
        true
    }
    pub unsafe fn init_from_kernel_args(&mut self, args: &petroleum::assembly::KernelArgs) {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[fb_ctx] init_from_kernel_args called\n");
        if self.renderer.is_some() {
            return;
        }
        if args.fb_address < 0x100000
            || args.fb_width == 0
            || args.fb_width > 16384
            || args.fb_height == 0
            || args.fb_height > 16384
            || args.fb_bpp != 32
        {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[fb_ctx] validation FAILED\n");
            return;
        }
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[fb_ctx] validation passed\n");
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let fb_virt = args.fb_address + off;
        let stride = if args.fb_stride > 0 {
            args.fb_stride
        } else {
            args.fb_width.saturating_mul(4)
        };
        let pixel_format = match args.fb_pixel_format {
            0 => petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            _ => petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        };
        let info = FramebufferInfo {
            address: fb_virt,
            width: args.fb_width,
            height: args.fb_height,
            stride,
            pixel_format: Some(pixel_format),
            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };
        let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
        self.renderer = Some(UefiFramebufferWriter::Uefi32(writer));
        self.bpp = 32;
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[fb_ctx] renderer set successfully\n");
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
    pub fn pixel_offset(x: u32, y: u32, stride: u32) -> usize {
        (y * stride + x) as usize
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
        } else {
            // WC (Write Combining) memory requires both an SFENCE to
            // make stores globally visible, and a dummy read from the
            // same WC range to drain the WC buffers.
            // _mm_mfence alone is not sufficient on real hardware.
            unsafe {
                core::arch::x86_64::_mm_sfence();
            }
            // Dummy read from the framebuffer to force WC buffer drain.
            if let Some(ref r) = self.renderer {
                let info = r.get_info();
                let fb_ptr = info.address as *mut u32;
                // Read the last pixel written (approximation: first pixel
                // of the framebuffer).  This forces any pending WC stores
                // to be committed to the device.
                unsafe {
                    core::ptr::read_volatile(fb_ptr);
                }
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
