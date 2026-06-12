//! BootContext — Boot-time information passed from UEFI/BIOS to the kernel.
//!
//! Consolidates: framebuffer config, memory map, RSDP, kernel args.
//!
//! Previously scattered across:
//! - `petroleum::FULLERENE_FRAMEBUFFER_CONFIG`
//! - `crate::heap::MEMORY_MAP`
//! - `petroleum::transition::KERNEL_ARGS`
//!
//! # Usage
//!
//! ```rust,ignore
//! let boot = BootContext::from_kernel_args(args_ptr, memory_map);
//! let fb = boot.framebuffer_info(); // → FramebufferInfo
//! let rsdp = boot.rsdp_address();   // → u64
//! ```

use petroleum::common::uefi::FullereneFramebufferConfig;
use petroleum::page_table::memory_map::MemoryMapDescriptor;
use spin::Mutex;

/// Boot-time context passed from the bootloader to the kernel.
///
/// Owns all information that is collected during boot and never
/// changes afterwards.  Once constructed, the kernel should treat
/// this as read-only.
pub struct BootContext {
    /// Framebuffer configuration (address, dimensions, pixel format).
    pub framebuffer_config: FullereneFramebufferConfig,

    /// EFI memory map descriptors (converted to static slice).
    /// Held behind a Mutex for the transition period; callers should
    /// prefer `memory_map()` which returns an `Option<&[...]>`.
    memory_map: Option<&'static [MemoryMapDescriptor]>,

    /// RSDP (Root System Description Pointer) physical address.
    pub rsdp_address: u64,

    /// Kernel arguments pointer (for accessing fb, system table, etc.).
    /// Safe to dereference while the UEFI runtime services are active.
    pub kernel_args: *const petroleum::assembly::KernelArgs,
}

// BootContext is Send+Sync because all its fields are trivially so.
unsafe impl Send for BootContext {}
unsafe impl Sync for BootContext {}

impl BootContext {
    /// Construct a `BootContext` from kernel arguments and a pre-parsed
    /// memory map.
    ///
    /// # Safety
    ///
    /// `kernel_args` must be a valid pointer to a `KernelArgs` struct
    /// that outlives the context.
    pub unsafe fn new(
        kernel_args: *const petroleum::assembly::KernelArgs,
        memory_map: Option<&'static [MemoryMapDescriptor]>,
        rsdp_address: u64,
    ) -> Self {
        Self {
            framebuffer_config: FullereneFramebufferConfig {
                address: unsafe { (*kernel_args).fb_address },
                width: unsafe { (*kernel_args).fb_width },
                height: unsafe { (*kernel_args).fb_height },
                pixel_format:
                    petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp: unsafe { (*kernel_args).fb_bpp },
                stride: unsafe {
                    let w = (*kernel_args).fb_width;
                    let bpp = (*kernel_args).fb_bpp;
                    w * (bpp / 8)
                },
            },
            memory_map,
            rsdp_address,
            kernel_args,
        }
    }

    /// Return a reference to the memory map descriptors, if available.
    pub fn memory_map(&self) -> Option<&[MemoryMapDescriptor]> {
        self.memory_map
    }

    /// Returns `true` if the framebuffer config appears valid.
    pub fn has_valid_framebuffer(&self) -> bool {
        let cfg = &self.framebuffer_config;
        cfg.address >= 0x100000
            && cfg.width > 0
            && cfg.width <= 16384
            && cfg.height > 0
            && cfg.height <= 16384
            && (cfg.bpp == 8 || cfg.bpp == 16 || cfg.bpp == 24 || cfg.bpp == 32)
            && cfg.stride > 0
    }

    /// Compute the framebuffer virtual address (physical + offset).
    pub fn framebuffer_virt(&self) -> u64 {
        let offset = petroleum::common::memory::get_physical_memory_offset() as u64;
        self.framebuffer_config.address + offset
    }

    /// Compute the total framebuffer byte size (stride × height × bpp/8).
    pub fn framebuffer_byte_size(&self) -> u64 {
        self.framebuffer_config.stride as u64
            * self.framebuffer_config.height as u64
            * (self.framebuffer_config.bpp as u64 / 8)
    }
}

/// Global boot context.  Set once during UEFI/BIOS boot and
/// never modified afterwards.
static BOOT_CONTEXT: Mutex<Option<BootContext>> = Mutex::new(None);

/// Initialise the global `BootContext`.
///
/// # Safety
///
/// Must be called exactly once during boot, before any other code
/// reads from the context.
pub fn init_boot_context(
    kernel_args: *const petroleum::assembly::KernelArgs,
    memory_map: Option<&'static [MemoryMapDescriptor]>,
    rsdp_address: u64,
) {
    let ctx = unsafe { BootContext::new(kernel_args, memory_map, rsdp_address) };
    *BOOT_CONTEXT.lock() = Some(ctx);
}

/// Get a reference to the global `BootContext`.
///
/// # Panics
///
/// Panics if `init_boot_context` has not been called.
pub fn get_boot_context() -> &'static Mutex<Option<BootContext>> {
    &BOOT_CONTEXT
}

/// Convenience: execute a closure with a reference to the boot context.
pub fn with_boot_context<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&BootContext) -> R,
{
    BOOT_CONTEXT.lock().as_ref().map(f)
}