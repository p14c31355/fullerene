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

/// Firmware image contained in the 7265 .ucode TLV stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareImage {
    /// Bootstrap image used for NVM access and PHY calibration.
    Init,
    /// Normal operational image.
    Runtime,
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

/// Number of service ticks for which RX beacons remain eligible after the
/// firmware reports scan completion.  The notification and the final RX DMA
/// buffers are not guaranteed to reach the host in the same interrupt.
pub const SCAN_RESULT_GRACE_TICKS: u32 = 512;

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
    PhyContext = 0x08,
    /// PHY configuration and calibration control used before MAC contexts.
    PhyConfiguration = 0x6a,
    /// Legacy LMAC scan configuration.  The command number is legacy, but
    /// firmware API 17 transports it through the long-command group.
    ScanConfig = 0x0c,
    AddStaKey = 0x17,
    /// Legacy scheduler queue configuration used before ADD_STA in non-DQA
    /// mode. The firmware initially associates the queue with station 0.
    ScdQueueCfg = 0x1d,
    /// LMAC scan request for the 7265 firmware API (SCAN_OFFLOAD_REQUEST_CMD).
    /// 0x18 is ADD_STA, not a scan request.
    ScanRequest = 0x51,
    ScanAbort = 0x52,
    ScanResults = 0x83,
    Auth = 0x1A,
    Assoc = 0x1B,
    Disassoc = 0x1C,
    AddSta = 0x18,
    MacContext = 0x28,
    TxAntConfig = 0x98,
    RxonAssoc = 0x20,
    PowerDown = 0x26,
    PowerUp = 0x27,
    ReplyAlive = 0x01,
    ReplyError = 0x02,
    InitCompleteNotif = 0x04,
    /// NVM access command. API 17 sends it in the Legacy group; only the
    /// completion command uses the Regulatory/NVM group.
    NvmAccess = 0x88,
    /// PHY calibration database notification emitted by INIT firmware.
    CalibResNotifPhyDb = 0x6b,
    /// PHY calibration database section accepted by runtime firmware.
    PhyDb = 0x6c,
    /// RX MPDU notification (legacy transport). Carries the raw 802.11 frame
    /// preceded by an `iwl_rx_mpdu_res_start` header.
    ReplyRxMpduCmd = 0xc1,
    /// Scan-offload completion notification, carrying periodic scan status.
    ScanCompleteUrgent = 0x6d,
    /// LMAC scan iteration completion notification.
    ScanOffloadCompleteNotif = 0xe7,
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
pub struct HcmdHeader {
    pub opcode: u8,
    pub group_id: u8,
    /// Queue and TFD sequence. The legacy header is exactly 4 bytes.
    pub sequence: u16,
}

/// Host-command header used for the long command group.
///
/// Group 0 uses [`HcmdHeader`].  Group 1 and later use this header and the
/// firmware expects `length` to contain the payload length, excluding this
/// eight-byte header.
#[repr(C, packed)]
pub struct HcmdHeaderWide {
    pub opcode: u8,
    pub group_id: u8,
    pub sequence: u16,
    pub length: u16,
    pub reserved: u8,
    pub version: u8,
}

#[repr(C, packed)]
pub struct HcmdResp {
    pub header: HcmdHeader,
    pub status: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct PhyChannelInfoV1 {
    pub band: u8,
    pub channel: u8,
    pub width: u8,
    pub ctrl_pos: u8,
}

/// PHY_CONTEXT_CMD API v1 used by the 7265 firmware.
///
/// The old channel-info form is four bytes; the newer API's channel number is
/// a le32 and must not be used with firmware API 17.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct PhyContextCmdV1 {
    pub id_and_color: u32,
    pub action: u32,
    pub apply_time: u32,
    pub tx_param_color: u32,
    pub channel: PhyChannelInfoV1,
    pub txchain_info: u32,
    pub rxchain_info: u32,
    pub acquisition_data: u32,
    pub dsp_cfg_flags: u32,
}

impl PhyContextCmdV1 {
    pub fn add(id: u8) -> Self {
        Self {
            id_and_color: id as u32,
            action: 1, // FW_CTXT_ACTION_ADD
            apply_time: 0,
            tx_param_color: 0,
            channel: PhyChannelInfoV1 {
                band: 1, // PHY_BAND_24
                channel: 1,
                width: 0, // 20 MHz, no HT
                ctrl_pos: 0,
            },
            txchain_info: 0x03,
            // Valid chains A+B, two idle chains, two active/MIMO chains.
            rxchain_info: (0x03 << 1) | (2 << 10) | (2 << 12),
            acquisition_data: 0,
            dsp_cfg_flags: 0,
        }
    }
}

/// PHY_CONFIGURATION_CMD payload used by the API-v17 runtime firmware.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct PhyConfigurationCmd {
    pub phy_config: u32,
    pub calib_flow_trigger: u32,
    pub calib_event_trigger: u32,
}

