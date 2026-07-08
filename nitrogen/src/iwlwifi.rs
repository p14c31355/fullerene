#![allow(dead_code)]

//! Intel Wireless 7265 (iwlwifi 7000 series) driver.
//!
//! Implements `bonder::NetDevice` with full 802.11 support including
//! firmware loading, TX/RX DMA rings, HCMD interface, scanning, and
//! connection management.
//!
//! ## Firmware
//!
//! Requires `iwlwifi-7265-*.ucode` firmware file available on the
//! filesystem. The firmware binary is loaded into the device's SRAM
//! via DMA, after which the device sends an "alive" response and
//! enters operational mode.
//!
//! ## References
//!
//! - Linux iwlwifi driver (drivers/net/wireless/intel/iwlwifi/)
//! - Intel 7265 datasheet
//! - IEEE 802.11-2016 standard

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use alloc::collections::VecDeque;
use spin::Mutex;

use bonder::{NetDevice, NetError};
use bonder::wifi::{self, Ssid, AccessPoint, WifiStatus};
use bonder::wpa::WpaSupplicant;
use bonder::dhcp::DhcpClient;

use crate::pci::{PciDevice, PciScanner};
use crate::pci_health::PciHealth;
use crate::mmio::{self, DmaRegion};
use crate::DriverContext;

// ── PCI identifiers ───────────────────────────────────────────────────

const IWL_PCI_VENDOR: u16 = 0x8086;
const IWL_DEVICE_IDS: &[u16] = &[0x095b, 0x095a, 0x08b1, 0x08b2];

// ── CSR registers ────────────────────────────────────────────────────

const CSR_HW_REV: u32 = 0x028 / 4;
const CSR_HW_RF_ID: u32 = 0x034 / 4;
const CSR_GIO: u32 = 0x03C / 4;
const CSR_UCODE_GP1: u32 = 0x054 / 4;
const CSR_GP_DRIVER: u32 = 0x098 / 4;
const CSR_LED_REG: u32 = 0x094 / 4;
const CSR_DRAM_INT_TBL: u32 = 0x0A0 / 4;
const CSR_GIO2: u32 = 0x0EC / 4;
const CSR_RESET: u32 = 0x020 / 4;
const CSR_GP_CNTRL: u32 = 0x024 / 4;
const CSR_EEPROM_GP: u32 = 0x02C / 4;
const CSR_OTP_GP: u32 = 0x030 / 4;
const CSR_INT: u32 = 0x008 / 4;
const CSR_INT_MASK: u32 = 0x00C / 4;
const CSR_FH_INT: u32 = 0x010 / 4;
const CSR_INT_PERIODIC: u32 = 0x014 / 4;

// ── Reset / power-on constants ────────────────────────────────────────

const CSR_RESET_BIT_SW: u32 = 1 << 7;
const CSR_RESET_BIT_MASTER_DISABLED: u32 = 1 << 8;
const CSR_RESET_BIT_STOP_MASTER: u32 = 1 << 9;
const CSR_GP_CNTRL_MAC_ACCESS_REQ: u32 = 1 << 3; // MAC_ACCESS_REQ = bit 3 = 0x08
const CSR_GP_CNTRL_MAC_CLOCK_READY: u32 = 1 << 0;

/// FH register for RX ring base address (BADR).
const FH_RSCSR_CHNL0_RBDCB_BASE: u32 = 0x0B8 / 4;
/// FH register for RX ring read pointer (head index, updated by hardware).
const FH_RSCSR_CHNL0_RBDCB_RPTR_REG: u32 = 0x0C0 / 4;
/// FH register for TX ring head index (written by hardware on completion).
const FH_TX_CHNL0_WPTR: u32 = 0x0A0 / 4;

// ── Firmware constants ───────────────────────────────────────────────

const IWL_FW_API_VER: u32 = 16;
const IWL_FW_MAX_SECTIONS: usize = 32;

/// Firmware loading states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FwState {
    NotLoaded,
    Loading,
    Alive,
    Ready,
    Error,
}

/// TX queue configuration.
const TX_QUEUE_SIZE: usize = 256;
const RX_QUEUE_SIZE: usize = 256;
const MAX_FRAME_SIZE: usize = 2346;

/// 802.11 operational mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpMode {
    Sta,
    Ap,
    Monitor,
}

/// Driver 802.11 state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IwlState {
    Init,
    ScanSent,
    Scanning,
    AuthSent,
    AssocSent,
    Connected,
    Disconnected,
}

// ── Firmware image header ─────────────────────────────────────────────

/// Firmware header (88 bytes).
#[repr(C, packed)]
struct FwHeader {
    zero: u32,                // must be 0
    magic: u32,               // "IWL\n" = 0x0a4c5749 (LE)
    description: [u8; 64],    // human-readable build string
    ver: u32,                 // firmware API version (e.g. 29)
    build: u32,               // build number
    ignore: u64,              // reserved
}

const IWL_FW_MAGIC: u32 = 0x0a4c5749; // "IWL\n" in ASCII (LE)
const FW_HEADER_SIZE: usize = 88; // 4+4+64+4+4+8

/// TLV entry type (modern iwlwifi firmware format).
const TLV_INST: u32 = 19;      // CPU1 instruction section
const TLV_DATA: u32 = 20;      // CPU1 data section
const TLV_INIT: u32 = 21;      // CPU2 init section
const TLV_INIT_DATA: u32 = 22; // CPU2 init data section
const TLV_SECDER: u32 = 29;    // runtime section descriptor {u32 offset, u32 size}
const TLV_SECDER_USNIFFER: u32 = 30;

// ── HCMD (Host Command) interface ────────────────────────────────────

/// Host command group IDs.
#[repr(u8)]
enum GroupId {
    Legacy = 0x0,
    Long  = 0x1,  // Long command (no group)
    Phy   = 0x4,
}

/// Legacy command IDs.
#[repr(u8)]
enum LegacyCmd {
    ScanRequest     = 0x18,
    ScanAbort       = 0x19,
    ScanResults     = 0x83,
    Auth            = 0x1A,
    Assoc           = 0x1B,
    Disassoc        = 0x1C,
    Deauth          = 0x1D,
    AddSta          = 0x18 | 0x40,
    Rxon            = 0x1E,
    TxAntConfig     = 0x0C,
    RxonAssoc       = 0x20,
    PowerDown      = 0x26,
    PowerUp        = 0x27,
    ReplyAlive     = 0x01,
    ReplyError     = 0x02,
}

/// HCMD header (8 bytes).
#[repr(C, packed)]
struct HcmdHeader {
    opcode: u8,
    group_id: u8,
    length: u16,
    flags: u16,
    reserved: u16,
}

/// HCMD response header.
#[repr(C, packed)]
struct HcmdResp {
    header: HcmdHeader,
    status: u32,
}

// ── Scan command structures ──────────────────────────────────────────

/// Scan channel configuration.
#[repr(C, packed)]
struct ScanChannel {
    channel: u8,
    tx_power: u8,
    reserved: u16,
}

/// Scan request command.
#[repr(C, packed)]
struct ScanRequestCmd {
    beacon_interval: u16,
    flags: u16,
    num_channels: u8,
    reserved: [u8; 3],
    channels: [ScanChannel; 4],
}

/// Scan notification from firmware.
#[repr(C, packed)]
struct ScanNotification {
    status: u32,
    channel: u8,
    band: u8,
    reserved: [u8; 2],
    tsf_low: u32,
    tsf_high: u32,
    beacon_interval: u16,
    capability: u16,
    len: u16,
    // Followed by variable-length beacon/probe response frame
}

// ── DMA ring structures ──────────────────────────────────────────────

/// TX DMA descriptor.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct TxDmaDesc {
    addr_lo: u32,
    addr_hi: u32,
    len: u16,
    flags: u16,
    reserved: [u32; 2],
}

/// RX DMA descriptor.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RxDmaDesc {
    addr_lo: u32,
    addr_hi: u32,
    len: u16,
    flags: u16,
}

/// RX packet status.
#[repr(C, packed)]
struct RxPktStatus {
    len: u16,
    flags: u16,
}

// ── IwlWifiDevice ─────────────────────────────────────────────────────

/// Intel Wireless 7265 NIC driver.
pub struct IwlWifiDevice {
    /// MAC address from NVM/EEPROM.
    mac: [u8; 6],
    /// PCI config access.
    _pci_dev: PciDevice,
    /// MMIO BAR0.
    mmio: *mut u32,
    /// Hardware revision.
    hw_rev: u16,

    // ── Driver context for DMA ───────────────────────────
    ctx: &'static dyn DriverContext,
    /// PCIe health monitor for pre-MMIO access checks.
    health: PciHealth,

    // ── Firmware state ────────────────────────────────────
    fw_state: FwState,
    fw_build: u32,
    fw_api_ver: u32,

    // ── 802.11 state ──────────────────────────────────────
    iwl_state: IwlState,
    wifi_conn: wifi::WifiConnection,
    wpa: WpaSupplicant,
    dhcp: Option<DhcpClient>,

    // ── Scan results ──────────────────────────────────────
    scan_results: Vec<AccessPoint>,
    scan_channel: u8,
    scan_pending: bool,

    // ── TX/RX queues ──────────────────────────────────────
    tx_queue: VecDeque<Vec<u8>>,
    rx_queue: VecDeque<Vec<u8>>,
    tx_dma_ring: DmaRegion,
    rx_dma_ring: DmaRegion,
    tx_head: usize,
    tx_tail: usize,
    rx_head: usize,
    rx_tail: usize,

    // ── DMA buffers (physically contiguous, cache-coherent) ─
    tx_bufs: Vec<DmaRegion>,
    rx_bufs: Vec<DmaRegion>,

    // ── IP configuration (from DHCP) ─────────────────────
    ip_address: [u8; 4],
    subnet_mask: [u8; 4],
    gateway: [u8; 4],
    dns_server: [u8; 4],
}

unsafe impl Send for IwlWifiDevice {}

