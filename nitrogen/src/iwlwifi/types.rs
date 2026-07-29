//! Data structures and enums for the Intel Wireless 7265 driver.

use alloc::boxed::Box;
use alloc::vec::Vec;
use bonder::wifi::{AccessPoint, WifiStatus};

use crate::DriverContext;
use crate::mmio::DmaRegion;
use crate::pci::PciDevice;
use crate::pci_health::PciHealth;
use crate::wifi;

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
    Echo = 0x03,
    AddStaKey = 0x17,
    /// LMAC scan request for the 7265 firmware API (SCAN_OFFLOAD_REQUEST_CMD).
    /// 0x18 is ADD_STA, not a scan request.
    ScanRequest = 0x51,
    ScanAbort = 0x52,
    ScanResults = 0x83,
    Auth = 0x1A,
    Assoc = 0x1B,
    Disassoc = 0x1C,
    Deauth = 0x1D,
    AddSta = 0x18 | 0x40,
    MacContext = 0x28,
    TxAntConfig = 0x98,
    RxonAssoc = 0x20,
    PowerDown = 0x26,
    PowerUp = 0x27,
    ReplyAlive = 0x01,
    ReplyError = 0x02,
}

/// ADD_STA_KEY command payload used by the 7000-series firmware API.
///
/// The common part is kept byte-oriented here because this driver supports
/// firmware revisions with different response layouts, while the key command
/// input layout is stable: station id, key slot, flags, 32-byte key storage,
/// and a 16-byte receive sequence counter.
#[repr(C, packed)]
pub struct AddStaKeyCmd {
    pub sta_id: u8,
    pub key_offset: u8,
    pub key_flags: u16,
    pub key: [u8; 32],
    pub rx_security_seq: [u8; 16],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct MacAcQos {
    pub cw_min: u16,
    pub cw_max: u16,
    pub aifsn: u8,
    pub fifos_mask: u8,
    pub edca_txop: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct MacDataSta {
    pub is_assoc: u32,
    pub dtim_time: u32,
    pub dtim_tsf: u64,
    pub beacon_interval: u32,
    pub reserved1: u32,
    pub dtim_interval: u32,
    pub data_policy: u32,
    pub listen_interval: u32,
    pub assoc_id: u32,
    pub assoc_beacon_arrive_time: u32,
}

#[repr(C, packed)]
pub struct MacContextCmd {
    pub id_and_color: u32,
    pub action: u32,
    pub mac_type: u32,
    pub tsf_id: u32,
    pub node_addr: [u8; 6],
    pub reserved_for_node_addr: u16,
    pub bssid_addr: [u8; 6],
    pub reserved_for_bssid_addr: u16,
    pub cck_rates: u32,
    pub ofdm_rates: u32,
    pub protection_flags: u32,
    pub cck_short_preamble: u32,
    pub short_slot: u32,
    pub filter_flags: u32,
    pub qos_flags: u32,
    pub ac: [MacAcQos; 5],
    pub sta: MacDataSta,
}

impl MacContextCmd {
    pub fn station(mac: [u8; 6]) -> Self {
        Self {
            id_and_color: 0,
            action: 1,
            // FW_MAC_TYPE_BSS_STA
            mac_type: 5,
            tsf_id: 0,
            node_addr: mac,
            reserved_for_node_addr: 0,
            bssid_addr: [0xff; 6],
            reserved_for_bssid_addr: 0,
            cck_rates: 0x0000_000f,
            ofdm_rates: 0x0000_00ff,
            protection_flags: 0,
            cck_short_preamble: 0x20,
            short_slot: 0x10,
            // MAC_FILTER_ACCEPT_GRP | MAC_FILTER_IN_BEACON
            filter_flags: (1 << 2) | (1 << 6),
            qos_flags: 0,
            ac: [MacAcQos {
                cw_min: 3,
                cw_max: 1023,
                aifsn: 2,
                fifos_mask: 0,
                edca_txop: 0,
            }; 5],
            sta: MacDataSta {
                is_assoc: 0,
                dtim_time: 0,
                dtim_tsf: 0,
                beacon_interval: 100,
                reserved1: 0,
                dtim_interval: 100,
                data_policy: 0,
                listen_interval: 10,
                assoc_id: 0,
                assoc_beacon_arrive_time: 0,
            },
        }
    }
}

#[repr(C, packed)]
pub struct HcmdHeader {
    pub opcode: u8,
    pub group_id: u8,
    /// Queue and TFD sequence. The legacy header is exactly 4 bytes.
    pub sequence: u16,
}

#[repr(C, packed)]
pub struct HcmdResp {
    pub header: HcmdHeader,
    pub status: u32,
}

// ── Scan command structures ────────

const SCAN_CHANNEL_COUNT: usize = 23;
const SCAN_DIRECT_SSID_COUNT: usize = 20;
const SCAN_SSID_MAX_LEN: usize = 32;
const SCAN_PROBE_BUFFER_SIZE: usize = 512;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanReqTxCmd {
    pub tx_flags: u32,
    pub rate_n_flags: u32,
    pub sta_id: u8,
    pub reserved: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanSsidIe {
    pub id: u8,
    pub len: u8,
    pub ssid: [u8; SCAN_SSID_MAX_LEN],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanScheduleLmac {
    pub delay: u16,
    pub iterations: u8,
    pub full_scan_mul: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanChannelOpt {
    pub flags: u16,
    pub non_ebs_ratio: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanChannelCfgLmac {
    pub flags: u32,
    pub channel_num: u16,
    pub iter_count: u16,
    pub iter_interval: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanProbeSegment {
    pub offset: u16,
    pub len: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanProbeReqV1 {
    pub mac_header: ScanProbeSegment,
    pub band_data: [ScanProbeSegment; 2],
    pub common_data: ScanProbeSegment,
    pub buf: [u8; SCAN_PROBE_BUFFER_SIZE],
}

#[repr(C, packed)]
pub struct ScanRequestCmd {
    pub reserved1: u32,
    pub n_channels: u8,
    pub active_dwell: u8,
    pub passive_dwell: u8,
    pub fragmented_dwell: u8,
    pub extended_dwell: u8,
    pub reserved2: u8,
    pub rx_chain_select: u16,
    pub scan_flags: u32,
    pub max_out_time: u32,
    pub suspend_time: u32,
    pub flags: u32,
    pub filter_flags: u32,
    pub tx_cmd: [ScanReqTxCmd; 2],
    pub direct_scan: [ScanSsidIe; SCAN_DIRECT_SSID_COUNT],
    pub scan_prio: u32,
    pub iter_num: u32,
    pub delay: u32,
    pub schedule: [ScanScheduleLmac; 2],
    pub channel_opt: [ScanChannelOpt; 2],
    pub channels: [ScanChannelCfgLmac; SCAN_CHANNEL_COUNT],
    pub probe_req: ScanProbeReqV1,
}

impl ScanRequestCmd {
    pub fn new(mac: [u8; 6]) -> Self {
        let channel_numbers: [u16; SCAN_CHANNEL_COUNT] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 36, 40, 44, 48, 149, 153, 157, 161, 165,
        ];
        let mut channels = [ScanChannelCfgLmac {
            // A normal discovery scan covers the complete channel entry.
            // The LMAC API reserves bit 27 for FULL and bit 28 for PARTIAL;
            // using PARTIAL for every channel can leave the request waiting
            // for a scan plan that was never configured.
            flags: 1 << 27,
            channel_num: 0,
            iter_count: 1,
            iter_interval: 0,
        }; SCAN_CHANNEL_COUNT];
        for (channel, number) in channels.iter_mut().zip(channel_numbers) {
            channel.channel_num = number;
        }

        let mut probe = ScanProbeReqV1 {
            mac_header: ScanProbeSegment { offset: 0, len: 26 },
            band_data: [
                ScanProbeSegment { offset: 26, len: 0 },
                ScanProbeSegment { offset: 26, len: 0 },
            ],
            common_data: ScanProbeSegment { offset: 26, len: 0 },
            buf: [0; SCAN_PROBE_BUFFER_SIZE],
        };
        // Wildcard probe request. It is not transmitted for this passive
        // scan, but the LMAC API still requires a valid probe descriptor.
        probe.buf[0..2].copy_from_slice(&0x0040u16.to_le_bytes());
        probe.buf[4..10].fill(0xff);
        probe.buf[10..16].copy_from_slice(&mac);
        probe.buf[16..22].fill(0xff);
        probe.buf[24..26].fill(0);

        Self {
            reserved1: 0,
            n_channels: SCAN_CHANNEL_COUNT as u8,
            active_dwell: 10,
            passive_dwell: 110,
            fragmented_dwell: 44,
            extended_dwell: 90,
            reserved2: 0,
            // Two valid RX chains, selected/forced in the same way as Linux.
            rx_chain_select: 0x01b7,
            scan_flags: (1 << 0) | (1 << 1) | (1 << 3) | (1 << 7),
            // Regular (wild) scans use the 120-TU associated-channel
            // budget. 37 TU is the fast-balance budget and can terminate a
            // passive dwell before a beacon is delivered.
            max_out_time: 120,
            suspend_time: 30,
            // PHY_BAND_24 and MAC_FILTER_ACCEPT_GRP|MAC_FILTER_IN_BEACON.
            flags: 1,
            filter_flags: (1 << 2) | (1 << 6),
            tx_cmd: [
                ScanReqTxCmd {
                    tx_flags: 0,
                    rate_n_flags: 0,
                    sta_id: 0xff,
                    reserved: [0; 3],
                },
                ScanReqTxCmd {
                    tx_flags: 0,
                    rate_n_flags: 0,
                    sta_id: 0xff,
                    reserved: [0; 3],
                },
            ],
            direct_scan: [ScanSsidIe {
                id: 0,
                len: 0,
                ssid: [0; SCAN_SSID_MAX_LEN],
            }; SCAN_DIRECT_SSID_COUNT],
            scan_prio: 2,
            iter_num: 1,
            delay: 0,
            schedule: [
                ScanScheduleLmac {
                    delay: 0,
                    iterations: 1,
                    full_scan_mul: 1,
                },
                ScanScheduleLmac {
                    delay: 0,
                    iterations: 0,
                    full_scan_mul: 1,
                },
            ],
            channel_opt: [
                ScanChannelOpt {
                    flags: 0,
                    non_ebs_ratio: 1,
                },
                ScanChannelOpt {
                    flags: 0,
                    non_ebs_ratio: 1,
                },
            ],
            channels,
            probe_req: probe,
        }
    }
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
pub struct TxDmaTb {
    pub addr_lo: u32,
    /// Bits 3:0 are the high DMA address nibble; bits 15:4 are length.
    pub hi_n_len: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct TxDmaDesc {
    pub reserved: [u8; 3],
    pub num_tbs: u8,
    pub tbs: [TxDmaTb; 20],
    pub pad: u32,
}

/// Legacy 7265 receive-buffer descriptor. The FH RBD circular buffer is a
/// table of one dword entries containing RB physical address bits [35:8].
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct RxDmaDesc {
    pub addr: u32,
}

/// Hardware-written status for the legacy receive ring.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct RxDmaStatus {
    pub closed_rb_num: u16,
    pub closed_fr_num: u16,
    pub finished_rb_num: u16,
    pub finished_fr_num: u16,
    pub spare: u32,
}

impl TxDmaDesc {
    pub const fn zeroed() -> Self {
        Self {
            reserved: [0; 3],
            num_tbs: 0,
            tbs: [TxDmaTb {
                addr_lo: 0,
                hi_n_len: 0,
            }; 20],
            pad: 0,
        }
    }
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
