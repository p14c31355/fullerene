//! VirtIO-GPU initialisation entry point.
//!
//! Probes PCI, maps BARs, allocates buffers, negotiates display,
//! maps the framebuffer with WC, and returns both the GPU handle
//! and the primary framebuffer renderer.
//!
//! Called by [`crate::graphics::init_graphics`].

use alloc::boxed::Box;
use nitrogen::pci::{PciConfigSpace, PciScanner};
use nitrogen::virtio::cap::{self, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};
use nitrogen::virtio::gpu::{self, VirtioGpu, VringAvail, VringDesc, VringUsed};
use petroleum::graphics::UefiFramebufferWriter;
use petroleum::page_table::constants::get_frame_allocator_mut;

/// Fixed virtual addresses for VirtIO MMIO BARs.
const COMMON_VIRT_BASE: usize = 0xffff800060000000;
const NOTIFY_VIRT_BASE: usize = 0xffff800070000000;

/// RAII guard that frees a contiguous frame allocation on drop.
struct ContiguousFrameGuard {
    phys: u64,
    pages: usize,
}

impl ContiguousFrameGuard {
    fn allocate(pages: usize) -> Option<Self> {
        let phys = get_frame_allocator_mut()
            .allocate_contiguous_frames(pages)
            .ok()? as u64;
        Some(Self { phys, pages })
    }
    fn phys(&self) -> u64 {
        self.phys
    }
    /// Disarm the guard so the frames persist after return.
    fn forget(mut self) -> u64 {
        let phys = self.phys;
        self.pages = 0;
        phys
    }
}

impl Drop for ContiguousFrameGuard {
    fn drop(&mut self) {
        if self.pages > 0 {
            get_frame_allocator_mut().free_contiguous_frames(self.phys, self.pages);
        }
    }
}

/// Complete VirtIO-GPU initialisation: probe → queue → display → renderer.
///
/// Returns the GPU handle and the framebuffer renderer on success,
/// or `None` if any step fails (caller falls back to GOP/VGA).
pub fn init() -> Option<(Box<VirtioGpu>, UefiFramebufferWriter)> {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;

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
    let val = (cmd.status as u32) << 16 | (cmd.command as u32 | 0x0004); // bus-master
    PciConfigSpace::write_config_dword_raw(
        gpu_dev.bus,
        gpu_dev.device,
        gpu_dev.function,
        0x04,
        val,
    );

    // 4. Map MMIO BARs
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        if mm
            .map_mmio_region(
                bar_info.address as usize,
                COMMON_VIRT_BASE,
                bar_info.size as usize,
            )
            .is_err()
            || mm
                .map_mmio_region(
                    notify_bar_info.address as usize,
                    NOTIFY_VIRT_BASE,
                    notify_bar_info.size as usize,
                )
                .is_err()
        {
            return None;
        }
    }

    let common_ptr = (COMMON_VIRT_BASE + common_cap.offset as usize) as *mut u32;
    let notify_ptr = (NOTIFY_VIRT_BASE + notify_cap.offset as usize) as *mut u32;

    // 5. Allocate command/response buffers (RAII-guarded)
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
    let desc_guard = ContiguousFrameGuard::allocate(1)?;
    let desc_virt = (desc_guard.phys() + off) as *mut VringDesc;
    let avail_guard = ContiguousFrameGuard::allocate(1)?;
    let avail_virt = (avail_guard.phys() + off) as *mut VringAvail;
    let used_guard = ContiguousFrameGuard::allocate(1)?;
    let used_virt = (used_guard.phys() + off) as *mut VringUsed;
    gpu.setup_queue(
        0,
        desc_virt,
        desc_guard.phys(),
        avail_virt,
        avail_guard.phys(),
        used_virt,
        used_guard.phys(),
    );

    // Disarm guards — GPU+queue owns these buffers
    cmd_guard.forget();
    resp_guard.forget();
    desc_guard.forget();
    avail_guard.forget();
    used_guard.forget();

    // 8. Framebuffer info
    let fb_config = {
        let opt = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|m| m.lock().clone());
        opt.unwrap_or(petroleum::common::FullereneFramebufferConfig {
            address: 0x40000000,
            width: 1024,
            height: 768,
            stride: 1024,
            pixel_format:
                petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            bpp: 32,
        })
    };
    let fb_phys = fb_config.address;
    let fb_virt = fb_phys + off;
    let fb_byte_size = (fb_config.stride * fb_config.height * (fb_config.bpp / 8)) as u64;
    let fb_pages = ((fb_byte_size + 4095) / 4096) as usize;

    // 9. Map framebuffer WC
    let wc_flags = x86_64::structures::paging::PageTableFlags::WRITE_THROUGH
        | x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        for i in 0..fb_pages {
            if mm
                .safe_map_page(
                    (fb_virt + (i * 4096) as u64) as usize,
                    (fb_phys + (i * 4096) as u64) as usize,
                    wc_flags,
                )
                .is_err()
            {
                log::error!("virtio_gpu: failed to map fb page {}/{}", i, fb_pages);
                return None;
            }
        }
    }

    // 10. Negotiate display
    let fb_size = (fb_config.stride * fb_config.height * (fb_config.bpp / 8)) as u32;
    gpu.init_display(fb_config.width, fb_config.height, fb_phys, fb_size)
        .ok()?;

    // 11. Create renderer
    let fb_info = petroleum::graphics::color::FramebufferInfo {
        address: fb_virt,
        width: fb_config.width,
        height: fb_config.height,
        stride: fb_config.stride,
        pixel_format: Some(fb_config.pixel_format),
        colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
    };
    let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
    let renderer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);

    log::info!(
        "virtio-gpu: display {}x{} ready",
        fb_config.width,
        fb_config.height
    );
    Some((Box::new(gpu), renderer))
}
