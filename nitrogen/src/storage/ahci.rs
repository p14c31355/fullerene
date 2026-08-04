//! AHCI (Advanced Host Controller Interface) driver.
//!
//! Implements SATA disk access via PCI AHCI controllers.  Discovers the
//! HBA memory registers, resets ports, sends IDENTIFY DEVICE, and reads
//! sectors via DMA.
//!
//! # References
//! - Serial ATA AHCI 1.3.1 Specification
//! - Serial ATA Revision 3.0

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::pci::PciDevice;

/// Global list of discovered AHCI controllers.
static CONTROLLERS: Mutex<Vec<Arc<Mutex<AhciController>>>> = Mutex::new(Vec::new());
static INITIALIZING: Mutex<BTreeSet<(u8, u8, u8)>> = Mutex::new(BTreeSet::new());

// ── HBA memory register offsets ──────────────────────────────────

const HBA_GHC: usize = 0x04; // Global Host Control
const HBA_PI: usize = 0x0C; // Ports Implemented

// ── GHC bits ─────────────────────────────────────────────────────
const GHC_HR: u32 = 1 << 0;
const GHC_AE: u32 = 1 << 31;

// ── Port register offsets (relative to port base) ────────────────
const PXCLB: usize = 0x00; // Command List Base Address
const PXCLBU: usize = 0x04; // Command List Base Address Upper
const PXFB: usize = 0x08; // FIS Base Address
const PXFBU: usize = 0x0C; // FIS Base Address Upper
const PXIS: usize = 0x10; // Interrupt Status
const PXIE: usize = 0x14; // Interrupt Enable
const PXCMD: usize = 0x18; // Command and Status
const PXSSTS: usize = 0x28; // SATA Status (SCR0: SStatus)
const PXSERR: usize = 0x30; // SATA Error (SCR1: SError)
const PXCI: usize = 0x38; // Command Issue
const PXIS_TFES: u32 = 1 << 30; // Task File Error Status

// ── PxCMD bits ───────────────────────────────────────────────────
const PXCMD_ST: u32 = 1 << 0; // Start DMA
const PXCMD_FRE: u32 = 1 << 4; // FIS Receive Enable
const PXCMD_FR: u32 = 1 << 14; // FIS Receive Running
const PXCMD_CR: u32 = 1 << 15; // Command List Running

// ── SATA status (PxSSTS) ────────────────────────────────────────
const SSTS_DET_MASK: u32 = 0x0F;
const SSTS_DET_PHY_OK: u32 = 0x03;
const PHY_WAIT_TIMEOUT_US: u64 = 1_000_000;

// ── Command Header ───────────────────────────────────────────────
#[repr(C)]
struct CommandHeader {
    dword0: u32,
    prdbc: u32,
    ctba: u32,
    ctbau: u32,
    rsvd: [u32; 4],
}

// ── Command Table ────────────────────────────────────────────────
#[repr(C, align(128))]
struct CommandTable {
    cfis: [u8; 64],
    acmd: [u8; 16],
    rsvd: [u8; 48],
    prdt: [PrdtEntry; 1],
}

// ── PRDT Entry ───────────────────────────────────────────────────
#[repr(C)]
struct PrdtEntry {
    dba: u32,
    dbau: u32,
    rsvd: u32,
    dbc: u32,
}

// ── Received FIS structure ───────────────────────────────────────
#[repr(C, align(256))]
struct ReceivedFis {
    dsfis: [u8; 28],
    pad0: [u8; 4],
    psfis: [u8; 24],
    pad1: [u8; 8],
    rfis: [u8; 24],
    pad2: [u8; 4],
    sdbfis: [u8; 8],
    ufis: [u8; 64],
    rsvd: [u8; 96],
}

// ── Controller ───────────────────────────────────────────────────

#[allow(dead_code)]
struct AhciPort {
    index: u8,
    port_mmio: *mut u32,
    cmd_list: *mut CommandHeader,
    cmd_list_phys: u64,
    fis: *mut ReceivedFis,
    fis_phys: u64,
    cmd_table: *mut CommandTable,
    cmd_table_phys: u64,
    data_buffer: *mut u8,
    data_buffer_phys: u64,
    data_buffer_size: usize,
    sector_size: u32,
    total_sectors: u64,
    lba48: bool,
}

enum TransferBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

