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
use sealant::{MmioRegion, Permissions};

static CONTROLLERS: Mutex<Vec<NvmeController>> = Mutex::new(Vec::new());

// ── Controller registers (offset from BAR0) ─────────────────────
const NVME_CAP: usize = 0x00;
const NVME_VS: usize = 0x08;
const NVME_INTMS: usize = 0x0C;
const NVME_CC: usize = 0x14;
const NVME_CSTS: usize = 0x1C;
const NVME_AQA: usize = 0x24;
const NVME_ASQ: usize = 0x28;
const NVME_ACQ: usize = 0x30;
const NVME_REGISTER_SPACE_SIZE: usize = 0x4000;

// ── CC bits ──────────────────────────────────────────────────────
const CC_EN: u32 = 1 << 0;
const CC_IOCQES: u32 = 4 << 20;
const CC_IOSQES: u32 = 6 << 16;

// ── CSTS bits ────────────────────────────────────────────────────
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

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

/// BAR0-backed controller registers. This is an MMIO resource, not a DMA
/// buffer; it is assigned once before any controller property or queue access.
struct NvmeRegisterBlock {
    #[allow(dead_code)]
    phys: u64,
    mmio: MmioRegion<'static>,
    size: usize,
}

unsafe impl Send for NvmeRegisterBlock {}
unsafe impl Sync for NvmeRegisterBlock {}

impl NvmeRegisterBlock {
    fn allocate(ctx: &'static dyn DriverContext, device: &PciDevice) -> Option<Self> {
        let bar0 = device.read_bar_info(0)?;
        if bar0.is_io || !bar0.is_64bit || bar0.address == 0 {
            return None;
        }
        let virt = ctx.phys_to_virt(bar0.address);
        ctx.map_mmio_region(bar0.address as usize, virt, NVME_REGISTER_SPACE_SIZE)
            .ok()?;
        // KernelDriverContext establishes the mapping above and keeps the
        // higher-half direct map alive for the controller lifetime.
        let mmio = unsafe {
            MmioRegion::from_address(virt, NVME_REGISTER_SPACE_SIZE, Permissions::READ_WRITE)
        }
        .ok()?;
        log::info!(
            "NVMe: BAR0 register block assigned at {:#x} ({} bytes)",
            bar0.address,
            NVME_REGISTER_SPACE_SIZE
        );
        Some(Self {
            phys: bar0.address,
            mmio,
            size: NVME_REGISTER_SPACE_SIZE,
        })
    }

    fn r32(&self, off: usize) -> u32 {
        debug_assert!(off + core::mem::size_of::<u32>() <= self.size);
        let val = match self.mmio.read_volatile_at::<u32>(off) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("NVMe: invalid MMIO read at {:#x}: {:?}", off, error);
                return u32::MAX;
            }
        };
        if val == u32::MAX {
            log::warn!("NVMe: MMIO read at offset {:#x} returned 0xFFFF_FFFF", off);
        }
        val
    }

    fn r64(&self, off: usize) -> u64 {
        debug_assert_eq!(off % 8, 0);
        debug_assert!(off + core::mem::size_of::<u64>() <= self.size);
        let val = match self.mmio.read_volatile_at::<u64>(off) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("NVMe: invalid MMIO read at {:#x}: {:?}", off, error);
                return u64::MAX;
            }
        };
        if val == u64::MAX {
            log::warn!(
                "NVMe: MMIO read at offset {:#x} returned 0xFFFF_FFFF_FFFF_FFFF",
                off
            );
        }
        val
    }

    fn w32(&self, off: usize, value: u32) {
        debug_assert!(off + core::mem::size_of::<u32>() <= self.size);
        if let Err(error) = self.mmio.write_volatile_at(off, value) {
            log::warn!(
                "NVMe: invalid MMIO write at {:#x} value={:#x}: {:?}",
                off,
                value,
                error
            );
        }
    }

    fn read(&self, off: usize, width: u8) -> Result<u64, DriverError> {
        match width {
            1 => self
                .mmio
                .read_volatile_at::<u8>(off)
                .map(u64::from)
                .map_err(|_| DriverError::InvalidArgument),
            2 => self
                .mmio
                .read_volatile_at::<u16>(off)
                .map(u64::from)
                .map_err(|_| DriverError::InvalidArgument),
            4 => self
                .mmio
                .read_volatile_at::<u32>(off)
                .map(u64::from)
                .map_err(|_| DriverError::InvalidArgument),
            8 => self
                .mmio
                .read_volatile_at::<u64>(off)
                .map_err(|_| DriverError::InvalidArgument),
            _ => Err(DriverError::InvalidArgument),
        }
    }

    fn write(&self, off: usize, width: u8, value: u64) -> Result<(), DriverError> {
        match width {
            1 if value <= u8::MAX as u64 => self
                .mmio
                .write_volatile_at(off, value as u8)
                .map_err(|_| DriverError::InvalidArgument),
            2 if value <= u16::MAX as u64 => self
                .mmio
                .write_volatile_at(off, value as u16)
                .map_err(|_| DriverError::InvalidArgument),
            4 if value <= u32::MAX as u64 => self
                .mmio
                .write_volatile_at(off, value as u32)
                .map_err(|_| DriverError::InvalidArgument),
            8 => self
                .mmio
                .write_volatile_at(off, value)
                .map_err(|_| DriverError::InvalidArgument),
            _ => Err(DriverError::InvalidArgument),
        }
    }
}

