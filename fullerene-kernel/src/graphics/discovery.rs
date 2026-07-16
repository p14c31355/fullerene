//! Framebuffer discovery — probes hardware for GOP parameters.
//!
//! Three probe strategies in priority order:
//! 1. An immutable `.data`-section snapshot (stored by `efi_main_stage2`)
//! 2. `KernelArgs` struct via the preserved virtual address
//! 3. PCI BAR0 scan

use petroleum::common::EfiGraphicsPixelFormat;
use petroleum::graphics::boot_screen::BootFramebuffer;
use spin::Once;

/// Raw probe result — physical address, dimensions, stride (bytes), pixel format.
pub struct FramebufferProbeResult {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

/// GOP parameters copied before the UEFI world switch.
///
/// This is deliberately a plain, copyable value: it must survive the shallow
/// page-table clone and remain usable by the allocation-free panic path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BootFramebufferParams {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bpp: u32,
    pub pixel_format: u32,
}

impl BootFramebufferParams {
    fn probe_result(self) -> Option<FramebufferProbeResult> {
        let minimum_stride = self.width.checked_mul(4)?;
        let framebuffer_bytes = u64::from(self.stride).checked_mul(u64::from(self.height))?;
        if !(0x10_0000..1 << 52).contains(&self.phys)
            || !(80..=16_384).contains(&self.width)
            || !(25..=16_384).contains(&self.height)
            || self.bpp != 32
            || self.stride < minimum_stride
            || self.stride % 4 != 0
            || framebuffer_bytes > 1 << 30
        {
            return None;
        }
        let pixel_format = match self.pixel_format {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => return None,
        };
        Some(FramebufferProbeResult {
            phys: self.phys,
            width: self.width,
            height: self.height,
            stride: self.stride,
            pixel_format,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootSnapshotError {
    ConflictingInitialization,
}

/// Immutable GOP snapshot stored in `.data` during `efi_main_stage2`.
#[unsafe(link_section = ".data")]
static BOOT_FRAMEBUFFER: Once<BootFramebufferParams> = Once::new();

/// Immutable KernelArgs virtual address preserved in `.data`.
#[unsafe(link_section = ".data")]
static KERNEL_ARGS_VA: Once<u64> = Once::new();

fn store_snapshot<T: Copy + Eq>(slot: &Once<T>, value: T) -> Result<(), BootSnapshotError> {
    let stored = slot.call_once(|| value);
    if *stored == value {
        Ok(())
    } else {
        Err(BootSnapshotError::ConflictingInitialization)
    }
}

/// Return a copy of the immutable boot framebuffer snapshot.
pub(crate) fn boot_framebuffer_params() -> Option<BootFramebufferParams> {
    BOOT_FRAMEBUFFER.get().copied()
}

/// Return the preserved KernelArgs virtual address.
pub(crate) fn kernel_args_va() -> Option<u64> {
    KERNEL_ARGS_VA.get().copied()
}

/// Store the virtual address of KernelArgs.  Called from `efi_main_stage2`.
pub fn store_args_va(va: u64) -> Result<(), BootSnapshotError> {
    let result = store_snapshot(&KERNEL_ARGS_VA, va);
    petroleum::serial::_print(format_args!("[store_args] va=0x{va:x}\n"));
    result
}

/// Store GOP parameters from the bootloader's KernelArgs.
/// Called from `efi_main_stage2` while `args_ptr` is valid.
pub fn store_boot_fb_params(
    phys: u64,
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    pixel_format: u32,
) -> Result<(), BootSnapshotError> {
    let result = store_snapshot(
        &BOOT_FRAMEBUFFER,
        BootFramebufferParams {
            phys,
            width,
            height,
            stride,
            bpp,
            pixel_format,
        },
    );
    petroleum::serial::_print(format_args!(
        "[store_fb] {width}x{height} stride={stride} phys=0x{phys:x} bpp={bpp} fmt={pixel_format}\n"
    ));
    result
}

/// Return the framebuffer through the bootstrap's direct mapping.
///
/// The initial page table maps the first 64 GiB into the higher-half direct
/// map, which is shared by every process page table. Early Bellows diagnostics
/// may use the identity alias, but kernel-owned rendering must not retain it.
pub fn direct_boot_framebuffer() -> Option<BootFramebuffer> {
    let params = boot_framebuffer_params()?;
    let size = u64::from(params.stride).checked_mul(u64::from(params.height))?;
    if params.phys.checked_add(size)? > 64 * 1024 * 1024 * 1024 {
        return None;
    }
    let direct_map_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    let direct_map_address = params.phys.checked_add(direct_map_offset)?;
    BootFramebuffer::new(
        direct_map_address,
        params.width,
        params.height,
        params.stride,
        params.bpp,
        params.pixel_format,
    )
}

/// Discovery engine — tries each probe strategy in order.
pub struct FramebufferDiscovery;

impl FramebufferDiscovery {
    /// Probe the immutable `.data` snapshot saved by `efi_main_stage2`.
    pub fn probe_data_globals() -> Option<FramebufferProbeResult> {
        let result = boot_framebuffer_params()?.probe_result()?;
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] .data globals valid\n");
        Some(result)
    }

    /// Probe `KernelArgs` via its preserved virtual address.
    pub fn probe_kernel_args() -> Option<FramebufferProbeResult> {
        let args_va = kernel_args_va()?;
        if args_va < 0xFFFF_8000_0000_0000 || args_va % core::mem::align_of::<petroleum::assembly::KernelArgs>() as u64 != 0 {
            return None;
        }
        let args = unsafe { &*(args_va as *const petroleum::assembly::KernelArgs) };
        let mut buf = [0u8; 64];
        let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] KernelArgs fb=0x");
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        if args.fb_address < 0x100000
            || args.fb_width == 0
            || args.fb_width > 16384
            || args.fb_height == 0
            || args.fb_height > 16384
            || args.fb_bpp != 32
        {
            return None;
        }
        let stride = if args.fb_stride > 0 {
            args.fb_stride
        } else {
            args.fb_width.saturating_mul(4)
        };
        let pixel_format = match args.fb_pixel_format {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] KernelArgs valid\n");
        Some(FramebufferProbeResult {
            phys: args.fb_address,
            width: args.fb_width,
            height: args.fb_height,
            stride,
            pixel_format,
        })
    }

    /// Probe PCI BAR0 for a VGA-compatible display controller.
    pub fn probe_pci(pci_devices: &[nitrogen::pci::PciDevice]) -> Option<FramebufferProbeResult> {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] scanning PCI\n");
        for dev in pci_devices {
            let vendor = nitrogen::pci::PciConfigSpace::read_config_word(dev.bus, dev.device, 0, 0);
            if vendor == 0xFFFF || vendor == 0x0000 {
                continue;
            }
            let class =
                nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0B);
            let subclass =
                nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0A);
            if class == 0x03 && subclass == 0x00 {
                // Intel GPUs (vendor 0x8086) use BAR2 for GTT aperture
                // (physical framebuffer behind BAR2 offset + 0).  Other
                // vendors use BAR0.
                let bar_reg = if vendor == 0x8086 { 0x18 } else { 0x10 };
                let bar = nitrogen::pci::PciConfigSpace::read_config_dword(
                    dev.bus, dev.device, 0, bar_reg,
                );
                let fb_phys = if (bar & 0x6) == 0x4 {
                    let bar_upper = nitrogen::pci::PciConfigSpace::read_config_dword(
                        dev.bus,
                        dev.device,
                        0,
                        bar_reg + 4,
                    );
                    ((bar_upper as u64) << 32) | ((bar & 0xFFFFFFF0) as u64)
                } else {
                    (bar & 0xFFFFFFF0) as u64
                };
                if fb_phys >= 0x100000 {
                    return Some(FramebufferProbeResult {
                        phys: fb_phys,
                        width: 1280,
                        height: 800,
                        stride: 1280 * 4,
                        pixel_format: EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
                    });
                }
            }
        }
        None
    }

    /// Run all probe strategies in priority order.
    pub fn discover(pci_devices: &[nitrogen::pci::PciDevice]) -> Option<FramebufferProbeResult> {
        Self::probe_data_globals()
            .or_else(|| Self::probe_kernel_args())
            .or_else(|| Self::probe_pci(pci_devices))
    }
}