pub struct AhciController {
    #[allow(dead_code)]
    device: PciDevice,
    hba_mmio: *mut u32,
    #[allow(dead_code)]
    hba_phys: u64,
    /// Number of implemented ports (0–31).
    num_ports: u32,
    /// Bit mask of ports implemented by the HBA.
    port_mask: u32,
    ports: Vec<AhciPort>,
}

/// One ATA disk exposed by an initialized AHCI controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AhciDeviceInfo {
    pub controller_index: usize,
    pub port_index: u8,
    pub sector_size: u32,
    pub total_sectors: u64,
}

// SAFETY: Single-threaded kernel — all device MMIO pointers are
// accessed only from the scheduler loop / init path.
unsafe impl Send for AhciController {}
unsafe impl Sync for AhciController {}

impl AhciController {
    /// Initialise an AHCI controller found on the PCI bus.
    ///
    /// `ctx` provides memory allocation, MMIO mapping, and address
    /// translation services (typically the kernel's [`DriverContext`]).
    pub fn init(ctx: &dyn DriverContext, device: PciDevice) -> Option<Self> {
        let bar5 = device.get_bar_info(5)?;
        if bar5.is_io {
            return None;
        }
        let hba_phys = bar5.address;
        let hba_virt = ctx.phys_to_virt(hba_phys) as *mut u32;

        ctx.map_mmio_region(hba_phys as usize, hba_virt as usize, bar5.size as usize)
            .ok()?;

        let mut ctrl = Self {
            device,
            hba_mmio: hba_virt,
            hba_phys,
            num_ports: 0,
            port_mask: 0,
            ports: Vec::new(),
        };

        let ghc = ctrl.r32(HBA_GHC);
        ctrl.w32(HBA_GHC, ghc | GHC_AE);
        ctrl.w32(HBA_GHC, ghc | GHC_AE | GHC_HR);
        if crate::timing::wait_timeout_us(500_000, || (ctrl.r32(HBA_GHC) & GHC_HR) == 0).is_err() {
            log::warn!("AHCI: HBA reset timed out — controller may be unresponsive");
            ctrl.w32(HBA_GHC, ctrl.r32(HBA_GHC) & !GHC_HR);
        }

        let pi = ctrl.r32(HBA_PI);
        ctrl.num_ports = pi.count_ones() as u32;
        ctrl.port_mask = pi;
        log::info!(
            "AHCI: HBA BAR5={:#x} size={:#x} GHC={:#x} PI={:#x}",
            hba_phys,
            bar5.size,
            ctrl.r32(HBA_GHC),
            pi
        );
        ctrl.probe_ports(ctx);

        Some(ctrl)
    }

    /// Probe implemented ports and publish disks that complete IDENTIFY.
    ///
    /// This is also used for an explicit `ahci_init` after boot.  A SATA link
    /// can become ready after the HBA reset, so a controller that was already
    /// created must still be allowed to discover newly-ready ports.
    fn probe_ports(&mut self, ctx: &dyn DriverContext) {
        let phy_deadline_tsc = unsafe { core::arch::x86_64::_rdtsc() }
            .wrapping_add(PHY_WAIT_TIMEOUT_US.saturating_mul(crate::timing::ticks_per_us()));
        for i in 0..32 {
            if (self.port_mask >> i) & 1 == 0 {
                continue;
            }
            if self.ports.iter().any(|port| port.index == i as u8) {
                continue;
            }

            let ssts = self.wait_for_phy_until(i as u8, phy_deadline_tsc);
            if ssts & SSTS_DET_MASK != SSTS_DET_PHY_OK {
                log::info!(
                    "AHCI port {}: no PHY after {} ms (SSTS={:#x}, SERR={:#x}), skipping init",
                    i,
                    PHY_WAIT_TIMEOUT_US / 1_000,
                    ssts,
                    self.port_serror(i as u8)
                );
                continue;
            }

            let Some(mut initialized) = self.init_port(ctx, i as u8, phy_deadline_tsc) else {
                continue;
            };
            match self.identify_port(&mut initialized) {
                Ok((sector_size, total_sectors, lba48)) => {
                    initialized.sector_size = sector_size;
                    initialized.total_sectors = total_sectors;
                    initialized.lba48 = lba48;
                    log::info!(
                        "AHCI port {}: ATA disk ({} bytes/sector, {} sectors)",
                        i,
                        sector_size,
                        total_sectors
                    );
                    self.ports.push(initialized);
                }
                Err(error) => {
                    self.release_port(ctx, initialized);
                    log::warn!("AHCI port {}: IDENTIFY failed: {}", i, error);
                }
            }
        }
    }