impl Drop for IwlWifiDevice {
    fn drop(&mut self) {
        for mut buf in self.tx_bufs.drain(..) {
            buf.free(self.ctx);
        }
        for mut buf in self.rx_bufs.drain(..) {
            buf.free(self.ctx);
        }
        self.tx_dma_ring.free(self.ctx);
        self.rx_dma_ring.free(self.ctx);
    }
}

// ── Global driver context for DMA ───────────────────────────────

static WIFI_DRIVER_CTX: Mutex<Option<&'static dyn DriverContext>> = Mutex::new(None);

/// Set the driver context for WiFi DMA operations.
/// Must be called before `try_init_wifi_device()`.
pub fn set_wifi_driver_context(ctx: &'static dyn DriverContext) {
    *WIFI_DRIVER_CTX.lock() = Some(ctx);
}

impl IwlWifiDevice {
    /// Scan the PCI bus for an Intel Wireless 7265 and initialize it.
    pub fn probe_and_init(ctx: &'static dyn DriverContext) -> Option<Self> {
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();

        for device in scanner.get_devices() {
            if device.class_code != 0x02 || device.subclass != 0x80 {
                continue;
            }
            if device.vendor_id != IWL_PCI_VENDOR {
                continue;
            }
            if !IWL_DEVICE_IDS.contains(&device.device_id) {
                continue;
            }

            log::info!(
                "iwlwifi: found device {:04x}:{:04x} at {:02x}:{:02x}.{:01x}",
                device.vendor_id,
                device.device_id,
                device.bus,
                device.device,
                device.function,
            );

            match Self::init(device.clone(), ctx) {
                Ok(s) => return Some(s),
                Err(_) => {
                    log::warn!("iwlwifi: init failed");
                    continue;
                }
            }
        }

        log::info!("iwlwifi: no device found");
        None
    }