/// SCD_QUEUE_CFG_CMD_API_S_VER_1, used by the legacy non-DQA scheduler.
///
/// Linux sends this before ADD_STA for the auxiliary queue. The auxiliary
/// station is allocated first and its real internal station ID is already
/// used here; the following ADD_STA publishes the same queue mask.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScdTxqCfgCmdV1 {
    pub token: u8,
    pub sta_id: u8,
    pub tid: u8,
    pub scd_queue: u8,
    pub action: u8,
    pub aggregate: u8,
    pub tx_fifo: u8,
    pub window: u8,
    pub ssn: u16,
    pub reserved: u16,
}

impl ScdTxqCfgCmdV1 {
    pub fn aux(sta_id: u8) -> Self {
        use super::registers::IWL_AUX_QUEUE;
        Self {
            token: 0,
            // Linux allocates the AUX station-table entry before enabling its
            // queue, so SCD_QUEUE_CFG must name that station (normally 1).
            sta_id,
            tid: 15, // IWL_MAX_TID_COUNT
            scd_queue: IWL_AUX_QUEUE as u8,
            action: 1,    // SCD_CFG_ENABLE_QUEUE
            aggregate: 0, // non-aggregated auxiliary queue
            tx_fifo: 5,   // IWL_MVM_TX_FIFO_MCAST
            window: 64,
            ssn: 0,
            reserved: 0,
        }
    }
}

/// One AC entry in the API-v1 MAC context QoS array.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct MacQosAc {
    pub cw_min: u16,
    pub cw_max: u16,
    pub aifsn: u8,
    pub fifos_mask: u8,
    pub edca_txop: u16,
}

/// Station-specific portion of the API-v1 MAC context union.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct MacStaData {
    pub is_assoc: u32,
    pub dtim_time: u32,
    pub dtim_tsf: u64,
    pub beacon_interval: u32,
    pub beacon_interval_reciprocal: u32,
    pub dtim_interval: u32,
    pub dtim_interval_reciprocal: u32,
    pub listen_interval: u32,
    pub assoc_id: u32,
    pub assoc_beacon_arrive_time: u32,
}

/// MAC_CONTEXT_CMD (0x28) payload for a minimal STA context.
///
/// This is the packed `MAC_CONTEXT_CMD_API_S_VER_1` layout used by the
/// 7265 firmware. In particular, the common fields, five AC QoS entries,
/// and the 44-byte STA union are all part of the command. Sending the old
/// shortened structure leaves the firmware command queue stopped at this
/// command, so the subsequent scan request is never executed.
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
    pub ac: [MacQosAc; 5],
    pub sta: MacStaData,
}

impl MacContextCmd {
    /// Create a minimal STA MAC context that accepts beacons and multicast
    /// frames — enough for passive scanning.
    pub fn sta(mac: [u8; 6]) -> Self {
        // Linux API v1: MAC_FILTER_ACCEPT_GRP = BIT(2),
        // MAC_FILTER_IN_BEACON = BIT(6).
        const FILTER_FLAGS: u32 = (1 << 2) | (1 << 6);
        Self {
            id_and_color: 0,
            action: 1,
            // FW_MAC_TYPE_BSS_STA.  FW_MAC_TYPE_AUX is 1 and is reserved for
            // the auxiliary scan station (MAC index 4).
            mac_type: 5,
            tsf_id: 0,
            node_addr: mac,
            reserved_for_node_addr: 0,
            // mac80211 keeps the station BSSID zeroed until association. The
            // firmware accepts that value for an unassociated BSS_STA
            // context; a broadcast BSSID is a different (and invalid for
            // this context) address value.
            bssid_addr: [0; 6],
            reserved_for_bssid_addr: 0,
            cck_rates: 0x0000_000f,
            // For an unassociated 2.4 GHz STA with no AP basic-rate set,
            // iwl_mvm_ack_rates() keeps the mandatory 6/12/24 Mbps OFDM
            // rates: bits 0, 2 and 4 in the OFDM bitmap.
            ofdm_rates: 0x0000_0015,
            protection_flags: 0,
            // The interface starts before association.  mac80211 leaves
            // both ERP flags clear until the AP advertises them.
            cck_short_preamble: 0,
            short_slot: 0,
            filter_flags: FILTER_FLAGS,
            qos_flags: 0,
            // API v1 expects each of the four EDCA entries to name the FIFO
            // owned by that access category.  Before association mac80211
            // has not supplied queue parameters yet, so Linux leaves the
            // timing values zero and only sets the FIFO masks. The fifth
            // entry is reserved (AC_NUM + 1) and stays zero.
            ac: [
                MacQosAc {
                    cw_min: 0,
                    cw_max: 0,
                    aifsn: 0,
                    fifos_mask: 1 << 0,
                    edca_txop: 0,
                },
                MacQosAc {
                    cw_min: 0,
                    cw_max: 0,
                    aifsn: 0,
                    fifos_mask: 1 << 1,
                    edca_txop: 0,
                },
                MacQosAc {
                    cw_min: 0,
                    cw_max: 0,
                    aifsn: 0,
                    fifos_mask: 1 << 2,
                    edca_txop: 0,
                },
                MacQosAc {
                    cw_min: 0,
                    cw_max: 0,
                    aifsn: 0,
                    fifos_mask: 1 << 3,
                    edca_txop: 0,
                },
                MacQosAc {
                    cw_min: 0,
                    cw_max: 0,
                    aifsn: 0,
                    fifos_mask: 0,
                    edca_txop: 0,
                },
            ],
            sta: MacStaData {
                is_assoc: 0,
                dtim_time: 0,
                dtim_tsf: 0,
                beacon_interval: 100,
                beacon_interval_reciprocal: 0x028f_5c28,
                dtim_interval: 0,
                dtim_interval_reciprocal: 0,
                listen_interval: 10,
                assoc_id: 0,
                assoc_beacon_arrive_time: 0,
            },
        }
    }
}