#[cfg(test)]
mod tests {
    use super::{BootFramebufferParams, BootSnapshotError, store_snapshot};
    use spin::Once;

    fn valid_params() -> BootFramebufferParams {
        BootFramebufferParams {
            phys: 0x100_0000,
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            bpp: 32,
            pixel_format: 1,
        }
    }

    #[test]
    fn snapshot_accepts_idempotent_initialization() {
        let snapshot = Once::new();

        assert_eq!(store_snapshot(&snapshot, 42_u64), Ok(()));
        assert_eq!(store_snapshot(&snapshot, 42_u64), Ok(()));
        assert_eq!(snapshot.get(), Some(&42));
    }

    #[test]
    fn snapshot_rejects_conflicting_initialization() {
        let snapshot = Once::new();

        assert_eq!(store_snapshot(&snapshot, 42_u64), Ok(()));
        assert_eq!(
            store_snapshot(&snapshot, 7_u64),
            Err(BootSnapshotError::ConflictingInitialization)
        );
        assert_eq!(snapshot.get(), Some(&42));
    }

    #[test]
    fn framebuffer_snapshot_validation_accepts_boot_gop_layout() {
        let result = valid_params().probe_result().expect("valid GOP snapshot");

        assert_eq!(result.phys, 0x100_0000);
        assert_eq!(result.width, 1920);
        assert_eq!(result.height, 1080);
        assert_eq!(result.stride, 1920 * 4);
    }

    #[test]
    fn framebuffer_snapshot_validation_rejects_short_stride() {
        let mut params = valid_params();
        params.stride = params.width * 4 - 4;

        assert!(params.probe_result().is_none());
    }
}