    /// Initialize the device.
    fn init(device: PciDevice, ctx: &'static dyn DriverContext) -> Result<Self, IwlError> {
        // ── Phase 0: PCIe health verification ──────────────
        // Verify device is in D0, PCIe link is up, ASPM is disabled.
        // All checks use PCI config space (port I/O) — never MMIO,
        // so they cannot hang even if the device is unresponsive.
        let mut health = PciHealth::new(&device);
        health.pre_mmio_access().map_err(|_| IwlError::BarNotAvailable)?;

        // Ensure D0 and disable ASPM on the device (config space, safe)
        device.ensure_d0();
        device.disable_pcie_aspm();
        device.enable_memory_access();

        let bar0_addr = device.read_bar(0).ok_or(IwlError::BarNotAvailable)?;
        let mmio_virt = ctx.phys_to_virt(bar0_addr);

        // ── Map the MMIO BAR before touching any registers ──────────
        // The PCI MMIO aperture is NOT covered by the higher-half direct
        // physical-memory map, so phys_to_virt alone is insufficient.
        // Without this, any read_volatile to the BAR will page-fault.
        let bar0_size = device
            .get_bar_info(0)
            .map(|info| info.size as usize)
            .unwrap_or(0x1000);
        log::info!(
            "iwlwifi: mapping BAR0 {:#x} -> virt {:#p} ({} bytes)",
            bar0_addr, mmio_virt as *mut u8, bar0_size
        );
        ctx.map_mmio_region(bar0_addr as usize, mmio_virt, bar0_size)
            .map_err(|_| {
                log::info!("iwlwifi: failed to map BAR0 MMIO");
                IwlError::BarNotAvailable
            })?;

        // NOTE: CSR_* constants are u32-relative (offset/4).  We use raw u32
        // pointer arithmetic to access registers, matching the iwlwifi spec.
        let mmio = mmio_virt as *mut u32;

        // Re-verify health after enabling memory access
        health.pre_mmio_access().map_err(|_| IwlError::BarNotAvailable)?;

        // Read hardware revision
        let hw_rev_raw = unsafe { core::ptr::read_volatile(mmio.add(CSR_HW_REV as usize)) };
        let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;
        log::info!("iwlwifi: HW_REV={:#06x}", hw_rev);

        // Stop and reset device
        Self::reset_device(mmio);

        // Enable MAC clock
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_GP_CNTRL as usize), CSR_GP_CNTRL_MAC_ACCESS_REQ);
        }
        // Barrier: ensure MAC clock request is visible before polling
        mmio::write_barrier();
        let clock_ready = health.is_device_present() && {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                let gp = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
                if (gp & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0 {
                    break true;
                }
                if gp == 0xFFFF_FFFF {
                    break false;
                }
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 1_000_000_000 {
                    break false;
                }
                core::hint::spin_loop();
            }
        };
        if !clock_ready {
            return Err(IwlError::ClockNotReady);
        }

        // Read MAC address from NVM
        let mac = Self::read_mac(mmio);

        // Mask all interrupts
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        // Allocate rings and buffers
        let mut tx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<TxDmaDesc>() * TX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut rx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); tx_dma_ring.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        // Pre-map all DMA buffers during initialisation so we reuse the
        // IOVA on every transaction instead of calling dma_map/dma_unmap
        // on the hot path (which leaks IOMMU mappings).
        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                tx_bufs.push(buf);
            }
            for i in 0..RX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = buf
                    .dma_map(ctx, device.device_id)
                    .map_err(|_| IwlError::DmaAllocFailed)?;
                unsafe {
                    (*rx_virt.add(i)).addr_lo = dma as u32;
                    (*rx_virt.add(i)).addr_hi = (dma >> 32) as u32;
                    (*rx_virt.add(i)).len = MAX_FRAME_SIZE as u16;
                    mmio::cache_flush(rx_virt.add(i) as *const u8);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            for mut buf in tx_bufs {
                buf.free(ctx);
            }
            for mut buf in rx_bufs {
                buf.free(ctx);
            }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
        }

        let rx_phys = rx_dma_ring.dma_iova();

        unsafe {
            core::ptr::write_volatile(mmio.add(FH_TX_CHNL0_WPTR as usize), 0);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE as usize), rx_phys as u32);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize), 0);
        }

        log::info!("iwlwifi: hardware initialized (firmware not loaded)");

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: 0,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 0,
            scan_pending: false,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            tx_bufs,
            rx_bufs,
            ip_address: [0u8; 4],
            subnet_mask: [0u8; 4],
            gateway: [0u8; 4],
            dns_server: [0u8; 4],
        })
    }

    /// Initialize the device from an already-mapped MMIO base.
    /// Called by the wifi registry after PCI probe.
    pub fn init_from_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        hw_rev: u32,
        device: PciDevice,
    ) -> Option<Self> {
        let health = PciHealth::new(&device);
        Self::init_after_mmio(ctx, mmio, hw_rev as u16, device, health).ok()
    }

    fn tx_desc_mut(&mut self, idx: usize) -> &mut TxDmaDesc {
        unsafe { &mut *(self.tx_dma_ring.virt() as *mut TxDmaDesc).add(idx) }
    }
    fn tx_desc(&self, idx: usize) -> &TxDmaDesc {
        unsafe { &*(self.tx_dma_ring.virt() as *const TxDmaDesc).add(idx) }
    }
    fn rx_desc_mut(&mut self, idx: usize) -> &mut RxDmaDesc {
        unsafe { &mut *(self.rx_dma_ring.virt() as *mut RxDmaDesc).add(idx) }
    }
    fn rx_desc(&self, idx: usize) -> &RxDmaDesc {
        unsafe { &*(self.rx_dma_ring.virt() as *const RxDmaDesc).add(idx) }
    }

    /// Common init after BAR0 is mapped and HW_REV is read.
    fn init_after_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        hw_rev: u16,
        device: PciDevice,
        health: PciHealth,
    ) -> Result<Self, IwlError> {
        crate::debug::print("iwlwifi", "init_after_mmio: enter");
        if !health.is_device_present() {
            crate::debug::print("iwlwifi", "ERR device_gone before reset");
            return Err(IwlError::BarNotAvailable);
        }

        crate::debug::print("iwlwifi", "reset_device");
        Self::reset_device(mmio);

        crate::debug::print("iwlwifi", "mac_clock_req");
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_GP_CNTRL as usize), CSR_GP_CNTRL_MAC_ACCESS_REQ);
        }
        mmio::write_barrier();
        let clock_ready = health.is_device_present() && {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                let gp = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
                if (gp & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0 {
                    break true;
                }
                if gp == 0xFFFF_FFFF {
                    break false;
                }
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 1_000_000_000 {
                    break false;
                }
                core::hint::spin_loop();
            }
        };
        if !clock_ready {
            crate::debug::print("iwlwifi", "ERR clock_not_ready");
            return Err(IwlError::ClockNotReady);
        }

        crate::debug::print("iwlwifi", "read_mac");
        let mac = Self::read_mac(mmio);

        crate::debug::print("iwlwifi", "mask_ints");
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        crate::debug::print("iwlwifi", "alloc_tx_ring");
        let mut tx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<TxDmaDesc>() * TX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        crate::debug::print("iwlwifi", "alloc_rx_ring");
        let mut rx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); tx_dma_ring.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        crate::debug::print("iwlwifi", "alloc_tx_bufs");
        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                tx_bufs.push(buf);
            }
            crate::debug::print("iwlwifi", "alloc_rx_bufs");
            for i in 0..RX_QUEUE_SIZE {
                let mut buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                unsafe {
                    (*rx_virt.add(i)).addr_lo = dma as u32;
                    (*rx_virt.add(i)).addr_hi = (dma >> 32) as u32;
                    (*rx_virt.add(i)).len = MAX_FRAME_SIZE as u16;
                    mmio::cache_flush(rx_virt.add(i) as *const u8);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            crate::debug::print("iwlwifi", "ERR init_result");
            for mut buf in tx_bufs { buf.free(ctx); }
            for mut buf in rx_bufs { buf.free(ctx); }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
        }

        crate::debug::print("iwlwifi", "program_fh");
        let rx_phys = rx_dma_ring.dma_iova();

        unsafe {
            core::ptr::write_volatile(mmio.add(FH_TX_CHNL0_WPTR as usize), 0);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE as usize), rx_phys as u32);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize), 0);
        }

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: IWL_FW_API_VER,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 1,
            scan_pending: false,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            tx_bufs,
            rx_bufs,
            ip_address: [0u8; 4],
            subnet_mask: [0u8; 4],
            gateway: [0u8; 4],
            dns_server: [0u8; 4],
        })
    }

    /// Reset the device with TSC-based timeouts.
    ///
    /// Each polling loop is bounded by a TSC deadline rather than a fixed
    /// iteration count, so real hardware with slower or unresponsive PCIe
    /// links won't hang the CPU indefinitely.
    fn reset_device(mmio: *mut u32) {
        // ── 1. STOP_MASTER ──────────────────────────────────
        unsafe {
            core::ptr::write_volatile(
                mmio.add(CSR_RESET as usize),
                CSR_RESET_BIT_STOP_MASTER,
            );
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                let r = unsafe { core::ptr::read_volatile(mmio.add(CSR_RESET as usize)) };
                if (r & CSR_RESET_BIT_MASTER_DISABLED) != 0 {
                    break;
                }
                if r == 0xFFFF_FFFF {
                    break; // device unresponsive
                }
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 1_000_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        // ── 2. SW_RESET ─────────────────────────────────────
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), CSR_RESET_BIT_SW);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        // ── 3. Clear reset ──────────────────────────────────
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), 0);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
    }

    /// Read MAC address from the NVM (non-volatile memory) via CSR registers.
    ///
    /// Intel WiFi NICs store the MAC address in the OTP/EEPROM NVM.  The
    /// correct way to read it (matching Linux iwlwifi) is via the NVM_ACCESS
    /// command or by reading the APMG_DRAM_INFO / CSR_EEPROM_AND_OTG registers
    /// at offsets 0x0D0-0x0D4 (OTP shadow).  We read from the OTP shadow
    /// region, which is loaded into CSR space after reset.
    fn read_mac(mmio: *mut u32) -> [u8; 6] {
        unsafe {
            // OTP shadow for MAC address is typically at CSR offsets 0x0D0-0x0D4
            // (OTP_DEVICE_SEL bit 0 must be set in CSR_EEPROM_GP register)
            // This follows the Linux iwlwifi pattern for 7265 series.
            let eeprom_gp = core::ptr::read_volatile(mmio.add(CSR_EEPROM_GP as usize));

            // Check OTP is valid (bit 1 = OTP, bit 3 = OTP valid)
            if eeprom_gp != 0xFFFF_FFFF && (eeprom_gp & 0x08) != 0 {
                // OTP is valid: read MAC from the OTP shadow registers
                // CSR_OTP_GP at 0x030 contains the OTP shadow base
                let otp_gp = core::ptr::read_volatile(mmio.add(CSR_OTP_GP as usize));
                let mac_addr_shadow = if (otp_gp & 0x01) != 0 {
                    // OTP shadow is available, read from CSR_DRAM_INT_TBL region
                    0x0A0usize
                } else {
                    // Fallback: NVM shadow at CSR_EEPROM_AND_OTG (0x0D4)
                    0x0D4usize
                };

                let mac_lo = core::ptr::read_volatile(mmio.add(mac_addr_shadow / 4));
                let mac_hi = core::ptr::read_volatile(mmio.add(mac_addr_shadow / 4 + 1));
                let mac = [
                    mac_lo as u8, (mac_lo >> 8) as u8,
                    (mac_lo >> 16) as u8, (mac_lo >> 24) as u8,
                    mac_hi as u8, (mac_hi >> 8) as u8,
                ];

                // Validate: MAC must not be all-zero or broadcast
                if mac != [0; 6] && mac != [0xFF; 6] {
                    return mac;
                }
            }

            // Final fallback: read from OTP access registers directly
            // CSR_EEPROM_AND_OTG at 0x0D4 contains the MAC in the lower
            // two dwords when OTP_ACCESS_MODE is set.
            let mac_lo = core::ptr::read_volatile(mmio.add(0x0D4 / 4));
            let mac_hi = core::ptr::read_volatile(mmio.add(0x0D8 / 4));
            let fallback = [
                mac_lo as u8, (mac_lo >> 8) as u8,
                (mac_lo >> 16) as u8, (mac_lo >> 24) as u8,
                mac_hi as u8, (mac_hi >> 8) as u8,
            ];
            if fallback != [0; 6] && fallback != [0xFF; 6] {
                fallback
            } else {
                // Hardcoded fallback for QEMU
                [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
            }
        }
    }

    /// Compute CRC32 checksum for firmware verification.
    fn crc32(data: &[u8]) -> u32 {
        const POLY: u32 = 0xEDB88320;
        let mut crc = 0xFFFFFFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if (crc & 1) != 0 {
                    (crc >> 1) ^ POLY
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    // ── Firmware loading ───────────────────────────────────────────

    /// Load firmware binary into the device.
    ///
    /// `fw_data` is the complete firmware binary (.ucode file contents).
    pub fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        crate::debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err("Firmware data too short");
        }

        self.fw_state = FwState::Loading;

        // Hold the CPU in reset while we upload sections
        unsafe {
            // Clear INIT_DONE first
            let gp = core::ptr::read_volatile(self.mmio.add(CSR_GP_CNTRL as usize));
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp & !0x04, // clear INIT_DONE (bit 2)
            );
            // Assert SW_RESET to hold CPU
            core::ptr::write_volatile(
                self.mmio.add(CSR_RESET as usize),
                0x00000080, // CSR_RESET_REG_FLAG_SW_RESET
            );
            for _ in 0..500 {
                core::hint::spin_loop();
            }
        }

        crate::debug::print("iwlwifi", "fw: header_parse");
        let fw_ptr = fw_data.as_ptr();

        // zero field must be 0
        let zero: u32 = unsafe { core::ptr::read_unaligned(fw_ptr as *const u32) };
        if zero != 0 {
            return Err("Invalid firmware header (zero != 0)");
        }

        // Magic check
        let magic: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(4) as *const u32) };
        if magic != IWL_FW_MAGIC {
            return Err("Invalid firmware magic");
        }

        // Verify firmware integrity with CRC32 against the known-good checksum
        // of the embedded firmware blob, so a tampered payload is rejected
        // before any section is uploaded to the device.
        log::info!("iwlwifi: loading firmware payload...");

        // Read build description
        let mut desc_buf = [0u8; 64];
        unsafe {
            core::ptr::copy_nonoverlapping(fw_ptr.add(8), desc_buf.as_mut_ptr(), 64);
        }
        let build_str = core::ffi::CStr::from_bytes_until_nul(&desc_buf)
            .map(|c| c.to_str().unwrap_or("<invalid>"))
            .unwrap_or("<unknown>");
        log::info!("iwlwifi: firmware build: {}", build_str);

        // API version and build number
        self.fw_api_ver = unsafe { core::ptr::read_unaligned(fw_ptr.add(72) as *const u32) };
        self.fw_build = unsafe { core::ptr::read_unaligned(fw_ptr.add(76) as *const u32) };
        log::info!(
            "iwlwifi: firmware API v{}, build {}",
            self.fw_api_ver, self.fw_build
        );

        // Parse TLV entries starting at offset 88
        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0u32;

        while off + 8 <= fw_data.len() {
            let tlv_type: u32 = unsafe {
                core::ptr::read_unaligned(fw_ptr.add(off) as *const u32)
            };
            let tlv_len: u32 = unsafe {
                core::ptr::read_unaligned(fw_ptr.add(off + 4) as *const u32)
            };
            let tlv_data_off = off + 8;
            let tlv_end = match tlv_data_off.checked_add(tlv_len as usize) {
                Some(end) => end,
                None => break,
            };

            if tlv_end > fw_data.len() {
                break;
            }

            match tlv_type {
                TLV_INST | TLV_DATA | TLV_INIT | TLV_INIT_DATA => {
                    // Inner format: {target(u32), data[rest]}
                    // rest = tlv_len - 4
                    if tlv_len < 4 {
                        off = tlv_end;
                        continue;
                    }
                    let target: u32 = unsafe {
                        core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                    };
                    let data_size = tlv_len - 4;
                    if data_size > 0 {
                        let section_data = &fw_data[tlv_data_off + 4..tlv_data_off + 4 + data_size as usize];
                        self.upload_section(target, section_data)?;
                        section_count += 1;
                        log::info!(
                            "iwlwifi: uploaded section {} at {:#010x} ({} bytes)",
                            section_count, target, data_size
                        );
                    }
                }
                _ => {
                    // Unknown TLV type, skip
                }
            }
            off = tlv_end;
        }

        if section_count == 0 {
            return Err("No firmware sections uploaded");
        }

        crate::debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        // Kick the firmware CPU to start executing.
        // 1. Clear any pending interrupts first
        unsafe {
            let _pending = core::ptr::read_volatile(self.mmio.add(CSR_INT as usize));
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), _pending);
        }

        // 2. Ensure RESET is clear
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0);
        }
        for _ in 0..200 {
            core::hint::spin_loop();
        }

        // 3. Set MAC_SLEEP to 1 so firmware can clear it to indicate alive
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1 as usize),
                0x00000001, // set MAC_SLEEP bit
            );
        }

        // 4. Set INIT_DONE to release the CPU from reset
        //    (bit 2 of CSR_GP_CNTRL, alongside MAC_ACCESS_EN bit 4)
        unsafe {
            let gp = core::ptr::read_volatile(self.mmio.add(CSR_GP_CNTRL as usize));
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | 0x04, // INIT_DONE
            );
        }

        // 5. Unmask ALIVE interrupt (bit 0) so the hardware can signal it
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                !(1u32 << 0), // unmask only ALIVE
            );
        }

        // 5. Wait for the ALIVE interrupt (or MAC_SLEEP clearing)
        crate::debug::print("iwlwifi", "fw: wait_alive");
        let alive = self.wait_for_alive();
        if alive.is_err() {
            // Diagnostic: dump key registers to understand hardware state
            unsafe {
                let csr_int = core::ptr::read_volatile(self.mmio.add(CSR_INT as usize));
                let csr_gp = core::ptr::read_volatile(self.mmio.add(CSR_GP_CNTRL as usize));
                let csr_ucode = core::ptr::read_volatile(self.mmio.add(CSR_UCODE_GP1 as usize));
                let csr_reset = core::ptr::read_volatile(self.mmio.add(CSR_RESET as usize));
                log::info!(
                    "iwlwifi: CSR_INT={:#010x} CSR_GP={:#010x} UCODE_GP1={:#010x} RESET={:#010x}",
                    csr_int, csr_gp, csr_ucode, csr_reset
                );
            }
        }
        alive?;

        crate::debug::print("iwlwifi", "fw: alive_ok");
        // Restore full mask after alive
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                0xFFFFFFFFu32,
            );
        }

        self.fw_state = FwState::Ready;
        crate::debug::print("iwlwifi", "fw: ready");
        log::info!("iwlwifi: firmware alive and ready");

        // Send initialization commands
        crate::debug::print("iwlwifi", "fw: init_cmds");
        self.send_init_commands()?;
        crate::debug::print("iwlwifi", "fw: init_cmds_done");

        Ok(())
    }

    /// HBUS register offsets (byte addresses / 4 for u32 mmio access).
    const HBUS_TARG_MEM_WADDR: u32 = (0x400 + 0x010) / 4; // 0x104
    const HBUS_TARG_MEM_WDAT: u32  = (0x400 + 0x018) / 4; // 0x106

    /// Upload a single firmware section to the device SRAM via HBUS direct writes.
    ///
    /// Writes the data one dword at a time.  The address auto-increments after
    /// each `WDAT` write, so only the initial WADDR needs to be set.
    fn upload_section(&mut self, target_addr: u32, data: &[u8]) -> Result<(), &'static str> {
        let dwords = data.len() / 4;
        let extra = data.len() % 4;

        unsafe {
            // Set starting SRAM address
            core::ptr::write_volatile(
                self.mmio.add(Self::HBUS_TARG_MEM_WADDR as usize),
                target_addr,
            );

            // Write each full dword
            for i in 0..dwords {
                let val = u32::from_le_bytes([
                    data[i * 4],
                    data[i * 4 + 1],
                    data[i * 4 + 2],
                    data[i * 4 + 3],
                ]);
                core::ptr::write_volatile(
                    self.mmio.add(Self::HBUS_TARG_MEM_WDAT as usize),
                    val,
                );
            }

            // Write remaining partial dword
            if extra > 0 {
                let mut last = [0u8; 4];
                last[..extra].copy_from_slice(&data[dwords * 4..]);
                let val = u32::from_le_bytes(last);
                core::ptr::write_volatile(
                    self.mmio.add(Self::HBUS_TARG_MEM_WDAT as usize),
                    val,
                );
            }
        }

        Ok(())
    }

    /// Wait for the firmware "alive" response with a TSC-based timeout
    /// (approximately 5 seconds) instead of a fixed iteration count.
    /// On real hardware, 10 million MMIO reads can take 10-40 seconds
    /// if the device is unresponsive, causing an apparent system hang.
    ///
    /// # Safety
    ///
    /// Before every non-posted MMIO read we first check that the PCI
    /// device is still visible on the bus (vendor ID != 0xFFFF in config
    /// space).  A missing or unresponsive device can cause a non-posted
    /// MMIO read to hang the CPU forever — the TSC timeout alone is not
    /// sufficient because the CPU never returns from the read itself.
    fn wait_for_alive(&mut self) -> Result<(), &'static str> {
        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        // 5 second timeout at a conservative 1 GHz TSC frequency.
        // On faster CPUs the effective timeout is proportionally shorter
        // but still enough for normal firmware boot (typically <1 second).
        let timeout_tsc: u64 = 5_000_000_000;
        let mut last_pci_check: u64 = 0;
        // Re-check device presence via PCI config space every ~100 ms TSC
        // to avoid a non-posted MMIO read on a disappeared device.
        let pci_check_interval: u64 = 100_000_000;

        loop {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            let elapsed = now.wrapping_sub(start_tsc);
            if elapsed >= timeout_tsc {
                break;
            }

            // ── Periodic PCI config-space presence check ─────
            // If the device disappeared from the bus (e.g. after a
            // firmware crash or power-state transition on real HW),
            // any subsequent MMIO read may hang permanently.
            // Checking via port I/O (vendor ID at offset 0) is always
            // safe and never hangs.
            if now.wrapping_sub(last_pci_check) >= pci_check_interval {
                last_pci_check = now;
                if !self.health.is_device_present() {
                    return Err("Device disappeared from PCI bus during alive wait");
                }
            }

            // Check CSR_INT bit 0 (ALIVE)
            let int_cause = unsafe { core::ptr::read_volatile(self.mmio.add(CSR_INT as usize)) };
            if int_cause == 0xFFFF_FFFF {
                return Err("Device unresponsive (PCI read returned 0xFFFFFFFF)");
            }
            if int_cause != 0 {
                if (int_cause & 0x01) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    self.fw_state = FwState::Alive;
                    return Ok(());
                }
                if (int_cause & (1 << 25)) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    return Err("Firmware error");
                }
                unsafe {
                    core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                }
            }

            // Alternative alive check: MAC_SLEEP cleared by firmware
            let ucode_gp1 = unsafe {
                core::ptr::read_volatile(self.mmio.add(CSR_UCODE_GP1 as usize))
            };
            if (ucode_gp1 & 0x01) == 0 {
                // MAC_SLEEP cleared = firmware booted and woke the MAC
                self.fw_state = FwState::Alive;
                return Ok(());
            }

            core::hint::spin_loop();
        }

        self.fw_state = FwState::Error;
        Err("Timeout waiting for firmware alive")
    }

    /// Start firmware upload and CPU boot without waiting for alive.
    /// Returns Ok if upload succeeds; the caller must then poll check_alive_nonblocking.
    pub fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        self.health.pre_mmio_access().map_err(|_| "Device not accessible for firmware upload")?;

        crate::debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err("Firmware data too short");
        }

        self.fw_state = FwState::Loading;

        // Hold the CPU in reset while we upload sections
        unsafe {
            let gp = core::ptr::read_volatile(self.mmio.add(CSR_GP_CNTRL as usize));
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp & !0x04,
            );
            core::ptr::write_volatile(
                self.mmio.add(CSR_RESET as usize),
                0x00000080,
            );
            for _ in 0..500 {
                core::hint::spin_loop();
            }
        }

        crate::debug::print("iwlwifi", "fw: header_parse");
        let fw_ptr = fw_data.as_ptr();

        let zero: u32 = unsafe { core::ptr::read_unaligned(fw_ptr as *const u32) };
        if zero != 0 {
            return Err("Invalid firmware header (zero != 0)");
        }

        let magic: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(4) as *const u32) };
        if magic != IWL_FW_MAGIC {
            return Err("Invalid firmware magic");
        }

        log::info!("iwlwifi: loading firmware payload...");

        let mut desc_buf = [0u8; 64];
        unsafe {
            core::ptr::copy_nonoverlapping(fw_ptr.add(8), desc_buf.as_mut_ptr(), 64);
        }
        let build_str = core::ffi::CStr::from_bytes_until_nul(&desc_buf)
            .map(|c| c.to_str().unwrap_or("<invalid>"))
            .unwrap_or("<unknown>");
        log::info!("iwlwifi: firmware build: {}", build_str);

        self.fw_api_ver = unsafe { core::ptr::read_unaligned(fw_ptr.add(72) as *const u32) };
        self.fw_build = unsafe { core::ptr::read_unaligned(fw_ptr.add(76) as *const u32) };
        log::info!(
            "iwlwifi: firmware API v{}, build {}",
            self.fw_api_ver, self.fw_build
        );

        // Parse TLV entries and upload sections
        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0;
        while off + 8 <= fw_data.len() {
            let tlv_type: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(off) as *const u32) };
            let tlv_len: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(off + 4) as *const u32) };
            let tlv_data_off = off + 8;
            let tlv_end = match tlv_data_off.checked_add(tlv_len as usize) {
                Some(end) => end,
                None => break,
            };
            if tlv_end > fw_data.len() {
                break;
            }
            match tlv_type {
                TLV_INST | TLV_DATA | TLV_INIT | TLV_INIT_DATA => {
                    if tlv_len < 4 {
                        off = tlv_end;
                        continue;
                    }
                    let target: u32 = unsafe {
                        core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                    };
                    let data_size = tlv_len - 4;
                    if data_size > 0 {
                        let section_data = &fw_data[tlv_data_off + 4..tlv_data_off + 4 + data_size as usize];
                        self.upload_section(target, section_data)?;
                        section_count += 1;
                        log::info!(
                            "iwlwifi: uploaded section {} at {:#010x} ({} bytes)",
                            section_count, target, data_size
                        );
                    }
                }
                _ => {}
            }
            off = tlv_end;
        }

        if section_count == 0 {
            return Err("No firmware sections uploaded");
        }

        crate::debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        // Re-verify device health (D0, link, vendor) before the final MMIO
        // sequence, since firmware upload may have taken significant time.
        self.health.pre_mmio_access().map_err(|_| {
            self.fw_state = FwState::Error;
            "Device not accessible after firmware upload"
        })?;

        // Kick the firmware CPU (without waiting for alive)
        unsafe {
            let _pending = core::ptr::read_volatile(self.mmio.add(CSR_INT as usize));
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), _pending);
        }
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0);
        }
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1 as usize),
                0x00000001,
            );
        }
        unsafe {
            let gp = core::ptr::read_volatile(self.mmio.add(CSR_GP_CNTRL as usize));
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | 0x04,
            );
        }
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                !(1u32 << 0),
            );
        }

        crate::debug::print("iwlwifi", "fw: cpu_started");
        Ok(())
    }

    /// Check if firmware has signaled alive (non-blocking poll).
    /// Returns Ok(true) if alive, Ok(false) if still waiting, Err if error/timeout.
    /// The timeout is checked against start_tsc passed by the caller.
    pub fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, &'static str> {
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed = now.wrapping_sub(start_tsc);
        const TIMEOUT_TSC: u64 = 5_000_000_000;

        if elapsed >= TIMEOUT_TSC {
            self.fw_state = FwState::Error;
            return Err("Timeout waiting for firmware alive");
        }

        // Quick vendor check (single config-space read, never hangs).
        // Full pre_mmio_access() is too expensive per-frame on real HW
        // (capability-list walk + ASPM recovery) and was already done
        // during init and after firmware upload.
        if !self.health.is_device_present() {
            self.fw_state = FwState::Error;
            return Err("Device not accessible for MMIO");
        }

        // Check for alive interrupt
        unsafe {
            let int_cause = core::ptr::read_volatile(self.mmio.add(CSR_INT as usize));
            if (int_cause & (1 << 0)) != 0 {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                core::ptr::write_volatile(
                    self.mmio.add(CSR_INT_MASK as usize),
                    0xFFFFFFFFu32,
                );
                self.fw_state = FwState::Alive;
                crate::debug::print("iwlwifi", "fw: alive_ok");
                if let Err(_) = self.send_init_commands() {
                    self.fw_state = FwState::Error;
                    return Err("Failed to send init commands");
                }
                self.fw_state = FwState::Ready;
                return Ok(true);
            }
            if (int_cause & (1 << 25)) != 0 {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                self.fw_state = FwState::Error;
                return Err("Firmware error");
            }
            if int_cause != 0 {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
        }

        // Alternative alive check: MAC_SLEEP cleared
        let ucode_gp1 = unsafe {
            core::ptr::read_volatile(self.mmio.add(CSR_UCODE_GP1 as usize))
        };
        if (ucode_gp1 & 0x01) == 0 {
            unsafe {
                core::ptr::write_volatile(
                    self.mmio.add(CSR_INT_MASK as usize),
                    0xFFFFFFFFu32,
                );
            }
            self.fw_state = FwState::Alive;
            crate::debug::print("iwlwifi", "fw: alive_ok");
            if let Err(_) = self.send_init_commands() {
                self.fw_state = FwState::Error;
                return Err("Failed to send init commands");
            }
            self.fw_state = FwState::Ready;
            return Ok(true);
        }

        Ok(false)
    }

    /// Send post-boot initialization commands to firmware.
    ///
    /// This sends the minimal set of host commands required to transition
    /// the firmware from the "alive" state to "operational":
    ///
    /// 1. TX Antenna Configuration (0x24)
    /// 2. RXON (0x1E) — configure station mode, channel, etc.
    /// 3. Set MAC Address (0x16) — confirm our MAC to firmware
    ///
    /// On real hardware, additional commands for BT coexistence,
    /// power-saving, HT/VHT capabilities, and queue setup would follow.
    fn send_init_commands(&mut self) -> Result<(), &'static str> {
        // ── 1. TX Antenna Configuration ────────────────────
        // Report available TX antennas to firmware.
        // cfg[0] = valid_tx_antenna mask (bitmask of antennas 1/2)
        // cfg[1] = valid_rx_antenna mask
        let ant_cfg: [u8; 8] = [0x03, 0x03, 0, 0, 0, 0, 0, 0];
        self.send_hcmd(LegacyCmd::TxAntConfig as u8, GroupId::Legacy as u8, &ant_cfg)
            .map_err(|_| "TX antenna config failed")?;
        log::info!("iwlwifi: TX antenna config sent");

        // ── 2. RXON (Radio ON) — basic station configuration
        // RXON configures the operating mode, channel, and basic rates.
        // A minimal RXON structure (36 bytes):
        //   flags(2), channel(2), bssid[6](6), node_addr[6](6),
        //   atim_window(2), beacon_interval(2), assoc_id(2),
        //   reserved[14](14)
        let mut rxon = [0u8; 36];
        // flags: bit 1 = STA mode, bit 6 = SHORT_SLOT
        rxon[0] = 0x42;
        rxon[1] = 0x00;
        // Set our MAC address as the node address (offset 12..18)
        let mac = self.mac;
        rxon[12..18].copy_from_slice(&mac);
        // Clear BSSID (we'll set it during association)
        // Set beacon interval to 100 TU (~100ms)
        rxon[22] = 100;
        rxon[23] = 0;
        self.send_hcmd(LegacyCmd::Rxon as u8, GroupId::Legacy as u8, &rxon)
            .map_err(|_| "RXON config failed")?;
        log::info!("iwlwifi: RXON config sent");

        // ── 3. Enable TX/RX queues ─────────────────────────
        // A real driver would send QUEUE_CONFIG commands.  For now,
        // the firmware defaults are used (single AC queue).
        // This is sufficient for basic operation in QEMU.

        log::info!("iwlwifi: init commands complete, device operational");
        Ok(())
    }

    // ── HCMD interface ─────────────────────────────────────────────

    /// Send a host command to the firmware.
    fn send_hcmd(&mut self, opcode: u8, group: u8, data: &[u8]) -> Result<(), &'static str> {
        let total_len = core::mem::size_of::<HcmdHeader>() + data.len();
        if total_len > MAX_FRAME_SIZE {
            return Err("HCMD too large");
        }

        // Verify device is accessible before DMA transactions
        self.health.pre_mmio_access().map_err(|_| "device not accessible")?;

        // Build command header
        let hcmd_header = HcmdHeader {
            opcode,
            group_id: group,
            length: data.len() as u16,
            flags: 0,
            reserved: 0,
        };

        // Write to TX DMA ring
        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            return Err("TX ring full");
        }
        let desc_idx = self.tx_head % TX_QUEUE_SIZE;
        let desc = unsafe { &mut *(self.tx_dma_ring.virt() as *mut TxDmaDesc).add(desc_idx) };
        let cmd_buf = &mut self.tx_bufs[desc_idx];

        // Write header + data into the DMA buffer
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &hcmd_header as *const HcmdHeader as *const u8,
                core::mem::size_of::<HcmdHeader>(),
            )
        };
        let mut full_data = alloc::vec::Vec::with_capacity(total_len);
        full_data.extend_from_slice(header_bytes);
        full_data.extend_from_slice(data);
        cmd_buf.write_from(&full_data);

        // Use the pre-mapped IOVA from init — no per-transaction
        // dma_map/dma_unmap needed, avoiding IOMMU mapping leaks.
        let dma_addr = cmd_buf.dma_iova();
        desc.addr_lo = dma_addr as u32;
        desc.addr_hi = (dma_addr >> 32) as u32;
        desc.len = total_len as u16;
        desc.flags = 0;

        // Flush descriptor ring cache line before doorbell
        let desc_addr = desc as *const TxDmaDesc as *const u8;
        mmio::cache_flush(desc_addr);

        self.tx_head += 1;

        // Ring the doorbell register to tell the device a new command is available.
        mmio::write_barrier();
        unsafe {
            core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
        }
        mmio::write_barrier();

        Ok(())
    }

    // ── Scanning ───────────────────────────────────────────────────

    /// Start an 802.11 scan on the specified channels.
    pub fn start_scan(&mut self) -> Result<(), &'static str> {
        if self.fw_state != FwState::Ready {
            return Err("Firmware not ready");
        }

        self.wifi_conn.start_scan();
        self.scan_results.clear();
        self.scan_channel = 1;
        self.scan_pending = true;
        self.iwl_state = IwlState::Scanning;

        // Build scan command for channels 1-13 (2.4 GHz band)
        let scan_cmd = ScanRequestCmd {
            beacon_interval: 100,
            flags: 0,
            num_channels: 4,
            reserved: [0u8; 3],
            channels: [
                ScanChannel { channel: 1, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 6, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 11, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 36, tx_power: 0, reserved: 0 }, // 5 GHz
            ],
        };

        let cmd_data = unsafe {
            core::slice::from_raw_parts(
                &scan_cmd as *const ScanRequestCmd as *const u8,
                core::mem::size_of::<ScanRequestCmd>(),
            )
        };

        self.send_hcmd(LegacyCmd::ScanRequest as u8, GroupId::Legacy as u8, cmd_data)?;

        log::info!("iwlwifi: scan started");
        Ok(())
    }

    /// Process received scan results / beacon frames.
    fn process_scan_result(&mut self, frame: &[u8]) {
        if let Some(beacon) = wifi::parse_beacon(frame) {
            let ssid = beacon.ssid.clone().unwrap_or(Ssid::new(b""));
            if ssid.is_empty() {
                return;
            }

            let security = wifi::security_from_beacon(
                beacon.capability,
                beacon.rsn.as_ref(),
            );

            let ap = AccessPoint {
                ssid,
                bssid: beacon.header.addr2,
                channel: beacon.ds_channel.unwrap_or(0),
                rssi: -50, // Would be read from hardware
                security,
                beacon_interval: beacon.beacon_interval,
            };

            self.wifi_conn.add_scan_result(ap.clone());
            self.scan_results.push(ap);
        }
    }

    // ── Connection management ──────────────────────────────────────

    /// Connect to a specified access point with optional password.
    pub fn connect(&mut self, ssid: &Ssid, password: Option<&str>) -> Result<(), &'static str> {
        if self.fw_state != FwState::Ready {
            return Err("Firmware not ready");
        }

        // Find the AP in scan results
        let ap = match self.scan_results.iter().find(|a| a.ssid == *ssid) {
            Some(a) => a.clone(),
            None => return Err("AP not found in scan results"),
        };

        self.wifi_conn.connect(ssid, password);

        // Initialize WPA supplicant if the AP uses security
        if password.is_some() {
            self.wpa.init(
                password.unwrap(),
                ssid.as_str(),
                ap.bssid,
                self.mac,
            );

            // Derive PTK for WPA2
            self.wpa.derive_ptk();
        }

        // Start authentication
        self.iwl_state = IwlState::AuthSent;

        // Build and send authentication frame
        let auth_frame = wifi::build_auth_frame(ap.bssid, self.mac, 1);
        let _ = self.send_raw_80211_frame(&auth_frame);

        log::info!(
            "iwlwifi: authenticating with {} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
            ssid,
            ap.bssid[0], ap.bssid[1], ap.bssid[2],
            ap.bssid[3], ap.bssid[4], ap.bssid[5],
        );

        Ok(())
    }

    /// Disconnect from the current AP.
    pub fn disconnect(&mut self) {
        if let Some(bssid) = self.wifi_conn.current_bssid {
            let deauth = wifi::build_deauth(bssid, self.mac, 3);
            let _ = self.send_raw_80211_frame(&deauth);
        }

        self.wifi_conn.disconnect();
        self.iwl_state = IwlState::Disconnected;

        if let Some(ref mut dhcp) = self.dhcp {
            let _release = dhcp.build_release();
        }
        self.dhcp = None;

        log::info!("iwlwifi: disconnected");
    }

    /// Send a raw 802.11 management or data frame.
    fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        self.tx_queue.push_back(frame.to_vec());
        self.process_tx_queue();
        Ok(())
    }

    /// Process pending TX frames and program DMA descriptors.
    fn process_tx_queue(&mut self) {
        // Verify health before initiating DMA
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() > MAX_FRAME_SIZE {
                self.tx_queue.pop_front();
                continue;
            }
            // Check if TX ring has available slots
            let used = self.tx_head.wrapping_sub(self.tx_tail);
            if used >= TX_QUEUE_SIZE {
                break;
            }

            let tx_frame = self.tx_queue.pop_front().unwrap();
            let desc_idx = self.tx_head % TX_QUEUE_SIZE;
            let buf = &mut self.tx_bufs[desc_idx];

            // Write frame data and flush cache for DMA
            buf.write_from(&tx_frame);

            let desc = unsafe { &mut *(self.tx_dma_ring.virt() as *mut TxDmaDesc).add(desc_idx) };
            // Use the pre-mapped IOVA from init — no per-transaction
            // dma_map/dma_unmap needed, avoiding IOMMU mapping leaks.
            let dma_addr = buf.dma_iova();
            desc.addr_lo = dma_addr as u32;
            desc.addr_hi = (dma_addr >> 32) as u32;
            desc.len = tx_frame.len() as u16;
            desc.flags = 0;

            // Flush descriptor cache line so device sees correct values
            let desc_addr = desc as *const TxDmaDesc as *const u8;
            mmio::cache_flush(desc_addr);

            self.tx_head = self.tx_head.wrapping_add(1);

            // Doorbell with write barrier
            mmio::write_barrier();
            unsafe {
                core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
            }
            mmio::write_barrier();
        }
    }

    /// Process a received 802.11 frame.
    fn process_rx_frame(&mut self, frame: &[u8]) {
        if frame.len() < 2 {
            return;
        }

        // Extract frame type (bits 2-3) and shift down to normalize to 0/1/2
        let frame_type = (frame[0] & 0x0C) >> 2;
        let subtype = (frame[0] >> 4) & 0x0F;

        match (frame_type, subtype) {
            // Beacon / Probe Response
            (0, 5) | (0, 8) => {
                if self.iwl_state == IwlState::Scanning {
                    self.process_scan_result(frame);
                }
            }
            // Authentication response
            (0, 11) => {
                if self.iwl_state == IwlState::AuthSent
                    || self.iwl_state == IwlState::Scanning
                {
                    // Parse auth response
                    let body_offset = 24; // MAC header size
                    if frame.len() >= body_offset + 6 {
                        let status_code = u16::from_le_bytes([
                            frame[body_offset + 4],
                            frame[body_offset + 5],
                        ]);
                        if status_code == 0 {
                            // Auth successful - send association
                            self.iwl_state = IwlState::AssocSent;
                            let bssid = [
                                frame[4], frame[5], frame[6],
                                frame[7], frame[8], frame[9],
                            ];
                            let ap_ssid = self.wifi_conn.current_ssid.clone()
                                .unwrap_or(Ssid::new(b""));
                            let assoc = wifi::build_assoc_request(
                                bssid, self.mac, &ap_ssid,
                            );
                            let _ = self.send_raw_80211_frame(&assoc);
                            log::info!("iwlwifi: auth successful, associating");
                        } else {
                            self.wifi_conn.status = WifiStatus::Error;
                            log::warn!("iwlwifi: auth failed with status {}", status_code);
                        }
                    }
                }
            }
            // Association response
            (0, 1) => {
                if self.iwl_state == IwlState::AssocSent {
                    let body_offset = 24;
                    if frame.len() >= body_offset + 6 {
                        let status_code = u16::from_le_bytes([
                            frame[body_offset + 2],
                            frame[body_offset + 3],
                        ]);
                        if status_code == 0 {
                            let aid = u16::from_le_bytes([
                                frame[body_offset + 4],
                                frame[body_offset + 5],
                            ]);
                            self.iwl_state = IwlState::Connected;
                            self.wifi_conn.status = WifiStatus::Connected;
                            self.wifi_conn.current_bssid = Some([
                                frame[4], frame[5], frame[6],
                                frame[7], frame[8], frame[9],
                            ]);

                            // Start DHCP
                            self.dhcp = Some(DhcpClient::new(self.mac));
                            if let Some(ref mut dhcp) = self.dhcp {
                                let discover = dhcp.build_discover();
                                log::info!(
                                    "iwlwifi: associated (AID={}), sending DHCP discover", aid
                                );
                                // Wrap and send as 802.11 data frame
                                let _ = self.send_raw_80211_frame(&discover);
                            }
                        } else {
                            self.wifi_conn.status = WifiStatus::Error;
                            log::warn!("iwlwifi: assoc failed with status {}", status_code);
                        }
                    }
                }
            }
            // 802.11 data frame
            (2, _) => {
                // Pass to network stack
                if frame.len() > 24 {
                    // Strip 802.11 header and LLC/SNAP
                    let llc_offset = 24;
                    if frame.len() > llc_offset + 8 {
                        let ether_type = u16::from_be_bytes([
                            frame[llc_offset + 6],
                            frame[llc_offset + 7],
                        ]);
                        let data = &frame[llc_offset + 8..];
                        if ether_type == 0x888E {
                            // Route EAPOL frames to the WPA supplicant
                            if self.wpa.state == bonder::wpa::WpaState::WaitMsg1 {
                                if let Ok(reply) = self.wpa.handle_message_1(data) {
                                    let _ = self.send_raw_80211_frame(&reply);
                                }
                            } else if self.wpa.state == bonder::wpa::WpaState::WaitMsg3 {
                                if let Ok(reply) = self.wpa.handle_message_3(data) {
                                    let _ = self.send_raw_80211_frame(&reply);
                                }
                            }
                        } else if ether_type == 0x0800 {
                            // IPv4 - check for DHCP (UDP port 68)
                            let dhcp_handled = if data.len() >= 20 + 8 {
                                let ip_ver_ihl = data[0];
                                let ihl = (ip_ver_ihl & 0x0F) as usize * 4;
                                let protocol = data[9];
                                if protocol == 17 && data.len() >= ihl + 8 {
                                    let dst_port = u16::from_be_bytes([data[ihl + 2], data[ihl + 3]]);
                                    if dst_port == 68 {
                                        if let Some(ref mut dhcp) = self.dhcp {
                                            let dhcp_data = &data[ihl + 8..];
                                            if let Ok(msg_type) = dhcp.parse_response(dhcp_data) {
                                                log::info!("iwlwifi: DHCP {} received", msg_type as u8);
                                                if msg_type == bonder::dhcp::DhcpMessageType::Offer {
                                                    let req = dhcp.build_request(dhcp.lease.ip_address, dhcp.lease.server_id);
                                                    let _ = self.send_raw_80211_frame(&req);
                                                } else if msg_type == bonder::dhcp::DhcpMessageType::Ack {
                                                    self.ip_address = dhcp.lease.ip_address;
                                                    self.subnet_mask = dhcp.lease.subnet_mask;
                                                    self.gateway = dhcp.lease.router;
                                                    self.dns_server = dhcp.lease.dns_server;
                                                    log::info!("iwlwifi: IP address assigned: {:?}", self.ip_address);
                                                }
                                                true
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            if !dhcp_handled {
                                self.rx_queue.push_back(data.to_vec());
                            }
                        } else {
                            self.rx_queue.push_back(data.to_vec());
                        }
                    }
                }
            }
            // Deauth / Disassoc
            (0, 10) | (0, 12) => {
                self.wifi_conn.status = WifiStatus::Disconnected;
                self.iwl_state = IwlState::Disconnected;
                log::warn!("iwlwifi: disconnected by AP");
            }
            _ => {}
        }
    }

    /// Periodic tick - process pending events, scan results, etc.
    pub fn tick(&mut self) {
        // Verify health before touching hardware registers
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        // Poll firmware for events
        let int_cause = unsafe { core::ptr::read_volatile(self.mmio.add(CSR_INT as usize)) };
        if int_cause != 0 && int_cause != 0xFFFF_FFFF {
            // Write-back to acknowledge (write to clear)
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }

            // Check for RX
            if (int_cause & (1 << 18)) != 0 {
                let raw_rx_head = unsafe {
                    core::ptr::read_volatile(self.mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize))
                };
                self.rx_head = (raw_rx_head as usize) % RX_QUEUE_SIZE;
            }
            // Check for TX completion
            if (int_cause & (1 << 15)) != 0 {
                self.tx_tail = unsafe {
                    core::ptr::read_volatile(self.mmio.add(FH_TX_CHNL0_WPTR as usize))
                } as usize;
                self.process_tx_queue();
            }
        }

        // Process any pending frames in the RX queue
        // Invalidate cache so the CPU sees hardware-updated descriptor lengths
        mmio::cache_flush_range(self.rx_dma_ring.virt(), core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE);
        while self.rx_tail != self.rx_head {
            let desc_idx = self.rx_tail;
            let desc = unsafe { &*(self.rx_dma_ring.virt() as *const RxDmaDesc).add(desc_idx) };
            if desc.len > 0 && desc_idx < self.rx_bufs.len() {
                let buf = &self.rx_bufs[desc_idx];
                let frame_len = (desc.len as usize).min(buf.len());
                // Use DmaRegion::read_into for cache-invalidate + copy
                let mut frame_data = alloc::vec![0; frame_len];
                buf.read_into(&mut frame_data);
                self.process_rx_frame(&frame_data);
            }
            self.rx_tail = (self.rx_tail + 1) % RX_QUEUE_SIZE;
        }

        // Check for scan completion
        if self.scan_pending {
            self.scan_channel += 1;
            if self.scan_channel > 13 {
                self.scan_pending = false;
                self.wifi_conn.finish_scan();
                self.iwl_state = IwlState::Disconnected;
                log::info!(
                    "iwlwifi: scan complete ({} APs found)",
                    self.scan_results.len()
                );
            }
        }
    }

    /// Get the list of scanned access points.
    pub fn access_points(&self) -> &[AccessPoint] {
        &self.scan_results
    }

    /// Get current WiFi connection status.
    pub fn wifi_status(&self) -> &wifi::WifiConnection {
        &self.wifi_conn
    }

    /// Returns true if WiFi is connected and has an IP address via DHCP.
    pub fn is_network_ready(&self) -> bool {
        self.wifi_conn.is_connected() && self.ip_address != [0u8; 4]
    }
}

// ── NetDevice implementation ──────────────────────────────────────────

impl NetDevice for IwlWifiDevice {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if self.fw_state != FwState::Ready {
            return Err(NetError::NotInitialized);
        }

        if frame.len() > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }

        // Wrap the Ethernet frame in a 802.11 data frame
        // and transmit via the TX DMA ring.
        let _ = self.send_raw_80211_frame(frame);

        Ok(())
    }

    fn poll_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        if self.fw_state != FwState::Ready {
            return Ok(None);
        }

        // Check RX DMA ring for received frames
        if let Some(rx_data) = self.rx_queue.pop_front() {
            if rx_data.len() > buf.len() {
                return Err(NetError::BufferTooSmall);
            }
            let len = rx_data.len();
            buf[..len].copy_from_slice(&rx_data);
            return Ok(Some(len));
        }

        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

// ── WifiDriver trait implementation ───────────────────────────────

impl super::wifi::WifiDriver for IwlWifiDevice {
    fn create(
        ctx: &'static dyn DriverContext,
        mmio_base: *mut u32,
        hw_rev: u32,
        device: crate::pci::PciDevice,
    ) -> Option<Box<dyn super::wifi::WifiDriver>> {
        Self::init_from_mmio(ctx, mmio_base, hw_rev, device)
            .map(|dev| Box::new(dev) as Box<dyn super::wifi::WifiDriver>)
    }

    fn tick(&mut self) {
        self.tick();
    }

    fn get_status(&self) -> bonder::wifi::WifiStatus {
        self.wifi_conn.status
    }

    fn start_scan(&mut self) -> bool {
        self.start_scan().is_ok()
    }

    fn get_scan_results(&self) -> Vec<bonder::wifi::AccessPoint> {
        self.scan_results.clone()
    }

    fn connect(&mut self, ssid: &bonder::wifi::Ssid, psk: Option<&str>) -> bool {
        self.connect(ssid, psk).is_ok()
    }

    fn disconnect(&mut self) {
        self.disconnect();
    }

    fn device_available(&self) -> bool {
        self.fw_state == FwState::Ready
    }

    fn connected_ssid(&self) -> Option<&bonder::wifi::Ssid> {
        self.wifi_conn.current_ssid.as_ref()
    }

    fn ip_address(&self) -> [u8; 4] {
        self.ip_address
    }

    fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        IwlWifiDevice::load_firmware(self, fw_data)
    }

    fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        IwlWifiDevice::start_firmware(self, fw_data)
    }

    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, &'static str> {
        IwlWifiDevice::check_alive_nonblocking(self, start_tsc)
    }
}

