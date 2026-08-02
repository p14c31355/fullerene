//! NVMe (Non-Volatile Memory Express) driver.
//!
//! Implements NVMe SSD access via PCI BAR enumeration, admin/completion
//! queue setup, and doorbell register writes.
//!
//! # References
//! - NVM Express Base Specification Revision 1.4
//! - NVMe over PCIe Transport Specification

use alloc::vec::Vec;
use core::ptr;
use spin::Mutex;

use crate::DriverError;
use crate::driver_context::{DmaAllocation, DriverContext};
use crate::pci::{PciDevice, PciScanner};

static CONTROLLERS: Mutex<Vec<NvmeController>> = Mutex::new(Vec::new());

// ── Controller registers (offset from BAR0) ─────────────────────
const NVME_INTMS: usize = 0x0C;
const NVME_CC: usize = 0x14;
const NVME_CSTS: usize = 0x1C;
const NVME_AQA: usize = 0x24;
const NVME_ASQ: usize = 0x28;
const NVME_ACQ: usize = 0x30;

// ── CC bits ──────────────────────────────────────────────────────
const CC_EN: u32 = 1 << 0;
const CC_IOCQES: u32 = 4 << 20;
const CC_IOSQES: u32 = 6 << 16;

// ── CSTS bits ────────────────────────────────────────────────────
const CSTS_RDY: u32 = 1 << 0;

// ── Queue sizes ──────────────────────────────────────────────────
const ADMIN_QUEUE_DEPTH: u16 = 64;

// ── Submission Queue Entry (64 bytes) ────────────────────────────
#[repr(C)]
struct SqEntry {
    opcode: u8,
    flags: u8,
    command_id: u16,
    nsid: u32,
    rsvd: [u32; 2],
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

// ── Completion Queue Entry (16 bytes) ───────────────────────────
#[repr(C)]
struct CqEntry {
    dw0: u32,
    rsvd: u32,
    sq_head: u16,
    sq_id: u16,
    command_id: u16,
    status: u16,
}

pub struct NvmeController {
    ctx: &'static dyn DriverContext,
    #[allow(dead_code)]
    device: PciDevice,
    mmio: *mut u32,
    #[allow(dead_code)]
    bar0_phys: u64,
    asq: *mut SqEntry,
    asq_iova: u64,
    submission_queue: DmaAllocation,
    #[allow(dead_code)]
    asq_tail: u16,
    acq: *mut CqEntry,
    acq_iova: u64,
    completion_queue: DmaAllocation,
    #[allow(dead_code)]
    acq_head: u16,
    #[allow(dead_code)]
    phase: u16,
}

unsafe impl Send for NvmeController {}
unsafe impl Sync for NvmeController {}

impl Drop for NvmeController {
    fn drop(&mut self) {
        if self.submission_queue.size != 0 {
            self.ctx.release_dma_buffer(self.submission_queue);
        }
        if self.completion_queue.size != 0 {
            self.ctx.release_dma_buffer(self.completion_queue);
        }
    }
}

impl NvmeController {
    pub fn init(ctx: &'static dyn DriverContext, device: PciDevice) -> Option<Self> {
        // Do not destructively probe BAR size here. Firmware-provided BARs
        // must remain untouched until the driver owns the device.
        let bar0 = device.read_bar_info(0)?;
        if bar0.is_io {
            return None;
        }
        let bar0_phys = bar0.address;
        if bar0_phys == 0 {
            return None;
        }
        let bar0_virt = ctx.phys_to_virt(bar0_phys) as *mut u32;

        ctx.map_mmio_region(bar0_phys as usize, bar0_virt as usize, 0x4000)
            .ok()?;

        let mut ctrl = Self {
            ctx,
            device,
            mmio: bar0_virt,
            bar0_phys,
            asq: ptr::null_mut(),
            asq_iova: 0,
            submission_queue: DmaAllocation {
                phys: 0,
                iova: 0,
                size: 0,
                frames: 0,
            },
            asq_tail: 0,
            acq: ptr::null_mut(),
            acq_iova: 0,
            completion_queue: DmaAllocation {
                phys: 0,
                iova: 0,
                size: 0,
                frames: 0,
            },
            acq_head: 0,
            phase: 1,
        };

        ctrl.w32(NVME_CC, 0);
        crate::timing::wait_timeout_us(500_000, || (ctrl.r32(NVME_CSTS) & CSTS_RDY) == 0).ok();

        let device_id = ((ctrl.device.bus as u16) << 8)
            | ((ctrl.device.device as u16) << 3)
            | ctrl.device.function as u16;
        // NVMe submission and completion queues are independent DMA objects.
        // The kernel owns the physical allocation and IOMMU mapping; the
        // driver only receives CPU and device addresses to program into NVMe.
        ctrl.submission_queue = ctx.allocate_dma_buffer(device_id, 4096).ok()?;
        ctrl.completion_queue = match ctx.allocate_dma_buffer(device_id, 4096) {
            Ok(queue) => queue,
            Err(_) => return None,
        };
        let asq_virt = ctx.phys_to_virt(ctrl.submission_queue.phys) as *mut u8;
        let acq_virt = ctx.phys_to_virt(ctrl.completion_queue.phys) as *mut u8;
        unsafe {
            ptr::write_bytes(asq_virt, 0, ctrl.submission_queue.size);
            ptr::write_bytes(acq_virt, 0, ctrl.completion_queue.size);
        }

        ctrl.asq = asq_virt as *mut SqEntry;
        ctrl.asq_iova = ctrl.submission_queue.iova;
        ctrl.acq = acq_virt as *mut CqEntry;
        ctrl.acq_iova = ctrl.completion_queue.iova;

        ctrl.w32(
            NVME_AQA,
            ((ADMIN_QUEUE_DEPTH - 1) as u32) | (((ADMIN_QUEUE_DEPTH - 1) as u32) << 16),
        );
        ctrl.w32(NVME_ASQ, ctrl.asq_iova as u32);
        ctrl.w32(NVME_ASQ + 4, (ctrl.asq_iova >> 32) as u32);
        ctrl.w32(NVME_ACQ, ctrl.acq_iova as u32);
        ctrl.w32(NVME_ACQ + 4, (ctrl.acq_iova >> 32) as u32);

        ctrl.w32(NVME_CC, CC_EN | CC_IOCQES | CC_IOSQES);
        if crate::timing::wait_timeout_us(500_000, || (ctrl.r32(NVME_CSTS) & CSTS_RDY) != 0)
            .is_err()
        {
            log::info!("NVMe: controller failed to become ready");
            return None;
        }

        ctrl.w32(NVME_INTMS, 0xFFFFFFFF);

        log::info!("NVMe: controller ready");
        Some(ctrl)
    }

