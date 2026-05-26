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
use nitrogen::pci::PciDevice;
use petroleum::initializer::FrameAllocator;
use spin::Mutex;

/// Global list of discovered AHCI controllers.
static CONTROLLERS: Mutex<Vec<AhciController>> = Mutex::new(Vec::new());

// ── HBA memory register offsets ──────────────────────────────────

const HBA_CAP: usize      = 0x00; // Host Capability
const HBA_GHC: usize      = 0x04; // Global Host Control
const HBA_IS: usize       = 0x08; // Interrupt Status
const HBA_PI: usize       = 0x0C; // Ports Implemented
const HBA_VS: usize       = 0x10; // Version
const HBA_CCC_CTL: usize  = 0x14; // Command Completion Coalescing Control
const HBA_CCC_PORTS: usize = 0x18; // Command Completion Coalescing Ports
const HBA_EM_LOC: usize   = 0x1C; // Enclosure Management Location
const HBA_EM_CTL: usize   = 0x20; // Enclosure Management Control
const HBA_CAP2: usize     = 0x24; // Host Capabilities Extended
const HBA_BOHC: usize     = 0x28; // BIOS/OS Handoff Control and Status

// ── GHC bits ─────────────────────────────────────────────────────
const GHC_HR: u32  = 1 << 0; // HBA Reset
const GHC_IE: u32  = 1 << 1; // Interrupt Enable
const GHC_AE: u32  = 1 << 31; // AHCI Enable

// ── Port register offsets (relative to port base) ────────────────
const PXCLB: usize  = 0x00; // Command List Base Address
const PXCLBU: usize = 0x04; // Command List Base Address Upper
const PXFB: usize   = 0x08; // FIS Base Address
const PXFBU: usize  = 0x0C; // FIS Base Address Upper
const PXIS: usize   = 0x10; // Interrupt Status
const PXIE: usize   = 0x14; // Interrupt Enable
const PXCMD: usize  = 0x18; // Command and Status
const PXTFD: usize  = 0x20; // Task File Data
const PXSIG: usize  = 0x24; // Signature
const PXSSTS: usize = 0x28; // SATA Status (SCR0: SStatus)
const PXSCTL: usize = 0x2C; // SATA Control (SCR2: SControl)
const PXSERR: usize = 0x30; // SATA Error (SCR1: SError)
const PXSACT: usize = 0x34; // SATA Active
const PXCI: usize   = 0x38; // Command Issue
const PXSNTF: usize = 0x3C; // SATA Notification

// ── PxCMD bits ───────────────────────────────────────────────────
const PXCMD_ST:  u32 = 1 << 0;  // Start DMA
const PXCMD_SUD: u32 = 1 << 1;  // Spin-Up Device
const PXCMD_POD: u32 = 1 << 2;  // Power-On Device
const PXCMD_FRE: u32 = 1 << 4;  // FIS Receive Enable
const PXCMD_FR:  u32 = 1 << 14; // FIS Receive Running
const PXCMD_CR:  u32 = 1 << 15; // Command List Running

// ── SATA status (PxSSTS) ────────────────────────────────────────
const SSTS_DET_MASK: u32 = 0x0F;
const SSTS_DET_PHY_OK: u32 = 0x03;

// ── Command Header ───────────────────────────────────────────────
#[repr(C)]
struct CommandHeader {
    dword0: u32, // CFL(5) | PMP(4) | PRDTL(16) | Rsvd(1) | A(1) | W(1) | P(1) | R(1) | C(1)
    prdbc: u32,  // PRD Byte Count transferred
    ctba: u32,   // Command Table Base Address (low)
    ctbau: u32,  // Command Table Base Address (upper)
    rsvd: [u32; 4],
}

// ── Command Table ────────────────────────────────────────────────
#[repr(C, align(128))]
struct CommandTable {
    cfis: [u8; 64],   // Command FIS
    acmd: [u8; 16],   // ATAPI Command
    rsvd: [u8; 48],
    prdt: [PrdtEntry; 1], // PRDT (at least 1 entry)
}

// ── PRDT Entry ───────────────────────────────────────────────────
#[repr(C)]
struct PrdtEntry {
    dba: u32,   // Data Base Address (low)
    dbau: u32,  // Data Base Address (upper)
    rsvd: u32,
    dbc: u32,   // Byte Count (22 bits) | Rsvd(9) | I(1)
}

const PRDT_I: u32 = 1 << 31; // Interrupt on completion

// ── Received FIS structure ───────────────────────────────────────
#[repr(C, align(256))]
struct ReceivedFis {
    dsfis: [u8; 28],   // DMA Setup FIS
    pad0: [u8; 4],
    psfis: [u8; 24],   // PIO Setup FIS
    pad1: [u8; 8],
    rfis: [u8; 24],    // D2H Register FIS
    pad2: [u8; 4],
    sdbfis: [u8; 8],   // Set Device Bits FIS
    ufis: [u8; 64],    // Unknown FIS
    rsvd: [u8; 96],
}

// ── ATA commands ─────────────────────────────────────────────────
const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;