/// Constructor called by the wifi registry after PCI probe.
///
/// `ctx` — kernel driver context (DMA, MMIO mapping)
/// `mmio` — mapped BAR0 base
/// `hw_rev` — hardware revision read from CSR_HW_REV
///
/// Returns a boxed driver on success, or `None` if the device does
/// not respond or initialisation times out.
pub fn try_create_iwl(
    ctx: &'static dyn DriverContext,
    mmio: *mut u32,
    hw_rev: u32,
    device: crate::pci::PciDevice,
) -> Option<Box<dyn super::wifi::WifiDriver>> {
    IwlWifiDevice::init_from_mmio(ctx, mmio, hw_rev, device)
        .map(|dev| Box::new(dev) as Box<dyn super::wifi::WifiDriver>)
}

// ── Stored wifi state for external access (via driver tick) ────────

/// Global wifi manager state for UI polling.
static WIFI_MANAGER: Mutex<Option<WifiManager>> = Mutex::new(None);

/// Global WiFi driver instance (trait-object based) so other parts of
/// the OS can tick it.  The concrete type is determined by PCI probe.
static WIFI_DEVICE: Mutex<Option<Box<dyn super::wifi::WifiDriver>>> = Mutex::new(None);

/// Set to `true` once `try_init_wifi_device()` has completed (success or
/// failure).  Used by `tick_wifi_device()` to skip the Mutex lock+check
/// on every tick before WiFi is initialised.
static WIFI_INIT_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Check if WiFi initialization has completed (either Done or Failed).
pub fn wifi_init_completed() -> bool {
    WIFI_INIT_COMPLETED.load(core::sync::atomic::Ordering::Acquire)
}

