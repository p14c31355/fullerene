//! Data structures and enums for the Intel Wireless 7265 driver.

use alloc::boxed::Box;
use alloc::vec::Vec;
use bonder::wifi::{AccessPoint, WifiStatus};

use crate::mmio::DmaRegion;
use crate::pci::PciDevice;
use crate::pci_health::PciHealth;
use crate::wifi;
use crate::DriverContext;

// ── Firmware states ────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FwState {
    NotLoaded,
    Loading,
    Alive,
    Ready,
    Error,
}

// ── 802.11 operational mode ────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpMode {
    Sta,
    Ap,
    Monitor,
}

// ── Driver 802.11 state machine ────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IwlState {
    Init,
    ScanSent,
    Scanning,
    AuthSent,
    AssocSent,
    Connected,
    Disconnected,
}

// ── Firmware image header ──────────

#[repr(C, packed)]
pub struct FwHeader {
    pub zero: u32,
    pub magic: u32,
    pub description: [u8; 64],
    pub ver: u32,
    pub build: u32,
    pub ignore: u64,
}

// ── HCMD (Host Command) interface ──

#[repr(u8)]
pub enum GroupId {
    Legacy = 0x0,
    Long = 0x1,
    Phy = 0x4,
}

#[repr(u8)]
pub enum LegacyCmd {
    ScanRequest = 0x18,
    ScanAbort = 0x19,
    ScanResults = 0x83,
    Auth = 0x1A,
    Assoc = 0x1B,
    Disassoc = 0x1C,
    Deauth = 0x1D,
    AddSta = 0x18 | 0x40,
    Rxon = 0x1E,
    TxAntConfig = 0x0C,
    RxonAssoc = 0x20,
    PowerDown = 0x26,
    PowerUp = 0x27,
    ReplyAlive = 0x01,
    ReplyError = 0x02,
}

#[repr(C, packed)]
pub struct HcmdHeader {
    pub opcode: u8,
    pub group_id: u8,
    pub length: u16,
    pub flags: u16,
    pub reserved: u16,
}

#[repr(C, packed)]
pub struct HcmdResp {
    pub header: HcmdHeader,
    pub status: u32,
}

// ── Scan command structures ────────

#[repr(C, packed)]
pub struct ScanChannel {
    pub channel: u8,
    pub tx_power: u8,
    pub reserved: u16,
}

#[repr(C, packed)]
pub struct ScanRequestCmd {
    pub beacon_interval: u16,
    pub flags: u16,
    pub num_channels: u8,
    pub reserved: [u8; 3],
    pub channels: [ScanChannel; 4],
}

#[repr(C, packed)]
pub struct ScanNotification {
    pub status: u32,
    pub channel: u8,
    pub band: u8,
    pub reserved: [u8; 2],
    pub tsf_low: u32,
    pub tsf_high: u32,
    pub beacon_interval: u16,
    pub capability: u16,
    pub len: u16,
}

// ── DMA ring structures ────────────

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct TxDmaDesc {
    pub addr_lo: u32,
    pub addr_hi: u32,
    pub len: u16,
    pub flags: u16,
    pub reserved: [u32; 2],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct RxDmaDesc {
    pub addr_lo: u32,
    pub addr_hi: u32,
    pub len: u16,
    pub flags: u16,
}

#[repr(C, packed)]
pub struct RxPktStatus {
    pub len: u16,
    pub flags: u16,
}

// ── WifiManager (public snapshot) ───

#[derive(Clone)]
pub struct WifiManager {
    pub device_available: bool,
    pub scan_results: Vec<AccessPoint>,
    pub status: WifiStatus,
    pub connected_ssid: Option<alloc::string::String>,
    pub ip_address: Option<alloc::string::String>,
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

// ── Incremental init phase ────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WifiInitPhase {
    Idle = 0,
    PciProbe = 1,
    MmioInit = 2,
    MmioPollMacClock = 3,
    DmaAlloc = 4,
    FwUpload = 5,
    FwWaitAlive = 6,
    FwInitCmds = 7,
    Done = 8,
    Failed = 9,
}

impl From<u8> for WifiInitPhase {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Idle,
            1 => Self::PciProbe,
            2 => Self::MmioInit,
            3 => Self::MmioPollMacClock,
            4 => Self::DmaAlloc,
            5 => Self::FwUpload,
            6 => Self::FwWaitAlive,
            7 => Self::FwInitCmds,
            8 => Self::Done,
            _ => Self::Failed,
        }
    }
}

// ── Firmware blob registry ────────

pub struct FirmwareBlob {
    pub data: &'static [u8],
    pub name: &'static str,
}

// ── Incremental init context ──────

unsafe impl Send for WifiInitContext {}
pub struct WifiInitContext {
    pub mmio_device: Option<Box<dyn wifi::WifiDriver>>,
    pub fw_candidate_idx: usize,
    pub fw_candidates: &'static [FirmwareBlob],
    pub alive_start_tsc: u64,
    pub pci_dev: Option<PciDevice>,
    pub mmio: *mut u32,
    pub driver_ctx: Option<&'static dyn DriverContext>,
    pub health: Option<PciHealth>,
    pub hw_rev: u16,
    pub mac: Option<[u8; 6]>,
    pub tx_dma_ring: Option<DmaRegion>,
    pub rx_dma_ring: Option<DmaRegion>,
    pub tx_bufs: Vec<DmaRegion>,
    pub rx_bufs: Vec<DmaRegion>,
}

// ── Error types ────────────────────

#[derive(Debug)]
pub enum IwlError {
    BarNotAvailable,
    ClockNotReady,
    DmaAllocFailed,
}
