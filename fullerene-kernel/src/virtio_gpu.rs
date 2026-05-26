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

    // Allocate command/response buffers (1 page each)
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let fa = petroleum::page_table::constants::get_frame_allocator_mut();
    let cmd_phys = fa.allocate_contiguous_frames(1).ok()? as u64;
    let cmd_buf = (cmd_phys + off) as *mut u8;
    let resp_phys = fa.allocate_contiguous_frames(1).ok()? as u64;
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

    // Queue memory
    let desc_phys = fa.allocate_contiguous_frames(1).expect("virtio-gpu: desc") as u64;
    let desc_virt = (desc_phys + off) as *mut gpu::VringDesc;
    let avail_phys = fa.allocate_contiguous_frames(1).expect("virtio-gpu: avail") as u64;
    let avail_virt = (avail_phys + off) as *mut gpu::VringAvail;
    let used_phys = fa.allocate_contiguous_frames(1).expect("virtio-gpu: used") as u64;
    let used_virt = (used_phys + off) as *mut gpu::VringUsed;

    gpu.setup_queue(0, desc_virt, desc_phys, avail_virt, avail_phys, used_virt, used_phys);

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