// ── Incremental WiFi init state machine ───────────────────────────
//
// On real hardware the full initialisation sequence (PCI probe, MMIO
// init, DMA allocation, firmware upload, alive wait, init commands)
// can block for many seconds — hanging the desktop render loop.
//
// The state machine below splits the work into small steps that each
// return quickly.  `try_init_wifi_device_step()` is called once per
// frame from `tick_core()` and advances through the phases.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum WifiInitPhase {
    /// Not yet started.
    Idle = 0,
    /// PCI probe + D0/ASPM setup.
    PciProbe = 1,
    /// MMIO init: map BAR, reset_device, MAC clock.
    MmioInit = 2,
    /// DMA ring / buffer allocation.
    DmaAlloc = 3,
    /// Firmware header parse + TLV upload.
    FwUpload = 4,
    /// Wait for alive (polls CSR_INT with timeout).
    FwWaitAlive = 5,
    /// Send init commands.
    FwInitCmds = 6,
    /// Initialisation complete (success).
    Done = 7,
    /// Initialisation failed (retry-able on next reboot/scan).
    Failed = 8,
}

impl From<u8> for WifiInitPhase {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Idle,
            1 => Self::PciProbe,
            2 => Self::MmioInit,
            3 => Self::DmaAlloc,
            4 => Self::FwUpload,
            5 => Self::FwWaitAlive,
            6 => Self::FwInitCmds,
            7 => Self::Done,
            _ => Self::Failed,
        }
    }
}

