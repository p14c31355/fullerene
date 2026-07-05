//! VirtIO-GPU hardware initialisation — low-level PCI probe, BAR mapping,
//! queue setup, and display negotiation.
//!
//! This module handles the **hardware mechanism** portion of VirtIO-GPU
//! initialisation (per nitrogen's design philosophy).  The higher-level
//! framebuffer→renderer wiring that depends on `petroleum::graphics` types
//! remains in the kernel's `drivers/virtio_gpu.rs`.
//!
//! Called by the kernel's `drivers/virtio_gpu::init()`.

use alloc::boxed::Box;

use crate::driver_context::DriverContext;
use crate::pci::{PciConfigSpace, PciScanner};
use crate::virtio::cap::{self, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};
use crate::virtio::gpu::{self, VirtioGpu, VringAvail, VringDesc, VringUsed};

/// RAII guard that holds a contiguous frame allocation.
///
/// The guard frees the allocated frames on drop unless disarmed with
/// [`forget`](Self::forget).
struct ContiguousFrameGuard<'c> {
    phys: u64,
    pages: usize,
    ctx: &'c dyn DriverContext,
}

impl<'c> ContiguousFrameGuard<'c> {
    fn allocate(ctx: &'c dyn DriverContext, pages: usize) -> Option<Self> {
        let phys = ctx.allocate_contiguous_frames(pages).ok()?;
        Some(Self { phys, pages, ctx })
    }
    fn phys(&self) -> u64 {
        self.phys
    }
    fn forget(mut self) -> u64 {
        let phys = self.phys;
        self.pages = 0;
        phys
    }
}

impl<'c> Drop for ContiguousFrameGuard<'c> {
    fn drop(&mut self) {
        if self.pages > 0 {
            self.ctx.free_contiguous_frames(self.phys, self.pages);
        }
    }
}

/// Fixed virtual addresses for VirtIO MMIO BARs.
pub const COMMON_VIRT_BASE: usize = 0xffff800060000000;
pub const NOTIFY_VIRT_BASE: usize = 0xffff800070000000;

/// Result of hardware-level VirtIO-GPU initialisation.
///
/// The caller (kernel) must map the framebuffer and create a renderer
/// using `petroleum::graphics` types.
pub struct VirtioGpuInitResult {
    pub gpu: Box<VirtioGpu>,
    pub fb_phys: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_stride: u32,
    pub fb_bpp: u32,
    pub fb_virt_base: u64,
    pub fb_byte_size: u64,
}

/// Probe PCI, map BARs, allocate queues, negotiate display.
///
/// Returns hardware initialisation results.  The caller must still
/// map the framebuffer pages and create a `UefiFramebufferWriter`.
pub fn init(ctx: &dyn DriverContext) -> Option<VirtioGpuInitResult> {
    // 1. PCI probe
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    let gpu_dev = scanner
        .get_devices()
        .iter()
        .find(|d| d.vendor_id == 0x1af4 && d.device_id == 0x1050)
        .cloned()?;
    log::info!(
        "virtio-gpu: found at {:02x}:{:02x}.{:01x}",
        gpu_dev.bus,
        gpu_dev.device,
        gpu_dev.function
    );

    // 2. Capability parsing
    let caps = cap::get_virtio_caps(&gpu_dev);
    cap::dump_capabilities(&gpu_dev);
    let common_cap = caps
        .iter()
        .find(|c| c.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG)
        .cloned()?;
    let notify_cap = caps
        .iter()
        .find(|c| c.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG)
        .cloned()?;

    // 3. BAR info
    let bar_info = gpu_dev.get_bar_info(common_cap.bar)?;
    let notify_bar_info = gpu_dev.get_bar_info(notify_cap.bar)?;
    gpu_dev.enable_memory_access();

    let cmd = PciConfigSpace::read_from_device(gpu_dev.bus, gpu_dev.device, gpu_dev.function)?;
    let val = (cmd.status as u32) << 16 | (cmd.command as u32 | 0x0004);
    PciConfigSpace::write_config_dword_raw(
        gpu_dev.bus,
        gpu_dev.device,
        gpu_dev.function,
        0x04,
        val,
    );

    // 4. Map MMIO BARs
    ctx.map_mmio_region(
        bar_info.address as usize,
        COMMON_VIRT_BASE,
        bar_info.size as usize,
    )
    .ok()?;
    ctx.map_mmio_region(
        notify_bar_info.address as usize,
        NOTIFY_VIRT_BASE,
        notify_bar_info.size as usize,
    )
    .ok()?;

    let common_ptr = (COMMON_VIRT_BASE + common_cap.offset as usize) as *mut u32;
    let notify_ptr = (NOTIFY_VIRT_BASE + notify_cap.offset as usize) as *mut u32;

    // 5. Allocate command/response buffers
    let cmd_guard = ContiguousFrameGuard::allocate(ctx, 1)?;
    let cmd_phys = cmd_guard.phys();
    let cmd_buf = ctx.phys_to_virt(cmd_phys) as *mut u8;
    let resp_guard = ContiguousFrameGuard::allocate(ctx, 1)?;
    let resp_phys = resp_guard.phys();
    let resp_buf = ctx.phys_to_virt(resp_phys) as *mut u8;
    unsafe {
        core::ptr::write_bytes(cmd_buf, 0, 4096);
        core::ptr::write_bytes(resp_buf, 0, 4096);
    }

    // 6. Initialise GPU device
    let mut gpu = gpu::init_virtio_gpu(
        common_ptr,
        notify_ptr,
        gpu_dev.clone(),
        common_cap.bar,
        cmd_buf,
        cmd_phys,
        4096,
        resp_buf,
        resp_phys,
        4096,
    )?;

    // 7. Queue memory
    let desc_guard = ContiguousFrameGuard::allocate(ctx, 1)?;
    let desc_virt = ctx.phys_to_virt(desc_guard.phys()) as *mut VringDesc;
    let avail_guard = ContiguousFrameGuard::allocate(ctx, 1)?;
    let avail_virt = ctx.phys_to_virt(avail_guard.phys()) as *mut VringAvail;
    let used_guard = ContiguousFrameGuard::allocate(ctx, 1)?;
    let used_virt = ctx.phys_to_virt(used_guard.phys()) as *mut VringUsed;
    unsafe { gpu.setup_queue(
        0,
        desc_virt,
        desc_guard.phys(),
        avail_virt,
        avail_guard.phys(),
        used_virt,
        used_guard.phys(),
    ) };

    cmd_guard.forget();
    resp_guard.forget();
    desc_guard.forget();
    avail_guard.forget();
    used_guard.forget();

    // 8. Return result for the kernel to finalise framebuffer mapping
    //    and renderer creation.
    Some(VirtioGpuInitResult {
        gpu: Box::new(gpu),
        fb_phys: 0, // filled in by caller
        fb_width: 0,
        fb_height: 0,
        fb_stride: 0,
        fb_bpp: 0,
        fb_virt_base: 0,
        fb_byte_size: 0,
    })
}