    fn r32(&self, off: usize) -> u32 {
        let val = unsafe { ptr::read_volatile(self.mmio.add(off / 4)) };
        if val == 0xFFFF_FFFF {
            log::warn!("NVMe: MMIO read at offset {:#x} returned 0xFFFF_FFFF", off);
        }
        val
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(self.mmio.add(off / 4), v);
        }
    }
}

/// Initialise all NVMe controllers found on the PCI bus.
pub fn init(ctx: &'static dyn DriverContext) {
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        if dev.class_code == 0x01 && dev.subclass == 0x08 {
            log::info!(
                "NVMe: found device {:#06x}:{:#06x}",
                dev.vendor_id,
                dev.device_id
            );
            let _ = init_device(ctx, dev.clone());
        }
    }
    if CONTROLLERS.lock().is_empty() {
        log::info!("NVMe: no NVMe devices found");
    }
}

/// Initialize exactly one NVMe PCI function and return its stable controller
/// index (`nvme0`, `nvme1`, ...).  The kernel request/completion queues call
/// this function; the old scan-all entry point remains for standalone users.
pub fn init_device(
    ctx: &'static dyn DriverContext,
    device: PciDevice,
) -> Result<usize, DriverError> {
    if device.class_code != 0x01 || device.subclass != 0x08 {
        return Err(DriverError::DeviceNotFound);
    }

    // Initialization requests may be retried by the kernel.  Keep the
    // controller index stable and avoid resetting an already-ready device.
    let existing = {
        let controllers = CONTROLLERS.lock();
        controllers.iter().position(|controller| {
            controller.device.bus == device.bus
                && controller.device.device == device.device
                && controller.device.function == device.function
        })
    };
    if let Some(index) = existing {
        return Ok(index);
    }

    if !device.enable_memory_access() {
        return Err(DriverError::DeviceFault);
    }
    let ctrl = NvmeController::init(ctx, device).ok_or(DriverError::NotReady)?;
    let mut controllers = CONTROLLERS.lock();
    let index = controllers.len();
    controllers.push(ctrl);
    Ok(index)
}

/// Number of controllers that completed initialization.
pub fn controller_count() -> usize {
    CONTROLLERS.lock().len()
}