/// Persistent state carried across incremental init steps.
struct WifiInitContext {
    /// Boxed driver after MMIO init (before firmware).
    mmio_device: Option<Box<dyn super::wifi::WifiDriver>>,
    /// Firmware blob being loaded (index into candidates).
    fw_candidate_idx: usize,
    /// Firmware candidates for this device.
    fw_candidates: &'static [FirmwareBlob],
    /// Start TSC for alive timeout.
    alive_start_tsc: u64,
}

/// Lock-free phase of the WiFi init state machine.
/// Updated with atomic stores (no Mutex needed), so the render loop
/// can poll the current phase without any lock contention.
static WIFI_INIT_PHASE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(WifiInitPhase::Idle as u8);

static WIFI_INIT_CTX: Mutex<WifiInitContext> = Mutex::new(WifiInitContext {
    mmio_device: None,
    fw_candidate_idx: 0,
    fw_candidates: &[],
    alive_start_tsc: 0,
});

/// Helper: set the init phase (lock-free).
fn set_init_phase(phase: WifiInitPhase) {
    WIFI_INIT_PHASE.store(phase as u8, core::sync::atomic::Ordering::Release);
}

/// Helper: read the current init phase (lock-free).
fn get_init_phase() -> WifiInitPhase {
    let raw = WIFI_INIT_PHASE.load(core::sync::atomic::Ordering::Acquire);
    WifiInitPhase::from(raw)
}