    fn port_mmio(&self, port: u8) -> *mut u32 {
        let port_base = 0x100 + (port as usize) * 0x80;
        unsafe { self.hba_mmio.add(port_base / 4) }
    }

    fn wait_for_phy_until(&self, port: u8, deadline_tsc: u64) -> u32 {
        let port_mmio = self.port_mmio(port);
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let remaining_tsc = deadline_tsc.saturating_sub(now);
        let remaining_us = remaining_tsc.div_ceil(crate::timing::ticks_per_us());
        let _ = crate::timing::wait_timeout_us(remaining_us, || {
            Self::r32_port(port_mmio, PXSSTS) & SSTS_DET_MASK == SSTS_DET_PHY_OK
        });
        Self::r32_port(port_mmio, PXSSTS)
    }

    fn port_serror(&self, port: u8) -> u32 {
        Self::r32_port(self.port_mmio(port), PXSERR)
    }

    fn init_port(
        &self,
        ctx: &dyn DriverContext,
        port: u8,
        phy_deadline_tsc: u64,
    ) -> Option<AhciPort> {
        let port_mmio = self.port_mmio(port);

        let cmd = Self::r32_port(port_mmio, PXCMD);
        Self::w32_port(port_mmio, PXCMD, cmd & !(PXCMD_ST | PXCMD_FRE));
        if crate::timing::wait_timeout_us(500_000, || {
            let c = Self::r32_port(port_mmio, PXCMD);
            (c & (PXCMD_CR | PXCMD_FR)) == 0
        })
        .is_err()
        {
            log::warn!("AHCI port {}: command engine did not stop", port);
            return None;
        }

        let ssts = self.wait_for_phy_until(port, phy_deadline_tsc);
        let det = ssts & SSTS_DET_MASK;
        if det != SSTS_DET_PHY_OK {
            log::info!("AHCI port {}: no device (SSTS={:#x})", port, ssts);
            return None;
        }

        let cmd_list_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!(
                    "AHCI port {}: failed to allocate cmd_list frame: {}",
                    port,
                    e
                );
                return None;
            }
        };
        let cmd_list = ctx.phys_to_virt(cmd_list_phys) as *mut CommandHeader;
        let fis_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!("AHCI port {}: failed to allocate FIS frame: {}", port, e);
                ctx.free_frame(cmd_list_phys);
                return None;
            }
        };
        let fis = ctx.phys_to_virt(fis_phys) as *mut ReceivedFis;
        let cmd_table_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!(
                    "AHCI port {}: failed to allocate cmd_table frame: {}",
                    port,
                    e
                );
                ctx.free_frame(cmd_list_phys);
                ctx.free_frame(fis_phys);
                return None;
            }
        };
        let cmd_table = ctx.phys_to_virt(cmd_table_phys) as *mut CommandTable;

        let data_buffer_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!("AHCI port {}: failed to allocate data frame: {}", port, e);
                ctx.free_frame(cmd_list_phys);
                ctx.free_frame(fis_phys);
                ctx.free_frame(cmd_table_phys);
                return None;
            }
        };
        let data_buffer = ctx.phys_to_virt(data_buffer_phys) as *mut u8;

        unsafe {
            ptr::write_bytes(cmd_list as *mut u8, 0, 4096);
            ptr::write_bytes(fis as *mut u8, 0, 4096);
            ptr::write_bytes(cmd_table as *mut u8, 0, 4096);
            ptr::write_bytes(data_buffer, 0, 4096);
        }

        Self::w32_port(port_mmio, PXCLB, cmd_list_phys as u32);
        Self::w32_port(port_mmio, PXCLBU, (cmd_list_phys >> 32) as u32);
        Self::w32_port(port_mmio, PXFB, fis_phys as u32);
        Self::w32_port(port_mmio, PXFBU, (fis_phys >> 32) as u32);

        unsafe {
            (*cmd_list).ctba = cmd_table_phys as u32;
            (*cmd_list).ctbau = (cmd_table_phys >> 32) as u32;
            (*cmd_list).dword0 = 0;
        }

        Self::w32_port(port_mmio, PXSERR, 0xFFFFFFFF);
        Self::w32_port(port_mmio, PXIS, 0xFFFFFFFF);
        Self::w32_port(port_mmio, PXIE, 0);

        Self::w32_port(
            port_mmio,
            PXCMD,
            Self::r32_port(port_mmio, PXCMD) | PXCMD_FRE | PXCMD_ST,
        );

        Some(AhciPort {
            index: port,
            port_mmio,
            cmd_list,
            cmd_list_phys,
            fis,
            fis_phys,
            cmd_table,
            cmd_table_phys,
            data_buffer,
            data_buffer_phys,
            data_buffer_size: 4096,
            sector_size: 512,
            total_sectors: 0,
            lba48: false,
        })
    }

    fn release_port(&self, ctx: &dyn DriverContext, port: AhciPort) {
        Self::stop_command_engine(port.port_mmio);
        ctx.free_frame(port.cmd_list_phys);
        ctx.free_frame(port.fis_phys);
        ctx.free_frame(port.cmd_table_phys);
        ctx.free_frame(port.data_buffer_phys);
    }

    fn stop_command_engine(port_mmio: *mut u32) {
        let cmd = Self::r32_port(port_mmio, PXCMD);
        Self::w32_port(port_mmio, PXCMD, cmd & !(PXCMD_ST | PXCMD_FRE));
        let _ = crate::timing::wait_timeout_us(500_000, || {
            let status = Self::r32_port(port_mmio, PXCMD);
            (status & (PXCMD_CR | PXCMD_FR)) == 0
        });
    }

    fn identify_port(&self, port: &mut AhciPort) -> Result<(u32, u64, bool), crate::DriverError> {
        Self::issue_command(port, 0xEC, 0, 1, false)?;
        let identify = unsafe { core::slice::from_raw_parts(port.data_buffer, 512) };
        let word = |index: usize| -> u16 {
            u16::from_le_bytes([identify[index * 2], identify[index * 2 + 1]])
        };

        let lba48 = (word(83) & (1 << 10)) != 0;
        let total_sectors = if lba48 {
            (word(100) as u64)
                | ((word(101) as u64) << 16)
                | ((word(102) as u64) << 32)
                | ((word(103) as u64) << 48)
        } else {
            (word(60) as u64) | ((word(61) as u64) << 16)
        };
        if total_sectors == 0 {
            return Err(crate::DriverError::Protocol);
        }

        // ATA words 117–118 advertise a logical sector size when word 106
        // marks it valid. Legacy disks use the 512-byte default.
        let sector_size = if (word(106) & (1 << 14)) != 0 && (word(106) & (1 << 12)) != 0 {
            (word(117) as u32 | ((word(118) as u32) << 16)).saturating_mul(2)
        } else {
            512
        };
        if !matches!(sector_size, 512 | 1024 | 2048 | 4096)
            || sector_size as usize > port.data_buffer_size
        {
            return Err(crate::DriverError::NotSupported);
        }
        Ok((sector_size, total_sectors, lba48))
    }

    fn issue_command(
        port: &mut AhciPort,
        command: u8,
        lba: u64,
        count: u16,
        write: bool,
    ) -> Result<(), crate::DriverError> {
        if count == 0 || count as usize * port.sector_size as usize > port.data_buffer_size {
            return Err(crate::DriverError::InvalidArgument);
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(crate::DriverError::InvalidArgument)?;
        if !port.lba48 && end > (1u64 << 28) {
            return Err(crate::DriverError::InvalidArgument);
        }
        if Self::r32_port(port.port_mmio, PXCI) & 1 != 0 {
            return Err(crate::DriverError::Busy);
        }

        let bytes = count as usize * port.sector_size as usize;
        unsafe {
            ptr::write_bytes(port.cmd_table as *mut u8, 0, 4096);
            let header = &mut *port.cmd_list;
            header.dword0 = 5 | (1 << 16) | if write { 1 << 6 } else { 0 };
            header.prdbc = 0;
            let fis = &mut (*port.cmd_table).cfis;
            fis[0] = 0x27; // Register Host-to-Device FIS
            fis[1] = 1 << 7; // command/control bit
            fis[2] = command;
            let lba_bytes = lba.to_le_bytes();
            fis[4..7].copy_from_slice(&lba_bytes[..3]);
            fis[7] = (1 << 6)
                | if port.lba48 {
                    0
                } else {
                    ((lba >> 24) & 0x0F) as u8
                }; // LBA mode and LBA[27:24] for legacy ATA
            if port.lba48 {
                fis[8..11].copy_from_slice(&lba_bytes[3..6]);
                fis[12..14].copy_from_slice(&count.to_le_bytes());
            } else {
                fis[12] = count as u8;
                fis[13] = 0;
            }
            (*port.cmd_table).prdt[0] = PrdtEntry {
                dba: port.data_buffer_phys as u32,
                dbau: (port.data_buffer_phys >> 32) as u32,
                rsvd: 0,
                dbc: (bytes as u32 - 1) | (1 << 31),
            };
        }

        Self::w32_port(port.port_mmio, PXSERR, 0xFFFF_FFFF);
        Self::w32_port(port.port_mmio, PXIS, 0xFFFF_FFFF);
        Self::w32_port(port.port_mmio, PXCI, 1);
        if crate::timing::wait_timeout_us(1_000_000, || {
            Self::r32_port(port.port_mmio, PXCI) & 1 == 0
        })
        .is_err()
        {
            Self::recover_port(port.port_mmio);
            return Err(crate::DriverError::TimedOut);
        }
        if Self::r32_port(port.port_mmio, PXIS) & PXIS_TFES != 0 {
            Self::recover_port(port.port_mmio);
            return Err(crate::DriverError::Io);
        }
        Ok(())
    }

    fn recover_port(port_mmio: *mut u32) {
        let cmd = Self::r32_port(port_mmio, PXCMD);
        Self::w32_port(port_mmio, PXCMD, cmd & !PXCMD_ST);
        let _ = crate::timing::wait_timeout_us(500_000, || {
            Self::r32_port(port_mmio, PXCMD) & PXCMD_CR == 0
        });
        Self::w32_port(port_mmio, PXCI, 0);
        Self::w32_port(port_mmio, PXSERR, 0xFFFF_FFFF);
        Self::w32_port(port_mmio, PXIS, 0xFFFF_FFFF);
        if cmd & PXCMD_ST != 0 {
            Self::w32_port(
                port_mmio,
                PXCMD,
                Self::r32_port(port_mmio, PXCMD) | PXCMD_ST,
            );
        }
    }

    fn transfer(
        &mut self,
        port_index: u8,
        lba: u64,
        count: u16,
        mut buffer: TransferBuffer<'_>,
    ) -> Result<(), crate::DriverError> {
        let port_position = self
            .ports
            .iter()
            .position(|port| port.index == port_index)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        let port = &mut self.ports[port_position];
        let bytes = (count as usize)
            .checked_mul(port.sector_size as usize)
            .ok_or(crate::DriverError::InvalidArgument)?;
        let buffer_len = match &buffer {
            TransferBuffer::Read(buf) => buf.len(),
            TransferBuffer::Write(buf) => buf.len(),
        };
        if buffer_len < bytes {
            return Err(crate::DriverError::InvalidArgument);
        }
        let write = matches!(&buffer, TransferBuffer::Write(_));
        if lba.checked_add(count as u64).is_none() || lba + count as u64 > port.total_sectors {
            return Err(crate::DriverError::InvalidArgument);
        }

        let per_command =
            (port.data_buffer_size / port.sector_size as usize).min(u16::MAX as usize) as u16;
        let mut completed = 0usize;
        while completed < count as usize {
            let chunk = (count as usize - completed).min(per_command as usize) as u16;
            let chunk_bytes = chunk as usize * port.sector_size as usize;
            let offset = completed * port.sector_size as usize;
            if let TransferBuffer::Write(buf) = &buffer {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buf.as_ptr().add(offset),
                        port.data_buffer,
                        chunk_bytes,
                    );
                }
            }
            Self::issue_command(
                port,
                if port.lba48 {
                    if write { 0x35 } else { 0x25 }
                } else if write {
                    0xCA
                } else {
                    0xC8
                },
                lba + completed as u64,
                chunk,
                write,
            )?;
            if let TransferBuffer::Read(buf) = &mut buffer {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        port.data_buffer,
                        buf.as_mut_ptr().add(offset),
                        chunk_bytes,
                    );
                }
            }
            completed += chunk as usize;
        }
        Ok(())
    }

    fn r32(&self, off: usize) -> u32 {
        let val = unsafe { ptr::read_volatile(self.hba_mmio.add(off / 4)) };
        if val == 0xFFFF_FFFF {
            log::warn!("AHCI: MMIO read at {:#x} returned 0xFFFF_FFFF", off);
        }
        val
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(self.hba_mmio.add(off / 4), v);
        }
    }
    fn r32_port(base: *mut u32, off: usize) -> u32 {
        let val = unsafe { ptr::read_volatile(base.add(off / 4)) };
        if val == 0xFFFF_FFFF {
            log::warn!("AHCI: port MMIO read at {:#x} returned 0xFFFF_FFFF", off);
        }
        val
    }
    fn w32_port(base: *mut u32, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(base.add(off / 4), v);
        }
    }
}

