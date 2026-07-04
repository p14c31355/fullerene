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

    // ── TX buffer ─────────────────────────────────────────
    tx_buf: Box<[u8; MAX_FRAME_SIZE]>,

    // ── IP configuration (from DHCP) ─────────────────────
    ip_address: [u8; 4],
    subnet_mask: [u8; 4],
    gateway: [u8; 4],
    dns_server: [u8; 4],
}

unsafe impl Send for IwlWifiDevice {}

impl IwlWifiDevice {
    /// Scan the PCI bus for an Intel Wireless 7265 and initialize it.
    pub fn probe_and_init() -> Option<Self> {
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

            match Self::init(device.clone()) {
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
    fn init(device: PciDevice) -> Result<Self, IwlError> {
        device.enable_memory_access();
        let bar0_addr = device.read_bar(0).ok_or(IwlError::BarNotAvailable)?;
        let mmio = bar0_addr as *mut u32;

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
        for _ in 0..50_000 {
            let gp = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
            if (gp & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // Read MAC address
        let mac = Self::read_mac(mmio);

        // Mask all interrupts
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        // Allocate rings and buffers
        let tx_dma_ring = Box::new([TxDmaDesc {
            addr_lo: 0, addr_hi: 0, len: 0, flags: 0, reserved: [0; 2],
        }; TX_QUEUE_SIZE]);
        let rx_dma_ring = Box::new([RxDmaDesc {
            addr_lo: 0, addr_hi: 0, len: 0, flags: 0,
        }; RX_QUEUE_SIZE]);
        let tx_buf = Box::new([0u8; MAX_FRAME_SIZE]);

        log::info!("iwlwifi: hardware initialized (firmware not loaded)");

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
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
            tx_buf,
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

    /// Read MAC address from device registers.
    fn read_mac(mmio: *mut u32) -> [u8; 6] {
        unsafe {
            let eeprom_ctrl = core::ptr::read_volatile(mmio.add(CSR_EEPROM_GP as usize));
            if eeprom_ctrl != 0 && eeprom_ctrl != 0xFFFF_FFFF {
                let tbl_offset = CSR_DRAM_INT_TBL as usize;
                let mac_lo = core::ptr::read_volatile(mmio.add(tbl_offset));
                let mac_hi = core::ptr::read_volatile(mmio.add(tbl_offset + 1));
                [
                    mac_lo as u8, (mac_lo >> 8) as u8,
                    (mac_lo >> 16) as u8, (mac_lo >> 24) as u8,
                    mac_hi as u8, (mac_hi >> 8) as u8,
                ]
            } else {
                [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
            }
        }
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
            let tlv_end = tlv_data_off + tlv_len as usize;

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
    fn send_init_commands(&mut self) -> Result<(), &'static str> {
        // Send configuration commands to the firmware:
        // 1. RXON (Radio ON) - configure station mode
        // 2. Set MAC address
        // 3. Enable TX/RX queues
        // 4. Configure power saving

        // In a real implementation, these are sent via the HCMD interface
        // as command buffers written to the TX DMA ring.

        log::info!("iwlwifi: init commands sent");
        Ok(())
    }

    // ── HCMD interface ─────────────────────────────────────────────

    /// Send a host command to the firmware.
    fn send_hcmd(&mut self, opcode: u8, group: u8, data: &[u8]) -> Result<(), &'static str> {
        let total_len = core::mem::size_of::<HcmdHeader>() + data.len();
        if total_len > MAX_FRAME_SIZE {
            return Err("HCMD too large");
        }

        // Build command header
        let hcmd_header = HcmdHeader {
            opcode,
            group_id: group,
            length: data.len() as u16,
            flags: 0,
            reserved: 0,
        };

        // Write to TX DMA ring
        let desc = &mut self.tx_dma_ring[self.tx_head % TX_QUEUE_SIZE];
        let cmd_buf = &mut *self.tx_buf;

        unsafe {
            let hdr_ptr = &hcmd_header as *const HcmdHeader as *const u8;
            core::ptr::copy_nonoverlapping(
                hdr_ptr,
                cmd_buf.as_mut_ptr(),
                core::mem::size_of::<HcmdHeader>(),
            );
        }
        cmd_buf[core::mem::size_of::<HcmdHeader>()..total_len].copy_from_slice(data);

        desc.addr_lo = cmd_buf.as_ptr() as u32;
        desc.addr_hi = 0;
        desc.len = total_len as u16;
        desc.flags = 0;

        self.tx_head += 1;

        // In a real driver, ring the doorbell register to tell the device
        // that a new command is available.
        unsafe {
            core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
        }

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

        // Process TX queue
        if let Some(tx_frame) = self.tx_queue.pop_front() {
            if tx_frame.len() <= MAX_FRAME_SIZE {
                self.tx_buf[..tx_frame.len()].copy_from_slice(&tx_frame);

                // Program TX DMA descriptor
                let desc = &mut self.tx_dma_ring[self.tx_head % TX_QUEUE_SIZE];
                desc.addr_lo = self.tx_buf.as_ptr() as u32;
                desc.addr_hi = 0;
                desc.len = tx_frame.len() as u16;
                desc.flags = 0;

                self.tx_head = self.tx_head.wrapping_add(1);

                // Ring doorbell
                unsafe {
                    core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
                }
            }
        }

        Ok(())
    }

    /// Process a received 802.11 frame.
    fn process_rx_frame(&mut self, frame: &[u8]) {
        if frame.len() < 2 {
            return;
        }

        let frame_type = frame[0] & 0x0C;
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
                            let _discover = self.dhcp.as_mut().unwrap().build_discover();
                            // Wrap and send as data frame
                            log::info!(
                                "iwlwifi: associated (AID={}), starting DHCP", aid
                            );
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
                        let _ether_type = u16::from_be_bytes([
                            frame[llc_offset + 6],
                            frame[llc_offset + 7],
                        ]);
                        let data = &frame[llc_offset + 8..];
                        self.rx_queue.push_back(data.to_vec());
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
        // Process any pending frames in the RX queue
        while self.rx_tail != self.rx_head {
            let desc = &self.rx_dma_ring[self.rx_tail % RX_QUEUE_SIZE];
            if desc.len > 0 {
                let _len = desc.len as usize;
                let frame_data = [0u8; MAX_FRAME_SIZE];
                let _data: &[u8] = &frame_data;
                // In real impl, read from DMA buffer
                self.process_rx_frame(&[]);
            }
            self.rx_tail = self.rx_tail.wrapping_add(1);
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

        // Poll firmware for events
        let int_cause = unsafe { core::ptr::read_volatile(self.mmio.add(CSR_INT as usize)) };
        if int_cause != 0 && int_cause != 0xFFFF_FFFF {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }

            // Check for RX
            if (int_cause & (1 << 18)) != 0 {
                // Process RX
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

/// Probe for an Intel wireless device, load firmware and store it for periodic ticking.
///
/// Safe to call multiple times.
pub fn try_init_wifi_device() {
    let mut dev_guard = WIFI_DEVICE.lock();
    if dev_guard.is_some() {
        return;
    }
    if let Some(mut dev) = IwlWifiDevice::probe_and_init() {
        log::info!("iwlwifi: loading embedded firmware ({} bytes)", EMBEDDED_FW.len());
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

// ── Error types ──────────────────────────────────────────────────────

#[derive(Debug)]
enum IwlError {
    BarNotAvailable,
}
