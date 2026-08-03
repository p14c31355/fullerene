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

struct ControllerRegistry {
    controllers: Vec<NvmeController>,
    initializing: Vec<DeviceKey>,
}

impl ControllerRegistry {
    const fn new() -> Self {
        Self {
            controllers: Vec::new(),
            initializing: Vec::new(),
        }
    }
}

static CONTROLLERS: Mutex<ControllerRegistry> = Mutex::new(ControllerRegistry::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceKey {
    bus: u8,
    device: u8,
    function: u8,
}

impl DeviceKey {
    fn from_device(device: &PciDevice) -> Self {
        Self {
            bus: device.bus,
            device: device.device,
            function: device.function,
        }
    }
}

// A failed controller is quarantined after cleanup.  This prevents a later
// ioctl from reusing a device whose hardware state or ownership is unknown.
static FAILED_CONTROLLERS: Mutex<Vec<DeviceKey>> = Mutex::new(Vec::new());

fn same_device(a: &PciDevice, b: &PciDevice) -> bool {
    a.bus == b.bus && a.device == b.device && a.function == b.function
}

fn mark_failed(device: &PciDevice) {
    let mut failed = FAILED_CONTROLLERS.lock();
    if !failed.iter().any(|entry| {
        entry.bus == device.bus
            && entry.device == device.device
            && entry.function == device.function
    }) {
        failed.push(DeviceKey::from_device(device));
    }
}

fn is_failed(device: &PciDevice) -> bool {
    FAILED_CONTROLLERS.lock().iter().any(|entry| {
        entry.bus == device.bus
            && entry.device == device.device
            && entry.function == device.function
    })
}

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
const NVME_DOORBELL_BASE: usize = 0x1000;
// CAP.TO is a device-provided upper bound, not a reason for an ioctl caller
// to busy-poll indefinitely. Keep each status wait bounded.
const MAX_CONTROLLER_TIMEOUT_US: u64 = 5_000_000;

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
    ctx: &'static dyn DriverContext,
    phys: u64,
    virt: usize,
    mmio: MmioRegion<'static>,
    size: usize,
}

unsafe impl Send for NvmeRegisterBlock {}
unsafe impl Sync for NvmeRegisterBlock {}

impl NvmeRegisterBlock {
    fn allocate(ctx: &'static dyn DriverContext, device: &PciDevice) -> Result<Self, DriverError> {
        let bar0 = device.get_bar_info(0).ok_or(DriverError::DeviceNotFound)?;
        if bar0.is_io || !bar0.is_64bit || bar0.address == 0 {
            return Err(DriverError::InvalidArgument);
        }
        if (bar0.size as usize) < NVME_REGISTER_SPACE_SIZE {
            log::warn!(
                "NVMe: BAR0 is too small ({:#x} bytes, need {:#x})",
                bar0.size,
                NVME_REGISTER_SPACE_SIZE
            );
            return Err(DriverError::InvalidArgument);
        }
        let virt = ctx.phys_to_virt(bar0.address);
        ctx.map_mmio_region(bar0.address as usize, virt, NVME_REGISTER_SPACE_SIZE)
            .map_err(DriverError::from)?;
        // KernelDriverContext establishes the mapping above and keeps the
        // higher-half direct map alive for the controller lifetime.
        let mmio = match unsafe {
            MmioRegion::from_address(virt, NVME_REGISTER_SPACE_SIZE, Permissions::READ_WRITE)
        } {
            Ok(mmio) => mmio,
            Err(_) => {
                ctx.unmap_mmio_region(bar0.address as usize, virt, NVME_REGISTER_SPACE_SIZE);
                return Err(DriverError::MmioMappingFailed);
            }
        };
        log::info!(
            "NVMe: BAR0 register block assigned at {:#x} ({} bytes)",
            bar0.address,
            NVME_REGISTER_SPACE_SIZE
        );
        Ok(Self {
            ctx,
            phys: bar0.address,
            virt,
            mmio,
            size: NVME_REGISTER_SPACE_SIZE,
        })
    }

    fn r32(&self, off: usize) -> Result<u32, DriverError> {
        debug_assert!(off + core::mem::size_of::<u32>() <= self.size);
        let val = self.mmio.read_volatile_at::<u32>(off).map_err(|error| {
            log::warn!("NVMe: invalid MMIO read at {:#x}: {:?}", off, error);
            DriverError::Io
        })?;
        if val == u32::MAX {
            log::warn!("NVMe: MMIO read at offset {:#x} returned 0xFFFF_FFFF", off);
        }
        Ok(val)
    }

