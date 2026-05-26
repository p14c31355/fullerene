//! VirtIO-GPU stabilization wrapper.
//!
//! Consolidates PCI capability probing, BAR MMIO mapping, queue setup
//! and display negotiation into a single initialisation entry point.
//! Called by [`crate::graphics::init_graphics`] when a VirtIO-GPU
//! PCI device (vendor 0x1AF4, device 0x1050) is detected.
//!
//! # Fallback
//!
//! Returns `None` gracefully when no device is present so the caller
//! falls back to UEFI GOP / VESA.

use alloc::boxed::Box;
use nitrogen::pci::{PciConfigSpace, PciDevice, PciScanner};
use nitrogen::virtio::gpu;
use nitrogen::virtio::gpu::VirtioGpu;

/// RAII guard that frees a contiguous frame allocation on drop.
///
/// Call [`Self::forget`] to prevent deallocation when the frame is
/// still in use (e.g. handed off to hardware).
struct ContiguousFrameGuard {
    phys: u64,
    pages: usize,
}

impl ContiguousFrameGuard {
    /// Allocate `pages` contiguous frames. Returns `None` on OOM.
    fn allocate(pages: usize) -> Option<Self> {
        let phys = petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(pages)
            .ok()? as u64;
        Some(Self { phys, pages })
    }

    fn phys(&self) -> u64 {
        self.phys
    }

    /// Disarm the guard, returning ownership of the underlying
    /// physical frames so the caller is responsible for freeing them.
    fn forget(mut self) -> u64 {
        let phys = self.phys;
        // Prevent Drop from freeing the frames
        self.pages = 0;
        phys
    }
}

impl Drop for ContiguousFrameGuard {
    fn drop(&mut self) {
        if self.pages > 0 {
            petroleum::page_table::constants::get_frame_allocator_mut()
                .free_contiguous_frames(self.phys, self.pages);
        }
    }
}

/// Probe PCI for a VirtIO-GPU device and initialise it.
///
/// Returns `Some(VirtioGpu)` on success, `None` if no device is found
/// or initialisation fails.
pub fn init(common_virt: u64, notify_virt: u64, fb_addr: u64, fb_w: u32, fb_h: u32, fb_stride: u32) -> Option<Box<VirtioGpu>> {
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    let gpu_dev = scanner.get_devices().iter()
        .find(|d| d.vendor_id == 0x1af4 && d.device_id == 0x1050)
        .cloned()?;

    log::info!("virtio-gpu: found device at {:02x}:{:02x}.{:01x}",
        gpu_dev.bus, gpu_dev.device, gpu_dev.function);

    let caps = nitrogen::virtio::cap::get_virtio_caps(&gpu_dev);
    nitrogen::virtio::cap::dump_capabilities(&gpu_dev);

    let common_cap = caps.iter().find(|c| c.cfg_type == nitrogen::virtio::cap::VIRTIO_PCI_CAP_COMMON_CFG)?;
    let notify_cap = caps.iter().find(|c| c.cfg_type == nitrogen::virtio::cap::VIRTIO_PCI_CAP_NOTIFY_CFG)?;

    gpu_dev.enable_memory_access();

    let common_virt_ptr = (common_virt + common_cap.offset as u64) as *mut u32;
    let notify_virt_ptr = (notify_virt + notify_cap.offset as u64) as *mut u32;

    // Allocate command/response buffers (1 page each) with RAII guards.
    // If any allocation fails, previously-allocated guards will auto-free.
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let cmd_guard = ContiguousFrameGuard::allocate(1)?;
    let cmd_phys = cmd_guard.phys();
    let cmd_buf = (cmd_phys + off) as *mut u8;
    let resp_guard = ContiguousFrameGuard::allocate(1)?;
    let resp_phys = resp_guard.phys();
    let resp_buf = (resp_phys + off) as *mut u8;
    unsafe {
        core::ptr::write_bytes(cmd_buf, 0, 4096);
        core::ptr::write_bytes(resp_buf, 0, 4096);
    }

    let gpu_result = gpu::init_virtio_gpu(
        common_virt_ptr,
        notify_virt_ptr,
        gpu_dev.clone(),
        common_cap.bar,
        cmd_buf, cmd_phys, 4096,
        resp_buf, resp_phys, 4096,
    );

    let mut gpu = gpu_result?;

    // Queue memory — allocate with RAII guards.
    let desc_guard = ContiguousFrameGuard::allocate(1)?;
    let desc_phys = desc_guard.phys();
    let desc_virt = (desc_phys + off) as *mut gpu::VringDesc;
    let avail_guard = ContiguousFrameGuard::allocate(1)?;
    let avail_phys = avail_guard.phys();
    let avail_virt = (avail_phys + off) as *mut gpu::VringAvail;
    let used_guard = ContiguousFrameGuard::allocate(1)?;
    let used_phys = used_guard.phys();
    let used_virt = (used_phys + off) as *mut gpu::VringUsed;

    gpu.setup_queue(0, desc_virt, desc_phys, avail_virt, avail_phys, used_virt, used_phys);

    // All allocations succeeded — disarm guards so frames persist after
    // we return. The GPU device (and its queue) own these buffers now.
    cmd_guard.forget();
    resp_guard.forget();
    desc_guard.forget();
    avail_guard.forget();
    used_guard.forget();

    // Negotiate display
    let fb_size = fb_stride * fb_h;
    match gpu.init_display(fb_w, fb_h, fb_addr, fb_size) {
        Ok(()) => {
            log::info!("virtio-gpu: display {}x{} initialised", fb_w, fb_h);
            Some(Box::new(gpu))
        }
        Err(e) => {
            log::info!("virtio-gpu: init_display failed: {:?}", e);
            None
        }
    }
}