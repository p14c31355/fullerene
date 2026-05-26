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
use nitrogen::pci::{PciConfigSpace, PciDevice};
use petroleum::initializer::FrameAllocator;
use spin::Mutex;

static CONTROLLERS: Mutex<Vec<NvmeController>> = Mutex::new(Vec::new());

// ── Controller registers (offset from BAR0) ─────────────────────
const NVME_CAP: usize  = 0x00; // Controller Capabilities
const NVME_VS: usize   = 0x08; // Version
const NVME_INTMS: usize = 0x0C; // Interrupt Mask Set
const NVME_INTMC: usize = 0x10; // Interrupt Mask Clear
const NVME_CC: usize   = 0x14; // Controller Configuration
const NVME_CSTS: usize = 0x1C; // Controller Status
const NVME_AQA: usize  = 0x24; // Admin Queue Attributes
const NVME_ASQ: usize  = 0x28; // Admin Submission Queue Base
const NVME_ACQ: usize  = 0x30; // Admin Completion Queue Base

// ── CC bits ──────────────────────────────────────────────────────
const CC_EN: u32 = 1 << 0;      // Enable
const CC_IOCQES: u32 = 4 << 20; // I/O Completion Queue Entry Size (2^4 = 16)
const CC_IOSQES: u32 = 6 << 16; // I/O Submission Queue Entry Size (2^6 = 64)

// ── CSTS bits ────────────────────────────────────────────────────
const CSTS_RDY: u32 = 1 << 0; // Ready

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
    dw0: u32,  // Command Specific
    rsvd: u32,
    sq_head: u16,
    sq_id: u16,
    command_id: u16,
    status: u16, // phase bit in bit 0
}

// ── Admin commands ───────────────────────────────────────────────
const ADMIN_DELETE_IO_SQ: u8 = 0x00;
const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_DELETE_IO_CQ: u8 = 0x04;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;

pub struct NvmeController {
    device: PciDevice,
    mmio: *mut u32,
    bar0_phys: u64,
    /// Admin Submission Queue (circular buffer, 64 entries).
    asq: *mut SqEntry,
    asq_phys: u64,
    asq_tail: u16,
    /// Admin Completion Queue (circular buffer, 64 entries).
    acq: *mut CqEntry,
    acq_phys: u64,
    acq_head: u16,
    /// Current phase bit for completion queue.
    phase: u16,
    /// Contiguous physical memory for queue DMA (one large allocation).
    queue_phys: u64,
}

unsafe impl Send for NvmeController {}
unsafe impl Sync for NvmeController {}

impl NvmeController {
    pub fn init(device: PciDevice) -> Option<Self> {
        let bar0 = device.get_bar_info(0)?;
        if bar0.is_io { return None; }
        let bar0_phys = bar0.address;
        let bar0_virt =
            petroleum::common::memory::physical_to_virtual(bar0_phys as usize) as *mut u32;

        // Map MMIO BAR0
        {
            let mut mgr = crate::memory_management::get_memory_manager().lock();
            let m = mgr.as_mut()?;
            m.map_mmio_region(bar0_phys as usize, bar0_virt as usize, bar0.size as usize).ok()?;
        }

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

        // Disable controller if running
        ctrl.w32(NVME_CC, 0);
        for _ in 0..1_000_000 {
            if (ctrl.r32(NVME_CSTS) & CSTS_RDY) == 0 { break; }
            core::hint::spin_loop();
        }

        // Allocate queue memory (2 pages: 1 for ASQ, 1 for ACQ)
        let q_phys = {
            let mut mgr = crate::memory_management::get_memory_manager().lock();
            let m = mgr.as_mut()?;
            m.allocate_contiguous_frames(2).ok()? as u64
        };
        ctrl.queue_phys = q_phys;
        let q_virt = petroleum::common::memory::physical_to_virtual(q_phys as usize) as *mut u8;
        unsafe { ptr::write_bytes(q_virt, 0, 8192); }

        ctrl.asq = q_virt as *mut SqEntry;
        ctrl.asq_phys = q_phys;
        ctrl.acq = unsafe { q_virt.add(4096) } as *mut CqEntry;
        ctrl.acq_phys = q_phys + 4096;

        // Configure admin queues
        ctrl.w32(NVME_AQA, ((ADMIN_QUEUE_DEPTH - 1) as u32)
            | (((ADMIN_QUEUE_DEPTH - 1) as u32) << 16));
        ctrl.w32(NVME_ASQ, ctrl.asq_phys as u32);
        ctrl.w32(NVME_ASQ + 4, (ctrl.asq_phys >> 32) as u32);
        ctrl.w32(NVME_ACQ, ctrl.acq_phys as u32);
        ctrl.w32(NVME_ACQ + 4, (ctrl.acq_phys >> 32) as u32);

        // Enable controller
        ctrl.w32(NVME_CC, CC_EN | CC_IOCQES | CC_IOSQES);
        for _ in 0..1_000_000 {
            if (ctrl.r32(NVME_CSTS) & CSTS_RDY) != 0 { break; }
            core::hint::spin_loop();
        }
        if (ctrl.r32(NVME_CSTS) & CSTS_RDY) == 0 {
            log::info!("NVMe: controller failed to become ready");
            return None;
        }

        // Mask all interrupts
        ctrl.w32(NVME_INTMS, 0xFFFFFFFF);

        log::info!("NVMe: controller ready");
        Some(ctrl)
    }

    fn r32(&self, off: usize) -> u32 {
        unsafe { ptr::read_volatile(self.mmio.add(off / 4)) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { ptr::write_volatile(self.mmio.add(off / 4), v); }
    }
}

/// Initialise all NVMe controllers found on the PCI bus.
pub fn init() {
    let mut scanner = nitrogen::pci::PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        // NVMe: class 0x01 (mass storage), subclass 0x08
        if dev.class_code == 0x01 && dev.subclass == 0x08 {
            log::info!("NVMe: found device {:#06x}:{:#06x}", dev.vendor_id, dev.device_id);
            dev.enable_memory_access();
            if let Some(ctrl) = NvmeController::init(dev.clone()) {
                CONTROLLERS.lock().push(ctrl);
            }
        }
    }
    if CONTROLLERS.lock().is_empty() {
        log::info!("NVMe: no NVMe devices found");
    }
}