// ── Globals ──────────────────────────────────────────────────────

/// Initialize one AHCI PCI function and return its stable controller index.
pub fn init_device(
    ctx: &dyn DriverContext,
    device: PciDevice,
) -> Result<usize, crate::DriverError> {
    if device.class_code != 0x01 || device.subclass != 0x06 {
        return Err(crate::DriverError::DeviceNotFound);
    }

    let key = (device.bus, device.device, device.function);
    {
        let controllers = CONTROLLERS.lock();
        if let Some((index, controller)) =
            controllers
                .iter()
                .enumerate()
                .find_map(|(index, controller)| {
                    let matches = {
                        let controller_guard = controller.lock();
                        controller_guard.device.bus == device.bus
                            && controller_guard.device.device == device.device
                            && controller_guard.device.function == device.function
                    };
                    matches.then(|| (index, Arc::clone(controller)))
                })
        {
            drop(controllers);
            controller.lock().probe_ports(ctx);
            return Ok(index);
        }
        if !INITIALIZING.lock().insert(key) {
            return Err(crate::DriverError::Busy);
        }
    }

    if !device.enable_memory_access() {
        INITIALIZING.lock().remove(&key);
        return Err(crate::DriverError::Io);
    }
    let controller = match AhciController::init(ctx, device) {
        Some(controller) => controller,
        None => {
            INITIALIZING.lock().remove(&key);
            return Err(crate::DriverError::DeviceFault);
        }
    };
    let mut controllers = CONTROLLERS.lock();
    let index = controllers.len();
    log::info!(
        "AHCI: controller initialized as ahci{} ({} ports)",
        index,
        controller.num_ports
    );
    controllers.push(Arc::new(Mutex::new(controller)));
    INITIALIZING.lock().remove(&key);
    Ok(index)
}