// ── Controller ───────────────────────────────────────────────────

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
    device: PciDevice,
    hba_mmio: *mut u32,
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
    pub fn init(device: PciDevice) -> Option<Self> {
        let bar5 = device.get_bar_info(5)?;
        if bar5.is_io {
            return None; // Legacy mode — not supported
        }
        let hba_phys = bar5.address;
        let hba_virt = petroleum::common::memory::physical_to_virtual(hba_phys as usize) as *mut u32;

        // Map the ABAR MMIO region
        {
            let mut mgr = crate::memory_management::get_memory_manager().lock();
            let m = mgr.as_mut()?;
            m.map_mmio_region(hba_phys as usize, hba_virt as usize, bar5.size as usize).ok()?;
        }

        let mut ctrl = Self { device, hba_mmio: hba_virt, hba_phys, num_ports: 0 };

        // Enable AHCI and reset HBA
        let ghc = ctrl.r32(HBA_GHC);
        ctrl.w32(HBA_GHC, ghc | GHC_AE); // AHCI Enable
        ctrl.w32(HBA_GHC, ghc | GHC_AE | GHC_HR); // HBA Reset
        for _ in 0..1_000_000 { core::hint::spin_loop(); }
        ctrl.w32(HBA_GHC, ghc | GHC_AE); // Clear reset
        for _ in 0..100_000 { core::hint::spin_loop(); }

        let pi = ctrl.r32(HBA_PI); // Ports Implemented
        ctrl.num_ports = pi.count_ones() as u32;

        // Initialise each implemented port
        for i in 0..32 {
            if (pi >> i) & 1 == 0 { continue; }
            ctrl.init_port(i);
        }

        Some(ctrl)
    }

    fn init_port(&self, port: u8) {
        let port_base = 0x100 + (port as usize) * 0x80;
        let port_mmio = unsafe { self.hba_mmio.add(port_base / 4) };

        // Stop command engine and FIS receive
        let cmd = self.r32_port(port_mmio, PXCMD);
        self.w32_port(port_mmio, PXCMD, cmd & !(PXCMD_ST | PXCMD_FRE));
        // Wait for CR and FR to clear
        for _ in 0..1_000_000 {
            let c = self.r32_port(port_mmio, PXCMD);
            if (c & (PXCMD_CR | PXCMD_FR)) == 0 { break; }
            core::hint::spin_loop();
        }

        // Check device presence
        let ssts = self.r32_port(port_mmio, PXSSTS);
        let det = ssts & SSTS_DET_MASK;
        if det != SSTS_DET_PHY_OK {
            log::info!("AHCI port {}: no device (SSTS={:#x})", port, ssts);
            return;
        }

        // Allocate command list (1 slot) and FIS
        let cmd_list_phys = allocate_frame_phys();
        let cmd_list = phys_to_virt_mut::<CommandHeader>(cmd_list_phys);
        let fis_phys = allocate_frame_phys();
        let fis = phys_to_virt_mut::<ReceivedFis>(fis_phys);
        let cmd_table_phys = allocate_frame_phys();
        let cmd_table = phys_to_virt_mut::<CommandTable>(cmd_table_phys);

        unsafe {
            ptr::write_bytes(cmd_list, 0, 4096);
            ptr::write_bytes(fis, 0, 4096);
            ptr::write_bytes(cmd_table, 0, 4096);
        }

        // Set command list and FIS base
        self.w32_port(port_mmio, PXCLB, cmd_list_phys as u32);
        self.w32_port(port_mmio, PXCLBU, (cmd_list_phys >> 32) as u32);
        self.w32_port(port_mmio, PXFB, fis_phys as u32);
        self.w32_port(port_mmio, PXFBU, (fis_phys >> 32) as u32);

        // Link command header[0] → command table
        unsafe {
            (*cmd_list).ctba = cmd_table_phys as u32;
            (*cmd_list).ctbau = (cmd_table_phys >> 32) as u32;
            (*cmd_list).dword0 = 0; // no PRDT entries yet
        }

        // Clear errors
        self.w32_port(port_mmio, PXSERR, 0xFFFFFFFF);
        self.w32_port(port_mmio, PXIS, 0xFFFFFFFF);
        self.w32_port(port_mmio, PXIE, 0);

        // Enable FIS receive and start command engine
        self.w32_port(port_mmio, PXCMD, cmd | PXCMD_FRE | PXCMD_ST);
    }

    fn r32(&self, off: usize) -> u32 {
        unsafe { ptr::read_volatile(self.hba_mmio.add(off / 4)) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { ptr::write_volatile(self.hba_mmio.add(off / 4), v); }
    }
    fn r32_port(&self, base: *mut u32, off: usize) -> u32 {
        unsafe { ptr::read_volatile(base.add(off / 4)) }
    }
    fn w32_port(&self, base: *mut u32, off: usize, v: u32) {
        unsafe { ptr::write_volatile(base.add(off / 4), v); }
    }
}

// ── Globals ──────────────────────────────────────────────────────

/// Initialise all AHCI controllers found on the PCI bus.
pub fn init() {
    let mut scanner = nitrogen::pci::PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        // SATA controller: class 0x01 (mass storage), subclass 0x06
        if dev.class_code == 0x01 && dev.subclass == 0x06 {
            log::info!("AHCI: found device {:#06x}:{:#06x}", dev.vendor_id, dev.device_id);
            dev.enable_memory_access();
            if let Some(ctrl) = AhciController::init(dev.clone()) {
                log::info!("AHCI: controller initialised ({} ports)", ctrl.num_ports);
                CONTROLLERS.lock().push(ctrl);
            }
        }
    }
    if CONTROLLERS.lock().is_empty() {
        log::info!("AHCI: no SATA controllers found");
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn allocate_frame_phys() -> u64 {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().expect("ahci: memory manager not ready");
    m.allocate_frame().expect("ahci: frame alloc failed") as u64
}

fn phys_to_virt_mut<T>(phys: u64) -> *mut T {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    (phys + off) as *mut T
}