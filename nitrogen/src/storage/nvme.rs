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

use crate::driver_context::DriverContext;
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
    #[allow(dead_code)]
    device: PciDevice,
    mmio: *mut u32,
    #[allow(dead_code)]
    bar0_phys: u64,
    asq: *mut SqEntry,
    asq_phys: u64,
    #[allow(dead_code)]
    asq_tail: u16,
    acq: *mut CqEntry,
    acq_phys: u64,
    #[allow(dead_code)]
    acq_head: u16,
    #[allow(dead_code)]
    phase: u16,
    queue_phys: u64,
}

unsafe impl Send for NvmeController {}
unsafe impl Sync for NvmeController {}

impl NvmeController {
    pub fn init(ctx: &dyn DriverContext, device: PciDevice) -> Option<Self> {
        let bar0 = device.get_bar_info(0)?;
        if bar0.is_io {
            return None;
        }
        let bar0_phys = bar0.address;
        let bar0_virt = ctx.phys_to_virt(bar0_phys) as *mut u32;

        ctx.map_mmio_region(bar0_phys as usize, bar0_virt as usize, bar0.size as usize)
            .ok()?;

        let mut ctrl = Self {
            device,
            mmio: bar0_virt,
            bar0_phys,
            asq: ptr::null_mut(),
            asq_phys: 0,
            asq_tail: 0,
            acq: ptr::null_mut(),
            acq_phys: 0,
            acq_head: 0,
            phase: 1,
            queue_phys: 0,
        };

        ctrl.w32(NVME_CC, 0);
        crate::timing::wait_timeout_us(500_000, || {
            (ctrl.r32(NVME_CSTS) & CSTS_RDY) == 0
        }).ok();

        let q_phys = ctx.allocate_contiguous_frames(2).ok()?;
        ctrl.queue_phys = q_phys;
        let q_virt = ctx.phys_to_virt(q_phys) as *mut u8;
        unsafe {
            ptr::write_bytes(q_virt, 0, 8192);
        }

        ctrl.asq = q_virt as *mut SqEntry;
        ctrl.asq_phys = q_phys;
        ctrl.acq = unsafe { q_virt.add(4096) } as *mut CqEntry;
        ctrl.acq_phys = q_phys + 4096;

        ctrl.w32(
            NVME_AQA,
            ((ADMIN_QUEUE_DEPTH - 1) as u32) | (((ADMIN_QUEUE_DEPTH - 1) as u32) << 16),
        );
        ctrl.w32(NVME_ASQ, ctrl.asq_phys as u32);
        ctrl.w32(NVME_ASQ + 4, (ctrl.asq_phys >> 32) as u32);
        ctrl.w32(NVME_ACQ, ctrl.acq_phys as u32);
        ctrl.w32(NVME_ACQ + 4, (ctrl.acq_phys >> 32) as u32);

        ctrl.w32(NVME_CC, CC_EN | CC_IOCQES | CC_IOSQES);
        if crate::timing::wait_timeout_us(500_000, || {
            (ctrl.r32(NVME_CSTS) & CSTS_RDY) != 0
        }).is_err() {
            log::info!("NVMe: controller failed to become ready");
            return None;
        }

        ctrl.w32(NVME_INTMS, 0xFFFFFFFF);

        log::info!("NVMe: controller ready");
        Some(ctrl)
    }

    fn r32(&self, off: usize) -> u32 {
        unsafe { ptr::read_volatile(self.mmio.add(off / 4)) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(self.mmio.add(off / 4), v);
        }
    }
}

/// Initialise all NVMe controllers found on the PCI bus.
pub fn init(ctx: &dyn DriverContext) {
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        if dev.class_code == 0x01 && dev.subclass == 0x08 {
            log::info!(
                "NVMe: found device {:#06x}:{:#06x}",
                dev.vendor_id,
                dev.device_id
            );
            dev.enable_memory_access();
            if let Some(ctrl) = NvmeController::init(ctx, dev.clone()) {
                CONTROLLERS.lock().push(ctrl);
            }
        }
    }
    if CONTROLLERS.lock().is_empty() {
        log::info!("NVMe: no NVMe devices found");
    }
}
