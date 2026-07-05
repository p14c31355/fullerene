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

        // The kernel's page-table init creates a 64 GB (0 – 0x10_0000_0000)
        // identity + higher-half huge-page mapping.  If the framebuffer
        // lives above that range, the identity mapping is incomplete and
        // every pixel write will page-fault.
        const IDENTITY_MAP_GB64: u64 = 0x10_0000_0000;
        let fb_size_bytes = self.fb_stride_bytes as u64 * self.fb_height_px as u64;
        let fb_end = self.fb_phys.saturating_add(fb_size_bytes);
        if fb_end > IDENTITY_MAP_GB64 {
            // FB extends beyond the 64 GB identity map.  Try to create a
            // proper 4 KB WC mapping by splitting the covering huge page.
            // This may break on InsydeH2O (see above), but is still better
            // than silently writing to unmapped virtual memory.
            petroleum::write_serial_bytes(
                0x3F8, 0x3FD,
                b"[fb] WARNING: FB above 64 GB identity map, creating 4 KB WC mapping\n",
            );
            let fb_pages = ((fb_size_bytes + 4095) / 4096) as usize;
            if !crate::contexts::memory::with_memory_mut(|mem| {
                // Split the covering 2 MB huge page and map the FB range as WC.
                let flags = x86_64::structures::paging::PageTableFlags::NO_CACHE
                    | x86_64::structures::paging::PageTableFlags::PRESENT
                    | x86_64::structures::paging::PageTableFlags::WRITABLE
                    | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
                let off = petroleum::common::memory::get_physical_memory_offset() as u64;
                let fb_va = self.fb_phys + off;
                for page in 0..fb_pages {
                    let vaddr = (fb_va + page as u64 * 4096) as usize;
                    let paddr = (self.fb_phys + page as u64 * 4096) as usize;
                    if mem.map_page(vaddr, paddr, flags).is_err() {
                        return false;
                    }
                }
                true
            }).unwrap_or(false) {
                return false; // mapping failed — bail out
            }
        }

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

/// Execute a closure with safe, lifetime-limited access to the framebuffer.
///
/// The `FramebufferGuard` prevents the `&'static mut` aliasing bug by
/// constraining the pixel slice lifetime to the closure scope.  Access
/// is serialized through the kernel mutex.
///
/// This is intentionally named differently from the `define_context!`-generated
/// `with_framebuffer` (which provides `&FramebufferContext` directly) to avoid
/// name collisions.
pub fn with_framebuffer_guard<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut petroleum::graphics::FramebufferGuard) -> R,
{
    crate::contexts::kernel::with_kernel_mut(|k| {
        let renderer = k.framebuffer.renderer.as_mut()?;
        let info = renderer.get_info();
        if info.address == 0 {
            return None;
        }
        let ptr = info.address as *mut u32;
        let stride_pixels = info.stride / 4;
        let len = (stride_pixels as usize) * info.height as usize;
        // SAFETY: the framebuffer is mapped for the entire kernel lifetime,
        // but the guard constrains access to this closure scope only.
        let pixels = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
        let mut guard = petroleum::graphics::FramebufferGuard::new(
            pixels,
            info.width,
            info.height,
            stride_pixels,
        );
        Some(f(&mut guard))
    })
    .flatten()
}

crate::define_context!(FramebufferContext, framebuffer, FRAMEBUFFER);
