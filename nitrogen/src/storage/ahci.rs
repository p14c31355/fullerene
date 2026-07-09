//! AHCI (Advanced Host Controller Interface) driver.
//!
//! Implements SATA disk access via PCI AHCI controllers.  Discovers the
//! HBA memory registers, resets ports, sends IDENTIFY DEVICE, and reads
//! sectors via DMA.
//!
//! # References
//! - Serial ATA AHCI 1.3.1 Specification
//! - Serial ATA Revision 3.0

use alloc::vec::Vec;
use core::ptr;
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::pci::{PciDevice, PciScanner};

/// Global list of discovered AHCI controllers.
static CONTROLLERS: Mutex<Vec<AhciController>> = Mutex::new(Vec::new());

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

// ── PxCMD bits ───────────────────────────────────────────────────
const PXCMD_ST: u32 = 1 << 0; // Start DMA
const PXCMD_FRE: u32 = 1 << 4; // FIS Receive Enable
const PXCMD_FR: u32 = 1 << 14; // FIS Receive Running
const PXCMD_CR: u32 = 1 << 15; // Command List Running

// ── SATA status (PxSSTS) ────────────────────────────────────────
const SSTS_DET_MASK: u32 = 0x0F;
const SSTS_DET_PHY_OK: u32 = 0x03;

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
    hba_mmio: *mut u32,
    port_mmio: *mut u32,
    cmd_list: *mut CommandHeader,
    cmd_list_phys: u64,
    fis: *mut ReceivedFis,
    fis_phys: u64,
    cmd_table: *mut CommandTable,
    cmd_table_phys: u64,
}

pub struct AhciController {
    #[allow(dead_code)]
    device: PciDevice,
    hba_mmio: *mut u32,
    #[allow(dead_code)]
    hba_phys: u64,
    /// Number of implemented ports (0–31).
    num_ports: u32,
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
        };

        let ghc = ctrl.r32(HBA_GHC);
        ctrl.w32(HBA_GHC, ghc | GHC_AE);
        ctrl.w32(HBA_GHC, ghc | GHC_AE | GHC_HR);
        if crate::timing::wait_timeout_us(500_000, || {
            (ctrl.r32(HBA_GHC) & GHC_HR) == 0
        }).is_err() {
            log::warn!("AHCI: HBA reset timed out — controller may be unresponsive");
            ctrl.w32(HBA_GHC, ctrl.r32(HBA_GHC) & !GHC_HR);
        }

        let pi = ctrl.r32(HBA_PI);
        ctrl.num_ports = pi.count_ones() as u32;

        for i in 0..32 {
            if (pi >> i) & 1 == 0 {
                continue;
            }
            let port_base = 0x100 + (i as usize) * 0x80;
            let port_mmio = unsafe { ctrl.hba_mmio.add(port_base / 4) };
            let ssts = unsafe { core::ptr::read_volatile(port_mmio.add(PXSSTS / 4)) };
            let det = ssts & SSTS_DET_MASK;
            if det != SSTS_DET_PHY_OK {
                log::info!("AHCI port {}: no PHY (SSTS={:#x}), skipping init", i, ssts);
                continue;
            }
            ctrl.init_port(ctx, i);
        }

        Some(ctrl)
    }

    fn init_port(&self, ctx: &dyn DriverContext, port: u8) {
        let port_base = 0x100 + (port as usize) * 0x80;
        let port_mmio = unsafe { self.hba_mmio.add(port_base / 4) };

        let cmd = self.r32_port(port_mmio, PXCMD);
        self.w32_port(port_mmio, PXCMD, cmd & !(PXCMD_ST | PXCMD_FRE));
        crate::timing::wait_timeout_us(500_000, || {
            let c = self.r32_port(port_mmio, PXCMD);
            (c & (PXCMD_CR | PXCMD_FR)) == 0
        }).ok();

        let ssts = self.r32_port(port_mmio, PXSSTS);
        let det = ssts & SSTS_DET_MASK;
        if det != SSTS_DET_PHY_OK {
            log::info!("AHCI port {}: no device (SSTS={:#x})", port, ssts);
            return;
        }

        let cmd_list_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!(
                    "AHCI port {}: failed to allocate cmd_list frame: {}",
                    port,
                    e
                );
                return;
            }
        };
        let cmd_list = ctx.phys_to_virt(cmd_list_phys) as *mut CommandHeader;
        let fis_phys = match ctx.allocate_frame() {
            Ok(phys) => phys,
            Err(e) => {
                log::error!("AHCI port {}: failed to allocate FIS frame: {}", port, e);
                ctx.free_frame(cmd_list_phys);
                return;
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
                return;
            }
        };
        let cmd_table = ctx.phys_to_virt(cmd_table_phys) as *mut CommandTable;

        unsafe {
            ptr::write_bytes(cmd_list as *mut u8, 0, 4096);
            ptr::write_bytes(fis as *mut u8, 0, 4096);
            ptr::write_bytes(cmd_table as *mut u8, 0, 4096);
        }

        self.w32_port(port_mmio, PXCLB, cmd_list_phys as u32);
        self.w32_port(port_mmio, PXCLBU, (cmd_list_phys >> 32) as u32);
        self.w32_port(port_mmio, PXFB, fis_phys as u32);
        self.w32_port(port_mmio, PXFBU, (fis_phys >> 32) as u32);

        unsafe {
            (*cmd_list).ctba = cmd_table_phys as u32;
            (*cmd_list).ctbau = (cmd_table_phys >> 32) as u32;
            (*cmd_list).dword0 = 0;
        }

        self.w32_port(port_mmio, PXSERR, 0xFFFFFFFF);
        self.w32_port(port_mmio, PXIS, 0xFFFFFFFF);
        self.w32_port(port_mmio, PXIE, 0);

        self.w32_port(port_mmio, PXCMD, cmd | PXCMD_FRE | PXCMD_ST);
    }

    fn r32(&self, off: usize) -> u32 {
        let val = unsafe { ptr::read_volatile(self.hba_mmio.add(off / 4)) };
        if val == 0xFFFF_FFFF {
            log::warn!(
                "AHCI: MMIO read at {:#x} returned 0xFFFF_FFFF",
                off
            );
        }
        val
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(self.hba_mmio.add(off / 4), v);
        }
    }
    fn r32_port(&self, base: *mut u32, off: usize) -> u32 {
        let val = unsafe { ptr::read_volatile(base.add(off / 4)) };
        if val == 0xFFFF_FFFF {
            log::warn!(
                "AHCI: port MMIO read at {:#x} returned 0xFFFF_FFFF",
                off
            );
        }
        val
    }
    fn w32_port(&self, base: *mut u32, off: usize, v: u32) {
        unsafe {
            ptr::write_volatile(base.add(off / 4), v);
        }
    }
}

// ── Globals ──────────────────────────────────────────────────────

/// Initialise all AHCI controllers found on the PCI bus.
///
/// `ctx` provides memory allocation and MMIO mapping services.
pub fn init(ctx: &dyn DriverContext) {
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        if dev.class_code == 0x01 && dev.subclass == 0x06 {
            log::info!(
                "AHCI: found device {:#06x}:{:#06x}",
                dev.vendor_id,
                dev.device_id
            );
            dev.enable_memory_access();
            if let Some(ctrl) = AhciController::init(ctx, dev.clone()) {
                log::info!("AHCI: controller initialised ({} ports)", ctrl.num_ports);
                CONTROLLERS.lock().push(ctrl);
            }
        }
    }
    if CONTROLLERS.lock().is_empty() {
        log::info!("AHCI: no SATA controllers found");
    }
}