pub struct NvmeController {
    ctx: &'static dyn DriverContext,
    #[allow(dead_code)]
    device: PciDevice,
    registers: NvmeRegisterBlock,
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
    admin_queue_depth: u16,
    controller_timeout_us: u64,
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
        // Assign the controller register block before reading CAP/VS or
        // requesting any NVMe DMA queue memory.
        let registers = NvmeRegisterBlock::allocate(ctx, &device)?;

        let mut ctrl = Self {
            ctx,
            device,
            registers,
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
            admin_queue_depth: 0,
            controller_timeout_us: 500_000,
        };

        // CAP and VS are controller properties exposed through the PCI BAR,
        // not DMA buffers.  Read them before programming CC/AQA/ASQ/ACQ.
        let cap = ctrl.r64(NVME_CAP);
        let version = ctrl.r32(NVME_VS);
        let max_queue_depth = (cap as u16).saturating_add(1);
        let mps_min = ((cap >> 48) & 0xF) as u8;
        let mps_max = ((cap >> 52) & 0xF) as u8;
        let host_mps = 0u8; // 2^(12 + 0) = 4 KiB pages
        let nvm_command_set_supported = (cap & (1u64 << 37)) != 0;
        if version < 0x0001_0000
            || max_queue_depth < 2
            || host_mps < mps_min
            || host_mps > mps_max
            || !nvm_command_set_supported
        {
            log::info!(
                "NVMe: unsupported controller version={:#x} CAP={:#018x}",
                version,
                cap
            );
            return None;
        }
        ctrl.admin_queue_depth = core::cmp::min(ADMIN_QUEUE_DEPTH, max_queue_depth);
        let timeout_units = ((cap >> 24) & 0xFF) as u64;
        ctrl.controller_timeout_us = timeout_units
            .checked_mul(500_000)
            .filter(|timeout| *timeout != 0)
            .unwrap_or(500_000);

        ctrl.w32(NVME_CC, 0);
        if crate::timing::wait_timeout_us(ctrl.controller_timeout_us, || {
            let status = ctrl.r32(NVME_CSTS);
            status & CSTS_CFS == 0 && status & CSTS_RDY == 0
        })
        .is_err()
        {
            log::info!("NVMe: controller did not leave the ready state");
            return None;
        }

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
            ((ctrl.admin_queue_depth - 1) as u32) | (((ctrl.admin_queue_depth - 1) as u32) << 16),
        );
        ctrl.w32(NVME_ASQ, ctrl.asq_iova as u32);
        ctrl.w32(NVME_ASQ + 4, (ctrl.asq_iova >> 32) as u32);
        ctrl.w32(NVME_ACQ, ctrl.acq_iova as u32);
        ctrl.w32(NVME_ACQ + 4, (ctrl.acq_iova >> 32) as u32);

        ctrl.w32(NVME_CC, CC_EN | CC_IOCQES | CC_IOSQES);
        if crate::timing::wait_timeout_us(ctrl.controller_timeout_us, || {
            let status = ctrl.r32(NVME_CSTS);
            status & CSTS_CFS == 0 && status & CSTS_RDY != 0
        })
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
        self.registers.r32(off)
    }
    fn r64(&self, off: usize) -> u64 {
        self.registers.r64(off)
    }
    fn w32(&self, off: usize, v: u32) {
        self.registers.w32(off, v)
    }

    fn mmio_request(
        &self,
        bar: u8,
        offset: u32,
        width: u8,
        write: bool,
        value: u64,
    ) -> Result<u64, DriverError> {
        if bar != 0 {
            return Err(DriverError::NotSupported);
        }
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(DriverError::InvalidArgument);
        }
        let offset = offset as usize;
        if offset % width as usize != 0
            || offset
                .checked_add(width as usize)
                .is_none_or(|end| end > self.registers.size)
        {
            return Err(DriverError::InvalidArgument);
        }
        if write {
            self.registers.write(offset, width, value)?;
            Ok(0)
        } else {
            self.registers.read(offset, width)
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

/// Execute one driver-mediated MMIO request against an initialized NVMe
/// controller.  The caller owns the generic kernel SQ/CQ transaction; this
/// function only performs the driver-side dispatch and sealant-checked access.
pub fn request_mmio(
    device: &PciDevice,
    bar: u8,
    offset: u32,
    width: u8,
    write: bool,
    value: u64,
) -> Result<u64, DriverError> {
    let controllers = CONTROLLERS.lock();
    let controller = controllers
        .iter()
        .find(|controller| {
            controller.device.bus == device.bus
                && controller.device.device == device.device
                && controller.device.function == device.function
        })
        .ok_or(DriverError::NotReady)?;
    controller.mmio_request(bar, offset, width, write, value)
}

/// Number of controllers that completed initialization.
pub fn controller_count() -> usize {
    CONTROLLERS.lock().len()
}