/// ADD_STA command API v7, used by the old (pre-v12) station API.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AddStaCmdV7 {
    pub add_modify: u8,
    pub awake_acs: u8,
    pub tid_disable_tx: u16,
    pub mac_id_n_color: u32,
    pub addr: [u8; 6],
    pub reserved2: u16,
    pub sta_id: u8,
    pub modify_mask: u8,
    pub reserved3: u16,
    pub station_flags: u32,
    pub station_flags_msk: u32,
    pub add_immediate_ba_tid: u8,
    pub remove_immediate_ba_tid: u8,
    pub add_immediate_ba_ssn: u16,
    pub sleep_tx_count: u16,
    pub sleep_state_flags: u16,
    pub assoc_id: u16,
    pub beamform_flags: u16,
    pub tfd_queue_msk: u32,
}

impl AddStaCmdV7 {
    pub fn aux(mac_index: u8, sta_id: u8) -> Self {
        // The 7265 non-DQA layout reserves queue 11 for the auxiliary
        // station.  Linux advertises that queue in tfd_queue_msk even when
        // the first scan is passive; leaving it zero makes API-v17 firmware
        // reject the station command before it can return ADD_STA status.
        use super::registers::IWL_AUX_QUEUE;
        Self {
            add_modify: 0, // STA_MODE_ADD
            awake_acs: 0,
            tid_disable_tx: 0xffff,
            // mac_id_n_color names the AUX MAC context (index 4); sta_id is
            // an independent entry in the firmware station table.
            mac_id_n_color: mac_index as u32,
            addr: [0; 6],
            reserved2: 0,
            sta_id,
            modify_mask: 0,
            reserved3: 0,
            station_flags: 0,
            station_flags_msk: 0,
            add_immediate_ba_tid: 0,
            remove_immediate_ba_tid: 0,
            add_immediate_ba_ssn: 0,
            sleep_tx_count: 0,
            sleep_state_flags: 0,
            assoc_id: 0,
            beamform_flags: 0,
            tfd_queue_msk: 1 << IWL_AUX_QUEUE,
        }
    }
}

// ── Scan command structures ────────

pub const SCAN_CHANNEL_COUNT: usize = 23;
const SCAN_DIRECT_SSID_COUNT: usize = 20;
const SCAN_SSID_MAX_LEN: usize = 32;
const SCAN_PROBE_BUFFER_SIZE: usize = 512;

const SCAN_CHANNELS: [u8; SCAN_CHANNEL_COUNT] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 36, 40, 44, 48, 149, 153, 157, 161, 165,
];

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanDwellV1 {
    pub active: u8,
    pub passive: u8,
    pub fragmented: u8,
    pub reserved: u8,
}

/// SCAN_CFG_CMD API v1 payload used by the 7265 firmware.
///
/// This command is not the same as the later UMAC scan configuration: the
/// channel list is part of the payload and the command is sent with the
/// eight-byte wide-command header because it belongs to LONG_GROUP.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ScanConfigV1 {
    pub flags: u32,
    pub tx_chains: u32,
    pub rx_chains: u32,
    pub legacy_rates: u32,
    pub out_of_channel_time: u32,
    pub suspend_time: u32,
    pub dwell: ScanDwellV1,
    pub mac_addr: [u8; 6],
    pub bcast_sta_id: u8,
    pub channel_flags: u8,
    pub channel_array: [u8; SCAN_CHANNEL_COUNT],
}

