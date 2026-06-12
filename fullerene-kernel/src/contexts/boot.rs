//! BootContext — boot-time information from UEFI/BIOS.
use petroleum::common::uefi::FullereneFramebufferConfig;
use petroleum::page_table::memory_map::MemoryMapDescriptor;
use spin::Mutex;

pub struct BootContext {
    pub framebuffer_config: FullereneFramebufferConfig,
    pub memory_map: Option<&'static [MemoryMapDescriptor]>,
    pub rsdp_address: u64,
    pub kernel_args: *const petroleum::assembly::KernelArgs,
}
unsafe impl Send for BootContext {}
unsafe impl Sync for BootContext {}

impl BootContext {
    const fn empty() -> Self {
        Self {
            framebuffer_config: FullereneFramebufferConfig {
                address: 0,
                width: 0,
                height: 0,
                pixel_format:
                    petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp: 0,
                stride: 0,
            },
            memory_map: None,
            rsdp_address: 0,
            kernel_args: core::ptr::null(),
        }
    }

    pub unsafe fn new(
        kernel_args: *const petroleum::assembly::KernelArgs,
        memory_map: Option<&'static [MemoryMapDescriptor]>,
        rsdp_address: u64,
    ) -> Self {
        let (a, w, h, bpp) = unsafe {
            let a = &*kernel_args;
            (a.fb_address, a.fb_width, a.fb_height, a.fb_bpp)
        };
        Self {
            framebuffer_config: FullereneFramebufferConfig {
                address: a,
                width: w,
                height: h,
                pixel_format:
                    petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp,
                stride: w * (bpp / 8),
            },
            memory_map,
            rsdp_address,
            kernel_args,
        }
    }
    pub fn has_valid_framebuffer(&self) -> bool {
        let c = &self.framebuffer_config;
        c.address >= 0x100000
            && c.width > 0
            && c.width <= 16384
            && c.height > 0
            && c.height <= 16384
            && (c.bpp == 8 || c.bpp == 16 || c.bpp == 24 || c.bpp == 32)
            && c.stride > 0
    }
    pub fn framebuffer_virt(&self) -> u64 {
        self.framebuffer_config.address
            + petroleum::common::memory::get_physical_memory_offset() as u64
    }
    pub fn framebuffer_byte_size(&self) -> u64 {
        self.framebuffer_config.stride as u64
            * self.framebuffer_config.height as u64
            * (self.framebuffer_config.bpp as u64 / 8)
    }
}

static BOOT: Mutex<Option<BootContext>> = Mutex::new(None);
pub fn init_boot() {
    *BOOT.lock() = Some(BootContext::empty());
}
pub fn get_boot() -> &'static Mutex<Option<BootContext>> {
    &BOOT
}
pub fn with_boot<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&BootContext) -> R,
{
    BOOT.lock().as_ref().map(f)
}