/// Return the number of initialized AHCI controllers.
pub fn controller_count() -> usize {
    CONTROLLERS.lock().len()
}

/// Return the number of ATA disks currently identified on one controller.
pub fn device_count(controller_index: usize) -> usize {
    devices()
        .into_iter()
        .filter(|device| device.controller_index == controller_index)
        .count()
}

/// Return all ATA disks that completed IDENTIFY during AHCI initialization.
pub fn devices() -> Vec<AhciDeviceInfo> {
    let controllers = CONTROLLERS.lock().clone();
    controllers
        .iter()
        .enumerate()
        .flat_map(|(controller_index, controller)| {
            let controller = controller.lock();
            controller
                .ports
                .iter()
                .map(move |port| AhciDeviceInfo {
                    controller_index,
                    port_index: port.index,
                    sector_size: port.sector_size,
                    total_sectors: port.total_sectors,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Read sectors from an initialized ATA disk.
pub fn read_sectors(
    controller_index: usize,
    port_index: u8,
    lba: u64,
    count: u16,
    buf: &mut [u8],
) -> Result<(), crate::DriverError> {
    let mut controllers = CONTROLLERS.lock();
    let controller = controllers
        .get_mut(controller_index)
        .ok_or(crate::DriverError::DeviceNotFound)?;
    let controller = Arc::clone(controller);
    drop(controllers);
    controller
        .lock()
        .transfer(port_index, lba, count, TransferBuffer::Read(buf))
}

/// Write sectors to an initialized ATA disk.
pub fn write_sectors(
    controller_index: usize,
    port_index: u8,
    lba: u64,
    count: u16,
    buf: &[u8],
) -> Result<(), crate::DriverError> {
    let controllers = CONTROLLERS.lock();
    let controller = controllers
        .get(controller_index)
        .ok_or(crate::DriverError::DeviceNotFound)?;
    let controller = Arc::clone(controller);
    drop(controllers);
    controller
        .lock()
        .transfer(port_index, lba, count, TransferBuffer::Write(buf))
}