impl ScanConfigV1 {
    pub fn new(mac_addr: [u8; 6], bcast_sta_id: u8) -> Self {
        // This is the API-v1 SCAN_CONFIG_DB_CMD flag set.  In particular,
        // SET_AUX_STA_ID and CLEAR_FRAGMENTED are not part of the firmware's
        // initial scan configuration command; their bits are reserved here.
        let flags = (1 << 0)
            | (1 << 3)
            | (1 << 8)
            | (1 << 9)
            | (1 << 11)
            | (1 << 13)
            | (1 << 14)
            | (1 << 15)
            | ((SCAN_CHANNEL_COUNT as u32) << 26);

        Self {
            flags,
            tx_chains: 0x03,
            rx_chains: 0x03,
            legacy_rates: 0x0fff_0fff,
            out_of_channel_time: 170,
            suspend_time: 30,
            dwell: ScanDwellV1 {
                active: 20,
                passive: 110,
                fragmented: 20,
                reserved: 0,
            },
            mac_addr,
            bcast_sta_id,
            // EBS | ACCURATE_EBS | EBS_ADD | PRE_SCAN_PASSIVE2ACTIVE.
            channel_flags: 0x0f,
            channel_array: SCAN_CHANNELS,
        }
    }
}

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
    pub fn new(mac: [u8; 6], aux_sta_id: u8) -> Self {
        let mut channels = [ScanChannelCfgLmac {
            // Each entry is explicitly supplied by this request.  The
            // legacy LMAC API marks that form as PARTIAL; FULL is reserved
            // for a firmware-managed channel plan.
            flags: 1 << 28,
            channel_num: 0,
            iter_count: 1,
            iter_interval: 0,
        }; SCAN_CHANNEL_COUNT];
        for (channel, number) in channels.iter_mut().zip(SCAN_CHANNELS) {
            channel.channel_num = number as u16;
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
                    // The legacy scan engine transmits through the auxiliary
                    // station created during firmware initialization.
                    sta_id: aux_sta_id,
                    reserved: [0; 3],
                },
                ScanReqTxCmd {
                    tx_flags: 0,
                    rate_n_flags: 0,
                    sta_id: aux_sta_id,
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
    FwRuntimeUpload = 8,
    FwRuntimeWaitAlive = 9,
    FwRuntimeCmds = 10,
    Done = 11,
    Failed = 12,
}

impl WifiInitPhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::PciProbe => "pci_probe",
            Self::MmioInit => "mmio_init",
            Self::MmioPollMacClock => "mmio_poll_mac_clock",
            Self::DmaAlloc => "dma_alloc",
            Self::FwUpload => "fw_upload",
            Self::FwWaitAlive => "fw_wait_alive",
            Self::FwInitCmds => "fw_init_cmds",
            Self::FwRuntimeUpload => "fw_runtime_upload",
            Self::FwRuntimeWaitAlive => "fw_runtime_wait_alive",
            Self::FwRuntimeCmds => "fw_runtime_cmds",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Short labels suitable for the boot-screen Hint area.
    pub const fn screen_label(self) -> &'static [u8] {
        match self {
            Self::Idle => b"WIFI WAIT",
            Self::PciProbe => b"WIFI PCI",
            Self::MmioInit => b"WIFI MMIO",
            Self::MmioPollMacClock => b"WIFI CLOCK",
            Self::DmaAlloc => b"WIFI DMA",
            Self::FwUpload => b"WIFI FW LOAD",
            Self::FwWaitAlive => b"WIFI FW ALIVE",
            Self::FwInitCmds => b"WIFI COMMANDS",
            Self::FwRuntimeUpload => b"WIFI RT LOAD",
            Self::FwRuntimeWaitAlive => b"WIFI RT ALIVE",
            Self::FwRuntimeCmds => b"WIFI RT CMDS",
            Self::Done => b"WIFI READY",
            Self::Failed => b"WIFI FAILED",
        }
    }
}

impl From<u8> for WifiInitPhase {
    fn from(v: u8) -> Self {
        // Discriminants are contiguous 0..=12; any value outside that range
        // (and 9 itself) collapses to `Failed`, matching the prior match.
        const PHASES: [WifiInitPhase; 13] = [
            WifiInitPhase::Idle,
            WifiInitPhase::PciProbe,
            WifiInitPhase::MmioInit,
            WifiInitPhase::MmioPollMacClock,
            WifiInitPhase::DmaAlloc,
            WifiInitPhase::FwUpload,
            WifiInitPhase::FwWaitAlive,
            WifiInitPhase::FwInitCmds,
            WifiInitPhase::FwRuntimeUpload,
            WifiInitPhase::FwRuntimeWaitAlive,
            WifiInitPhase::FwRuntimeCmds,
            WifiInitPhase::Done,
            WifiInitPhase::Failed,
        ];
        PHASES
            .get(v as usize)
            .copied()
            .unwrap_or(WifiInitPhase::Failed)
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