    fn r64(&self, off: usize) -> Result<u64, DriverError> {
        debug_assert_eq!(off % 8, 0);
        debug_assert!(off + core::mem::size_of::<u64>() <= self.size);
        let val = self.mmio.read_volatile_at::<u64>(off).map_err(|error| {
            log::warn!("NVMe: invalid MMIO read at {:#x}: {:?}", off, error);
            DriverError::Io
        })?;
        if val == u64::MAX {
            log::warn!(
                "NVMe: MMIO read at offset {:#x} returned 0xFFFF_FFFF_FFFF_FFFF",
                off
            );
        }
        Ok(val)
    }

    fn w32(&self, off: usize, value: u32) -> Result<(), DriverError> {
        debug_assert!(off + core::mem::size_of::<u32>() <= self.size);
        self.mmio.write_volatile_at(off, value).map_err(|error| {
            log::warn!(
                "NVMe: invalid MMIO write at {:#x} value={:#x}: {:?}",
                off,
                value,
                error
            );
            DriverError::Io
        })
    }

    fn unmap(&self) {
        self.ctx
            .unmap_mmio_region(self.phys as usize, self.virt, self.size);
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

impl Drop for NvmeRegisterBlock {
    fn drop(&mut self) {
        self.unmap();
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
    hardware_owned: bool,
}

unsafe impl Send for NvmeController {}
unsafe impl Sync for NvmeController {}

impl Drop for NvmeController {
    fn drop(&mut self) {
        // Stop the controller before disabling bus mastering or releasing any
        // DMA memory it may still reference.
        self.stop_hardware();
        // If PCI refuses to clear Memory Space/Bus Master Enable, keep both
        // allocations quarantined. Releasing them would let a still-running
        // controller DMA into memory that has already been reused.
        if self.device.disable_memory_access() {
            if self.submission_queue.size != 0 {
                self.ctx.release_dma_buffer(self.submission_queue);
                self.submission_queue.size = 0;
            }
            if self.completion_queue.size != 0 {
                self.ctx.release_dma_buffer(self.completion_queue);
                self.completion_queue.size = 0;
            }
        } else {
            log::error!(
                "NVMe: PCI bus-master disable failed; quarantining DMA queues for {:02x}:{:02x}.{}",
                self.device.bus,
                self.device.device,
                self.device.function
            );
        }
    }
}

impl NvmeController {
    pub fn init(ctx: &'static dyn DriverContext, device: PciDevice) -> Result<Self, DriverError> {
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
            hardware_owned: false,
        };

        // CAP and VS are controller properties exposed through the PCI BAR,
        // not DMA buffers.  Read them before programming CC/AQA/ASQ/ACQ.
        let cap = ctrl.r64(NVME_CAP)?;
        let version = ctrl.r32(NVME_VS)?;
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
            return Err(DriverError::NotSupported);
        }
        ctrl.admin_queue_depth = core::cmp::min(ADMIN_QUEUE_DEPTH, max_queue_depth);
        let timeout_units = ((cap >> 24) & 0xFF) as u64;
        ctrl.controller_timeout_us = timeout_units
            .checked_mul(500_000)
            .filter(|timeout| *timeout != 0)
            .unwrap_or(500_000)
            .min(MAX_CONTROLLER_TIMEOUT_US);

        ctrl.hardware_owned = true;
        ctrl.w32(NVME_CC, 0)?;
        if !ctrl.wait_for_status(|status| status & CSTS_CFS == 0 && status & CSTS_RDY == 0) {
            log::info!("NVMe: controller did not leave the ready state");
            return Err(DriverError::TimedOut);
        }

        let device_id = ((ctrl.device.bus as u16) << 8)
            | ((ctrl.device.device as u16) << 3)
            | ctrl.device.function as u16;
        // NVMe submission and completion queues are independent DMA objects.
        // The kernel owns the physical allocation and IOMMU mapping; the
        // driver only receives CPU and device addresses to program into NVMe.
        let sq_bytes = ctrl.admin_queue_depth as usize * core::mem::size_of::<SqEntry>();
        let cq_bytes = ctrl.admin_queue_depth as usize * core::mem::size_of::<CqEntry>();
        ctrl.submission_queue = ctx
            .allocate_dma_buffer(device_id, sq_bytes)
            .map_err(DriverError::from)?;
        ctrl.completion_queue = ctx
            .allocate_dma_buffer(device_id, cq_bytes)
            .map_err(DriverError::from)?;
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
        )?;
        ctrl.w32(NVME_ASQ, ctrl.asq_iova as u32)?;
        ctrl.w32(NVME_ASQ + 4, (ctrl.asq_iova >> 32) as u32)?;
        ctrl.w32(NVME_ACQ, ctrl.acq_iova as u32)?;
        ctrl.w32(NVME_ACQ + 4, (ctrl.acq_iova >> 32) as u32)?;

        ctrl.w32(NVME_CC, CC_EN | CC_IOCQES | CC_IOSQES)?;
        if !ctrl.wait_for_status(|status| status & CSTS_CFS == 0 && status & CSTS_RDY != 0) {
            log::info!("NVMe: controller failed to become ready");
            return Err(DriverError::TimedOut);
        }

        ctrl.w32(NVME_INTMS, 0xFFFFFFFF)?;

        log::info!("NVMe: controller ready");
        Ok(ctrl)
    }

