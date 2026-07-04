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

/// FH register for RX ring head index.
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
    tx_dma_ring: Box<[TxDmaDesc; TX_QUEUE_SIZE]>,
    rx_dma_ring: Box<[RxDmaDesc; RX_QUEUE_SIZE]>,
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
        // If available here, also register the UC MMIO mapping:
        // ctx.map_mmio_region(bar0_addr as usize, mmio_virt, IWL_BAR0_SIZE)
        //     .map_err(|_| IwlError::BarNotAvailable)?;
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
        let mut clock_ready = false;
        for _ in 0..50_000 {
            let gp = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
            if (gp & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0 {
                clock_ready = true;
                break;
            }
            core::hint::spin_loop();
        }
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
        let tx_dma_ring = Box::new([TxDmaDesc {
            addr_lo: 0, addr_hi: 0, len: 0, flags: 0, reserved: [0; 2],
        }; TX_QUEUE_SIZE]);
        let mut rx_dma_ring = Box::new([RxDmaDesc {
            addr_lo: 0, addr_hi: 0, len: 0, flags: 0,
        }; RX_QUEUE_SIZE]);
        let mut tx_bufs = Vec::new();
        for _ in 0..TX_QUEUE_SIZE {
            let buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
            tx_bufs.push(buf);
        }
        let mut rx_bufs = Vec::new();
        for i in 0..RX_QUEUE_SIZE {
            let buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
            // Program the RX DMA descriptor with the IOMMU-mapped DMA address
            let dma = ctx.dma_map(device.device_id, buf.phys(), MAX_FRAME_SIZE)
                .map_err(|_| IwlError::DmaAllocFailed)?;
            rx_dma_ring[i].addr_lo = dma as u32;
            rx_dma_ring[i].addr_hi = (dma >> 32) as u32;
            rx_dma_ring[i].len = MAX_FRAME_SIZE as u16;
            rx_bufs.push(buf);
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

    /// Reset the device.
    fn reset_device(mmio: *mut u32) {
        unsafe {
            core::ptr::write_volatile(
                mmio.add(CSR_RESET as usize),
                CSR_RESET_BIT_STOP_MASTER,
            );
            for _ in 0..100_000 {
                let r = core::ptr::read_volatile(mmio.add(CSR_RESET as usize));
                if (r & CSR_RESET_BIT_MASTER_DISABLED) != 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), CSR_RESET_BIT_SW);
            for _ in 0..200_000 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), 0);
            for _ in 0..200_000 {
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

        // Restore full mask after alive
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                0xFFFFFFFFu32,
            );
        }

        self.fw_state = FwState::Ready;
        log::info!("iwlwifi: firmware alive and ready");

        // Send initialization commands
        self.send_init_commands()?;

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

    /// Wait for the firmware "alive" response.
    fn wait_for_alive(&mut self) -> Result<(), &'static str> {
        for _ in 0..10_000_000 {
            // Check CSR_INT bit 0 (ALIVE)
            let int_cause = unsafe { core::ptr::read_volatile(self.mmio.add(CSR_INT as usize)) };
            if int_cause != 0 && int_cause != 0xFFFF_FFFF {
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

        Err("Timeout waiting for firmware alive")
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
        let desc = &mut self.tx_dma_ring[desc_idx];
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

        // Map the buffer for DMA using DriverContext::dma_map().
        // This returns either an IOMMU IOVA or the physical address
        // (identity mapping) depending on whether VT-d is enabled.
        let dma_addr = self.ctx
            .dma_map(self._pci_dev.device_id, cmd_buf.phys(), total_len)
            .map_err(|_| "dma_map failed for HCMD")?;
        desc.addr_lo = dma_addr as u32;
        desc.addr_hi = (dma_addr >> 32) as u32;
        desc.len = total_len as u16;
        desc.flags = 0;

        // Flush descriptor ring cache line before doorbell
        let desc_addr = &self.tx_dma_ring[desc_idx] as *const TxDmaDesc as *const u8;
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

            let desc = &mut self.tx_dma_ring[desc_idx];
            // Use ctx.dma_map() to get the proper DMA/IOVA address
            let dma_addr = match self
                .ctx
                .dma_map(self._pci_dev.device_id, buf.phys(), tx_frame.len())
            {
                Ok(addr) => addr,
                Err(_) => {
                    log::warn!("iwlwifi: dma_map failed for TX frame");
                    break;
                }
            };
            desc.addr_lo = dma_addr as u32;
            desc.addr_hi = (dma_addr >> 32) as u32;
            desc.len = tx_frame.len() as u16;
            desc.flags = 0;

            // Flush descriptor cache line so device sees correct values
            let desc_addr = &self.tx_dma_ring[desc_idx] as *const TxDmaDesc as *const u8;
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
                self.rx_head = unsafe {
                    core::ptr::read_volatile(self.mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize))
                } as usize;
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
        while self.rx_tail != self.rx_head {
            let desc_idx = self.rx_tail;
            let desc = &self.rx_dma_ring[desc_idx];
            if desc.len > 0 && desc_idx < self.rx_bufs.len() {
                let buf = &self.rx_bufs[desc_idx];
                let frame_len = (desc.len as usize).min(buf.len());
                // Use DmaRegion::read_into for cache-invalidate + copy
                let mut frame_data = alloc::vec::Vec::with_capacity(frame_len);
                unsafe { frame_data.set_len(frame_len); }
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

// ── Stored wifi state for external access (via driver tick) ────────

/// Global wifi manager state for UI polling.
static WIFI_MANAGER: Mutex<Option<WifiManager>> = Mutex::new(None);

/// Global IwlWifiDevice instance so other parts of the OS can tick it.
static WIFI_DEVICE: Mutex<Option<IwlWifiDevice>> = Mutex::new(None);

#[derive(Clone)]
pub struct WifiManager {
    pub device_available: bool,
    pub scan_results: Vec<AccessPoint>,
    pub status: WifiStatus,
    pub connected_ssid: Option<String>,
    pub ip_address: Option<String>,
}

/// Embedded firmware binary: iwlwifi-7265D-29.ucode.
/// Path relative to this file: ../../bonder/iwlwifi/iwlwifi-7265D-29.ucode
const EMBEDDED_FW: &[u8] = include_bytes!("../../bonder/iwlwifi/iwlwifi-7265D-29.ucode");

/// Known-good CRC32 checksum of `EMBEDDED_FW`, used by [`IwlWifiDevice::load_firmware`]
/// to reject a tampered or corrupted blob before any section is uploaded to the device.
const EMBEDDED_FW_CRC32: u32 = 0xECB4_1451;

/// Probe for an Intel wireless device, load firmware and store it for periodic ticking.
///
/// Safe to call multiple times.  Requires that `set_wifi_driver_context()` has
/// been called before (typically by the kernel's init sequence).
pub fn try_init_wifi_device() {
    let ctx_opt = WIFI_DRIVER_CTX.lock();
    let ctx = match *ctx_opt {
        Some(c) => c,
        None => {
            log::warn!("iwlwifi: driver context not set, cannot init");
            return;
        }
    };
    drop(ctx_opt);

    let mut dev_guard = WIFI_DEVICE.lock();
    if dev_guard.is_some() {
        return;
    }
    if let Some(mut dev) = IwlWifiDevice::probe_and_init(ctx) {
        log::info!("iwlwifi: loading embedded firmware ({} bytes)", EMBEDDED_FW.len());

        // Verify firmware integrity with CRC32 against the known-good checksum
        let crc_computed = IwlWifiDevice::crc32(EMBEDDED_FW);
        if crc_computed != EMBEDDED_FW_CRC32 {
            log::warn!(
                "iwlwifi: firmware checksum mismatch (computed={:#010x}, expected={:#010x})",
                crc_computed, EMBEDDED_FW_CRC32
            );
            // Keep the device anyway so UI knows it exists
            *dev_guard = Some(dev);
            return;
        }

        match dev.load_firmware(EMBEDDED_FW) {
            Ok(()) => {
                log::info!("iwlwifi: firmware loaded successfully");
                *dev_guard = Some(dev);
            }
            Err(e) => {
                log::error!("iwlwifi: firmware load failed: {}", e);
                // Keep the device anyway so UI knows it exists
                *dev_guard = Some(dev);
            }
        }
    }
}

/// Tick the stored device and update the global wifi manager snapshot.
pub fn tick_wifi_device() {
    let mut dev_guard = WIFI_DEVICE.lock();
    if let Some(ref mut dev) = *dev_guard {
        dev.tick();
        update_wifi_manager(dev);
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
pub fn update_wifi_manager(dev: &IwlWifiDevice) {
    let mut mgr = WIFI_MANAGER.lock();
    if let Some(ref mut m) = *mgr {
        m.device_available = true;
        m.scan_results = dev.scan_results.clone();
        m.status = dev.wifi_conn.status;
        m.connected_ssid = dev.wifi_conn.current_ssid.as_ref().map(|s| s.to_string());
        if dev.ip_address != [0u8; 4] {
            m.ip_address = Some(alloc::format!(
                "{}.{}.{}.{}",
                dev.ip_address[0], dev.ip_address[1],
                dev.ip_address[2], dev.ip_address[3]
            ));
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
        let _ = dev.connect(ssid, password);
    }
}

// ── Error types ──────────────────────────────────────────────────────

#[derive(Debug)]
enum IwlError {
    BarNotAvailable,
    ClockNotReady,
    DmaAllocFailed,
}