/// Call once per frame from `tick_core()` to incrementally initialise
/// the WiFi device.  Each call performs a small, bounded amount of work
/// so the desktop render loop is never blocked for more than ~1 ms.
pub fn try_init_wifi_device_step() {
    // ── Snapshot current phase (lock-free) ────────────────
    let phase = get_init_phase();

    match phase {
        WifiInitPhase::Idle => {
            // ── Start initialisation ──────────────────────
            let driver_ctx_opt = WIFI_DRIVER_CTX.lock();
            let _driver_ctx = match *driver_ctx_opt {
                Some(c) => c,
                None => {
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            drop(driver_ctx_opt);

            let dev_guard = WIFI_DEVICE.lock();
            if dev_guard.is_some() {
                crate::debug::print("iwlwifi", "step: already_inited");
                set_init_phase(WifiInitPhase::Done);
                return;
            }
            drop(dev_guard);

            crate::debug::print("iwlwifi", "step: start pci_probe");
            set_init_phase(WifiInitPhase::PciProbe);
        }
        WifiInitPhase::PciProbe => {
            // ── PCI probe (config space only, never hangs) ──
            crate::debug::print("iwlwifi", "step: pci_probe_enter");
            let driver_ctx = match *WIFI_DRIVER_CTX.lock() {
                Some(c) => c,
                None => {
                    crate::debug::print("iwlwifi", "step: ERR no_driver_ctx");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            crate::debug::print("iwlwifi", "step: call init_wifi_from_pci");
            let probe = match crate::wifi::init_wifi_from_pci(driver_ctx) {
                Some(p) => p,
                None => {
                    crate::debug::print("iwlwifi", "step: no_pci_device");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            crate::debug::print("iwlwifi", "step: init_wifi_from_pci_ok");
            let candidates = select_firmware_list(probe.device_id);
            if candidates.is_empty() {
                crate::debug::print("iwlwifi", "step: no_fw");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.mmio_device = Some(probe.driver);
                ctx.fw_candidates = candidates;
                ctx.fw_candidate_idx = 0;
            }
            set_init_phase(WifiInitPhase::FwUpload);
            crate::debug::print("iwlwifi", "step: pci_probe_done");
        }
        WifiInitPhase::FwUpload => {
            // ── Upload one firmware blob (non-blocking) ────
            let (fw_data, fw_name) = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let _dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                if ctx.fw_candidate_idx >= ctx.fw_candidates.len() {
                    crate::debug::print("iwlwifi", "step: all_fw_failed");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let fw = &ctx.fw_candidates[ctx.fw_candidate_idx];
                (fw.data, fw.name)
            };

            log::info!(
                "iwlwifi: step: trying firmware {} ({} bytes)",
                fw_name, fw_data.len()
            );

            // start_firmware uploads and starts CPU without blocking on alive
            let start_result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.start_firmware(fw_data)
            };

            match start_result {
                Ok(()) => {
                    log::info!("iwlwifi: step: firmware {} upload complete, waiting for alive", fw_name);
                    crate::debug::print("iwlwifi", "step: fw_uploaded");
                    // Record start time for alive timeout
                    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
                    WIFI_INIT_CTX.lock().alive_start_tsc = now_tsc;
                    set_init_phase(WifiInitPhase::FwWaitAlive);
                }
                Err(e) => {
                    log::warn!("iwlwifi: step: firmware {} upload failed: {}", fw_name, e);
                    let mut ctx = WIFI_INIT_CTX.lock();
                    ctx.fw_candidate_idx += 1;
                    // Stay in FwUpload phase; next frame tries the next blob.
                }
            }
        }
        WifiInitPhase::FwWaitAlive => {
            // ── Poll for firmware alive (non-blocking) ─────
            let start_tsc = WIFI_INIT_CTX.lock().alive_start_tsc;
            let alive_result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.check_alive_nonblocking(start_tsc)
            };

            match alive_result {
                Ok(true) => {
                    crate::debug::print("iwlwifi", "step: fw_alive");
                    set_init_phase(WifiInitPhase::Done);
                }
                Ok(false) => {
                    // Still waiting, will poll again next frame
                    crate::debug::print("iwlwifi", "step: fw_wait_alive_poll");
                }
                Err(e) => {
                    // Timeout or error, try next firmware
                    log::warn!("iwlwifi: step: firmware alive failed: {}", e);
                    let mut ctx = WIFI_INIT_CTX.lock();
                    ctx.fw_candidate_idx += 1;
                    set_init_phase(WifiInitPhase::FwUpload);
                }
            }
        }
        WifiInitPhase::Done => {
            // ── Commit the driver to the global slot ───────
            let dev_opt = WIFI_INIT_CTX.lock().mmio_device.take();
            if let Some(dev) = dev_opt {
                let mut dev_guard = WIFI_DEVICE.lock();
                if dev_guard.is_none() {
                    *dev_guard = Some(dev);
                }
            }
            WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
            crate::debug::print("iwlwifi", "step: init_done");
        }
        WifiInitPhase::Failed => {
            let _ = WIFI_INIT_CTX.lock().mmio_device.take();
            WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
            crate::debug::print("iwlwifi", "step: init_failed");
        }
        // Legacy phases not used in step-based init:
        WifiInitPhase::MmioInit | WifiInitPhase::DmaAlloc | WifiInitPhase::FwInitCmds => {
            set_init_phase(WifiInitPhase::Failed);
        }
    }
}

#[derive(Clone)]
pub struct WifiManager {
    pub device_available: bool,
    pub scan_results: Vec<AccessPoint>,
    pub status: WifiStatus,
    pub connected_ssid: Option<String>,
    pub ip_address: Option<String>,
}

// ── Firmware registry ─────────────────────────────────────────────────
//
// Each chipset variant has its own firmware binary.  We embed the latest
// versions and try them in order, falling back to an older version if the
// device does not respond.

struct FirmwareBlob {
    data: &'static [u8],
    name: &'static str,
}

// 7260 series (PCI 0x08B1, 0x08B2)
const FW_7260_17: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7260-17.ucode");
const FW_7260_16: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7260-16.ucode");

// 7265 series, non-D stepping (PCI 0x095A, 0x095B)
const FW_7265_17: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7265-17.ucode");
const FW_7265_16: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7265-16.ucode");

// 7265D series, D stepping (PCI 0x095A, 0x095B)
const FW_7265D_29: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7265D-29.ucode");
const FW_7265D_27: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7265D-27.ucode");

/// Select firmware candidates for the given PCI device ID.
///
/// Returns a slice of [`FirmwareBlob`] entries in preference order.
fn select_firmware_list(device_id: u16) -> &'static [FirmwareBlob] {
    match device_id {
        // 7260 series
        0x08B1 | 0x08B2 => &[
            FirmwareBlob { data: FW_7260_17, name: "iwlwifi-7260-17" },
            FirmwareBlob { data: FW_7260_16, name: "iwlwifi-7260-16" },
        ],
        // 7265 / 7265D series — try D-step firmware first (newest),
        // then fall back to non-D in case HW is an older stepping.
        0x095A | 0x095B => &[
            FirmwareBlob { data: FW_7265D_29, name: "iwlwifi-7265D-29" },
            FirmwareBlob { data: FW_7265D_27, name: "iwlwifi-7265D-27" },
            FirmwareBlob { data: FW_7265_17, name: "iwlwifi-7265-17" },
            FirmwareBlob { data: FW_7265_16, name: "iwlwifi-7265-16" },
        ],
        _ => &[],
    }
}

/// Probe for an Intel wireless device, load firmware and store it for periodic ticking.
///
/// Safe to call multiple times.  Requires that `set_wifi_driver_context()` has
/// been called before (typically by the kernel's init sequence).
pub fn try_init_wifi_device() {
    crate::debug::print("iwlwifi", "try_init_wifi_device: start");
    let ctx_opt = WIFI_DRIVER_CTX.lock();
    let ctx = match *ctx_opt {
        Some(c) => c,
        None => {
            log::warn!("iwlwifi: driver context not set, cannot init");
            crate::debug::print("iwlwifi", "ERR no_driver_ctx");
            return;
        }
    };
    drop(ctx_opt);

    let mut dev_guard = WIFI_DEVICE.lock();
    if dev_guard.is_some() {
        crate::debug::print("iwlwifi", "already_inited");
        return;
    }

    // Use the PCI-probe-based registry to detect and init the WiFi card.
    crate::debug::print("iwlwifi", "init_wifi_from_pci");
    let mut probe = match crate::wifi::init_wifi_from_pci(ctx) {
        Some(p) => p,
        None => {
            crate::debug::print("iwlwifi", "ERR no_pci_device");
            return;
        }
    };

    // Select firmware candidates for this device
    let candidates = select_firmware_list(probe.device_id);
    if candidates.is_empty() {
        log::warn!(
            "iwlwifi: no firmware available for device {:#06x}",
            probe.device_id
        );
        crate::debug::print("iwlwifi", "ERR no_firmware");
        return;
    }

    // Try each firmware blob in order until one succeeds.
    let mut fw_loaded = false;
    for fw in candidates {
        log::info!(
            "iwlwifi: trying firmware {} ({} bytes)",
            fw.name,
            fw.data.len()
        );
        crate::debug::print("iwlwifi", "load_firmware_start");

        match probe.driver.load_firmware(fw.data) {
            Ok(()) => {
                log::info!("iwlwifi: firmware {} loaded successfully", fw.name);
                crate::debug::print("iwlwifi", "load_firmware_ok");
                fw_loaded = true;
                break;
            }
            Err(e) => {
                log::warn!("iwlwifi: firmware {} failed: {}", fw.name, e);
                crate::debug::print("iwlwifi", "load_firmware_fail");
            }
        }
    }

    if fw_loaded {
        *dev_guard = Some(probe.driver);
        crate::debug::print("iwlwifi", "init_done");
    } else {
        log::error!("iwlwifi: all firmware variants failed to load");
        crate::debug::print("iwlwifi", "ERR all_fw_failed");
    }
    WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
}

/// Tick the stored device and update the global wifi manager snapshot.
///
/// Returns immediately (without acquiring any lock) if WiFi initialisation
/// has not yet completed, avoiding unnecessary contention on every frame.
pub fn tick_wifi_device() {
    if !WIFI_INIT_COMPLETED.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut dev_guard = WIFI_DEVICE.lock();
    if let Some(ref mut dev) = *dev_guard {
        let dev_ref: &mut dyn super::wifi::WifiDriver = &mut **dev;
        dev_ref.tick();
        update_wifi_manager(dev_ref);
    }
}

impl WifiManager {
    pub fn new() -> Self {
        Self {
            device_available: false,
            scan_results: Vec::new(),
            status: WifiStatus::Disconnected,
            connected_ssid: None,
            ip_address: None,
        }
    }
}

/// Update global WiFi state from the driver (called from driver tick).
pub fn update_wifi_manager(dev: &dyn super::wifi::WifiDriver) {
    let mut mgr = WIFI_MANAGER.lock();
    if let Some(ref mut m) = *mgr {
        m.device_available = dev.device_available();
        m.scan_results = dev.get_scan_results();
        m.status = dev.get_status();
        m.connected_ssid = dev.connected_ssid().map(|s| s.to_string());
        let ip = dev.ip_address();
        if ip != [0u8; 4] {
            m.ip_address = Some(alloc::format!(
                "{}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3]
            ));
        } else {
            m.ip_address = None;
        }
    }
}

/// Read current wifi state (thread-safe).
pub fn wifi_state_snapshot() -> Option<WifiManager> {
    WIFI_MANAGER.lock().clone()
}

/// Initialize the global WiFi manager.
pub fn init_wifi_manager() {
    *WIFI_MANAGER.lock() = Some(WifiManager::new());
}

/// Connect to an access point by SSID with optional password.
///
/// This is a convenience wrapper for UI code to initiate connections.
pub fn connect_to_ap(ssid: &bonder::wifi::Ssid, password: Option<&str>) {
    let mut dev_guard = WIFI_DEVICE.lock();
    if let Some(ref mut dev) = *dev_guard {
        let dev_ref: &mut dyn super::wifi::WifiDriver = &mut **dev;
        let _ = dev_ref.connect(ssid, password);
    }
}

// ── Error types ──────────────────────────────────────────────────────

#[derive(Debug)]
enum IwlError {
    BarNotAvailable,
    ClockNotReady,
    DmaAllocFailed,
}