    fn r32(&self, off: usize) -> Result<u32, DriverError> {
        self.registers.r32(off)
    }
    fn r64(&self, off: usize) -> Result<u64, DriverError> {
        self.registers.r64(off)
    }
    fn w32(&self, off: usize, v: u32) -> Result<(), DriverError> {
        self.registers.w32(off, v)
    }

    fn wait_for_status(&self, condition: impl Fn(u32) -> bool) -> bool {
        crate::timing::poll_timeout_us(self.controller_timeout_us, || {
            self.r32(NVME_CSTS).ok().filter(|status| condition(*status))
        })
        .is_some()
    }

    fn stop_hardware(&mut self) {
        if !self.hardware_owned {
            return;
        }
        let _ = self.registers.w32(NVME_CC, 0);
        if !self.wait_for_status(|status| status & CSTS_RDY == 0) {
            log::warn!("NVMe: controller did not become stopped during cleanup");
        }
        self.hardware_owned = false;
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
            if offset < NVME_DOORBELL_BASE {
                return Err(DriverError::InvalidArgument);
            }
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
    if CONTROLLERS.lock().controllers.is_empty() {
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

    if is_failed(&device) {
        return Err(DriverError::DeviceFault);
    }

    // Claim the device under the same lock as the existing-controller lookup.
    // This prevents boot scanning and ioctl retries from initializing one PCI
    // function twice while the first initializer is outside the lock.
    let key = DeviceKey::from_device(&device);
    {
        let mut registry = CONTROLLERS.lock();
        if let Some(index) = registry
            .controllers
            .iter()
            .position(|controller| same_device(&controller.device, &device))
        {
            return Ok(index);
        }
        if registry.initializing.iter().any(|entry| *entry == key) {
            return Err(DriverError::Busy);
        }
        registry.initializing.push(key);
    }

    let result = (|| {
        if !device.enable_memory_access() {
            let _ = device.disable_memory_access();
            mark_failed(&device);
            return Err(DriverError::DeviceFault);
        }
        match NvmeController::init(ctx, device.clone()) {
            Ok(ctrl) => Ok(ctrl),
            Err(error) => {
                // NvmeController's failure path has already stopped hardware
                // and released or quarantined its owned resources.
                let _ = device.disable_memory_access();
                mark_failed(&device);
                Err(error)
            }
        }
    })();

    let mut registry = CONTROLLERS.lock();
    registry.initializing.retain(|entry| *entry != key);
    match result {
        Ok(ctrl) => {
            let index = registry.controllers.len();
            registry.controllers.push(ctrl);
            Ok(index)
        }
        Err(error) => Err(error),
    }
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
    if is_failed(device) {
        return Err(DriverError::DeviceFault);
    }
    let result = {
        let registry = CONTROLLERS.lock();
        let controller = registry
            .controllers
            .iter()
            .find(|controller| same_device(&controller.device, device))
            .ok_or(DriverError::NotReady)?;
        controller.mmio_request(bar, offset, width, write, value)
    };
    if let Err(error) = result {
        if error.is_fatal() {
            let controller = {
                let mut registry = CONTROLLERS.lock();
                registry
                    .controllers
                    .iter()
                    .position(|controller| same_device(&controller.device, device))
                    .map(|index| registry.controllers.remove(index))
            };
            // Dropping outside the registry lock performs the ordered device
            // shutdown, PCI bus-master disable, DMA release, and MMIO unmap.
            drop(controller);
            mark_failed(device);
        }
        return Err(error);
    }
    result
}

/// Number of controllers that completed initialization.
pub fn controller_count() -> usize {
    CONTROLLERS.lock().controllers.len()
}
