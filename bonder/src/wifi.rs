//! IEEE 802.11 Wireless LAN management.
//!
//! Provides data structures and parsing for 802.11 management frames
//! (beacons, probe requests/responses, authentication, association),
//! access point scanning, and connection state management.

use alloc::string::String;
use alloc::vec::Vec;

/// Maximum SSID length in bytes.
pub const SSID_MAX_LEN: usize = 32;

/// Service Set Identifier (network name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ssid(pub [u8; SSID_MAX_LEN], pub usize);

impl Ssid {
    pub fn new(name: &[u8]) -> Self {
        let len = name.len().min(SSID_MAX_LEN);
        let mut buf = [0u8; SSID_MAX_LEN];
        buf[..len].copy_from_slice(&name[..len]);
        Ssid(buf, len)
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0[..self.1]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.1
    }

    pub fn is_empty(&self) -> bool {
        self.1 == 0
    }
}

impl core::fmt::Display for Ssid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = core::str::from_utf8(&self.0[..self.1]).unwrap_or("<invalid>");
        write!(f, "{}", s)
    }
}

/// Basic Service Set Identifier (BSSID = MAC of the AP).
pub type Bssid = [u8; 6];

/// Security / encryption type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    Wep,
    WpaPsk,
    Wpa2Psk,
    Wpa3Sae,
}

impl Security {
    pub fn name(&self) -> &'static str {
        match self {
            Security::Open => "Open",
            Security::Wep => "WEP",
            Security::WpaPsk => "WPA-PSK",
            Security::Wpa2Psk => "WPA2-PSK",
            Security::Wpa3Sae => "WPA3-SAE",
        }
    }
    pub fn needs_password(&self) -> bool {
        !matches!(self, Security::Open)
    }
}

/// Signal strength indicator (RSSI in dBm).
pub type Rssi = i8;

/// A single access point discovered during scanning.
#[derive(Debug, Clone)]
pub struct AccessPoint {
    pub ssid: Ssid,
    pub bssid: Bssid,
    pub channel: u8,
    pub rssi: Rssi,
    pub security: Security,
    pub beacon_interval: u16,
}

/// 802.11 frame types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Management = 0,
    Control = 1,
    Data = 2,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(FrameType::Management),
            1 => Some(FrameType::Control),
            2 => Some(FrameType::Data),
            _ => None,
        }
    }
}

/// 802.11 management frame subtypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MgmtSubtype {
    AssociationRequest = 0,
    AssociationResponse = 1,
    ReassociationRequest = 2,
    ReassociationResponse = 3,
    ProbeRequest = 4,
    ProbeResponse = 5,
    Beacon = 8,
    Disassociation = 10,
    Authentication = 11,
    Deauthentication = 12,
    Action = 13,
}

impl MgmtSubtype {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(MgmtSubtype::AssociationRequest),
            1 => Some(MgmtSubtype::AssociationResponse),
            2 => Some(MgmtSubtype::ReassociationRequest),
            3 => Some(MgmtSubtype::ReassociationResponse),
            4 => Some(MgmtSubtype::ProbeRequest),
            5 => Some(MgmtSubtype::ProbeResponse),
            8 => Some(MgmtSubtype::Beacon),
            10 => Some(MgmtSubtype::Disassociation),
            11 => Some(MgmtSubtype::Authentication),
            12 => Some(MgmtSubtype::Deauthentication),
            13 => Some(MgmtSubtype::Action),
            _ => None,
        }
    }
}

/// 802.11 MAC frame header (24 bytes for standard data/management frames).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct WifiFrameHeader {
    pub frame_control: [u8; 2],
    pub duration_id: [u8; 2],
    pub addr1: [u8; 6],
    pub addr2: [u8; 6],
    pub addr3: [u8; 6],
    pub sequence_control: [u8; 2],
}

impl core::fmt::Debug for WifiFrameHeader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WifiFrameHeader")
            .field("frame_control", &self.frame_control)
            .field("addr1", &self.addr1)
            .field("addr2", &self.addr2)
            .field("addr3", &self.addr3)
            .finish()
    }
}

impl WifiFrameHeader {
    pub const SIZE: usize = 24;

    pub fn frame_type(&self) -> Option<FrameType> {
        FrameType::from_u8(self.frame_control[0] & 0x03)
    }

    pub fn mgmt_subtype(&self) -> Option<MgmtSubtype> {
        if self.frame_type() != Some(FrameType::Management) {
            return None;
        }
        MgmtSubtype::from_u8((self.frame_control[0] >> 4) & 0x0F)
    }
}

/// Parsed 802.11 beacon / probe response.
#[derive(Debug)]
pub struct BeaconFrame {
    pub header: WifiFrameHeader,
    pub timestamp: u64,
    pub beacon_interval: u16,
    pub capability: u16,
    pub ssid: Option<Ssid>,
    pub rates: Vec<u8>,
    pub ds_channel: Option<u8>,
    pub rsn: Option<RsnInfo>,
}

/// Parsed RSN (Robust Security Network) information element.
#[derive(Debug, Clone)]
pub struct RsnInfo {
    pub version: u16,
    pub group_cipher: u32,
    pub pair_cipher_count: u16,
    pub pair_ciphers: Vec<u32>,
    pub akm_count: u16,
    pub akms: Vec<u32>,
}

/// Authentication algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum AuthAlgorithm {
    Open = 0,
    SharedKey = 1,
    FastBssTransition = 2,
    Sae = 3,
}

/// Authentication frame body.
#[derive(Debug)]
pub struct AuthFrame {
    pub auth_algorithm: u16,
    pub auth_seq: u16,
    pub status_code: u16,
}

/// Association request frame body.
#[derive(Debug)]
pub struct AssocRequest {
    pub capability: u16,
    pub listen_interval: u16,
    pub ssid: Ssid,
    pub rates: Vec<u8>,
}

/// Association response frame body.
#[derive(Debug)]
pub struct AssocResponse {
    pub capability: u16,
    pub status_code: u16,
    pub aid: u16,
    pub rates: Vec<u8>,
}

/// Connection status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WifiStatus {
    #[default]
    Disconnected,
    Scanning,
    Authenticating,
    Associating,
    Handshake,
    Connected,
    Error,
}

/// WiFi connection state machine.
#[derive(Debug, Default)]
pub struct WifiConnection {
    pub status: WifiStatus,
    pub current_ssid: Option<Ssid>,
    pub current_bssid: Option<Bssid>,
    pub password: Option<String>,
    pub scan_results: Vec<AccessPoint>,
    pub auth_seq: u16,
    pub error_msg: Option<String>,
}

impl WifiConnection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self) -> bool {
        self.status == WifiStatus::Connected
    }

    pub fn start_scan(&mut self) {
        self.status = WifiStatus::Scanning;
        self.scan_results.clear();
        self.error_msg = None;
    }

    pub fn add_scan_result(&mut self, ap: AccessPoint) {
        // Avoid duplicates by BSSID
        if !self.scan_results.iter().any(|a| a.bssid == ap.bssid) {
            self.scan_results.push(ap);
        }
    }

    pub fn finish_scan(&mut self) {
        if self.status == WifiStatus::Scanning {
            self.status = WifiStatus::Disconnected;
        }
    }

    pub fn connect(&mut self, ssid: &Ssid, password: Option<&str>) {
        self.current_ssid = Some(ssid.clone());
        self.password = password.map(String::from);
        self.status = WifiStatus::Authenticating;
        self.auth_seq = 0;
        self.error_msg = None;
    }

    pub fn disconnect(&mut self) {
        self.status = WifiStatus::Disconnected;
        self.current_ssid = None;
        self.current_bssid = None;
        self.password = None;
        self.auth_seq = 0;
    }
}

/// Parse an 802.11 beacon or probe response frame.
pub fn parse_beacon(frame: &[u8]) -> Option<BeaconFrame> {
    if frame.len() < WifiFrameHeader::SIZE + 12 {
        return None;
    }

    let header = unsafe { core::ptr::read_unaligned(frame.as_ptr() as *const WifiFrameHeader) };

    let subtype = header.mgmt_subtype()?;
    if !matches!(subtype, MgmtSubtype::Beacon | MgmtSubtype::ProbeResponse) {
        return None;
    }

    let mut offset = WifiFrameHeader::SIZE;

    // Fixed parameters (12 bytes for beacon/probe response)
    let timestamp = u64::from_le_bytes([
        frame[offset],
        frame[offset + 1],
        frame[offset + 2],
        frame[offset + 3],
        frame[offset + 4],
        frame[offset + 5],
        frame[offset + 6],
        frame[offset + 7],
    ]);
    offset += 8;

    let beacon_interval = u16::from_le_bytes([frame[offset], frame[offset + 1]]);
    offset += 2;

    let capability = u16::from_le_bytes([frame[offset], frame[offset + 1]]);
    offset += 2;

    let mut ssid = None;
    let mut rates = Vec::new();
    let mut ds_channel = None;
    let mut rsn = None;

    // Tagged parameters
    while offset + 2 <= frame.len() {
        let tag_num = frame[offset];
        let tag_len = frame[offset + 1] as usize;
        offset += 2;
        if offset + tag_len > frame.len() {
            break;
        }

        match tag_num {
            0 => {
                // SSID
                let len = tag_len.min(SSID_MAX_LEN);
                let mut buf = [0u8; SSID_MAX_LEN];
                buf[..len].copy_from_slice(&frame[offset..offset + len]);
                ssid = Some(Ssid(buf, len));
            }
            1 => {
                // Supported Rates
                rates = frame[offset..offset + tag_len].to_vec();
            }
            3 => {
                // DS Parameter Set (channel)
                if tag_len >= 1 {
                    ds_channel = Some(frame[offset]);
                }
            }
            48 if tag_len >= 2 => {
                // RSN Information Element
                let version = u16::from_le_bytes([frame[offset], frame[offset + 1]]);
                let mut pos = offset + 2;
                let tag_end = offset + tag_len;

                let group_cipher = if pos + 4 <= tag_end {
                    u32::from_le_bytes([frame[pos], frame[pos + 1], frame[pos + 2], frame[pos + 3]])
                } else {
                    0
                };
                pos += 4;

                let pair_cipher_count = if pos + 2 <= tag_end {
                    u16::from_le_bytes([frame[pos], frame[pos + 1]])
                } else {
                    0
                };
                pos += 2;

                let mut pair_ciphers = Vec::new();
                for _ in 0..pair_cipher_count {
                    if pos + 4 <= tag_end {
                        pair_ciphers.push(u32::from_le_bytes([
                            frame[pos],
                            frame[pos + 1],
                            frame[pos + 2],
                            frame[pos + 3],
                        ]));
                        pos += 4;
                    }
                }

                let akm_count = if pos + 2 <= tag_end {
                    u16::from_le_bytes([frame[pos], frame[pos + 1]])
                } else {
                    0
                };
                pos += 2;

                let mut akms = Vec::new();
                for _ in 0..akm_count {
                    if pos + 4 <= tag_end {
                        akms.push(u32::from_le_bytes([
                            frame[pos],
                            frame[pos + 1],
                            frame[pos + 2],
                            frame[pos + 3],
                        ]));
                        pos += 4;
                    }
                }

                rsn = Some(RsnInfo {
                    version,
                    group_cipher,
                    pair_cipher_count,
                    pair_ciphers,
                    akm_count,
                    akms,
                });
            }
            _ => {}
        }
        offset += tag_len;
    }

    Some(BeaconFrame {
        header,
        timestamp,
        beacon_interval,
        capability,
        ssid,
        rates,
        ds_channel,
        rsn,
    })
}

/// Determine security type from capability and RSN info.
pub fn security_from_beacon(capability: u16, rsn: Option<&RsnInfo>) -> Security {
    if let Some(r) = rsn {
        for akm in &r.akms {
            match akm {
                0x000FAC01 | 0x000FAC05 => return Security::Wpa2Psk,
                0x000FAC02 => return Security::WpaPsk,
                0x000FAC08 => return Security::Wpa3Sae,
                _ => {}
            }
        }
    }

    let privacy = (capability >> 4) & 1;
    if privacy != 0 {
        // WEP or WPA (pre-RSN); default to WPA
        Security::WpaPsk
    } else {
        Security::Open
    }
}

/// Build a probe request frame.
pub fn build_probe_request(target: Option<&Ssid>) -> Vec<u8> {
    let mut frame = Vec::new();

    // Frame control: type=management(0), subtype=probe request(4)
    frame.push(0x40);
    frame.push(0x00);
    // Duration
    frame.extend_from_slice(&[0x00, 0x00]);
    // Addr1: broadcast
    frame.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    // Addr2: source (will be filled by driver)
    frame.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    // Addr3: broadcast
    frame.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    // Sequence control
    frame.extend_from_slice(&[0x00, 0x00]);

    // SSID IE
    match target {
        Some(ssid) => {
            frame.push(0x00);
            frame.push(ssid.len() as u8);
            frame.extend_from_slice(&ssid.0[..ssid.len()]);
        }
        None => {
            // Wildcard SSID (broadcast probe)
            frame.push(0x00);
            frame.push(0x00);
        }
    }

    // Supported rates
    frame.push(0x01);
    frame.push(0x08);
    frame.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]);

    // Extended supported rates
    frame.push(0x32);
    frame.push(0x04);
    frame.extend_from_slice(&[0x30, 0x48, 0x60, 0x6C]);

    // HT Capabilities (placeholder)
    frame.push(0x2D);
    frame.push(0x1A);
    frame.extend_from_slice(&[0x00; 26]);

    frame
}

/// Build an authentication frame (open system).
pub fn build_auth_frame(bssid: Bssid, client_mac: Bssid, seq: u16) -> Vec<u8> {
    let mut frame = Vec::new();

    // Frame control: type=management(0), subtype=auth(11)
    frame.push(0xB0);
    frame.push(0x00);
    // Duration
    frame.extend_from_slice(&[0x00, 0x00]);
    // Addr1: BSSID (AP)
    frame.extend_from_slice(&bssid);
    // Addr2: source (client MAC)
    frame.extend_from_slice(&client_mac);
    // Addr3: BSSID
    frame.extend_from_slice(&bssid);
    // Sequence control
    frame.extend_from_slice(&[0x00, 0x00]);

    // Auth algorithm (0 = open system)
    frame.extend_from_slice(&[0x00, 0x00]);
    // Auth transaction seq
    frame.extend_from_slice(&seq.to_le_bytes());
    // Status code (0 = success for seq 1)
    frame.extend_from_slice(&[0x00, 0x00]);

    frame
}

/// Build an association request frame.
pub fn build_assoc_request(bssid: Bssid, client_mac: Bssid, ssid: &Ssid) -> Vec<u8> {
    let mut frame = Vec::new();

    // Frame control: type=management(0), subtype=assoc request(0)
    frame.push(0x00);
    frame.push(0x00);
    // Duration
    frame.extend_from_slice(&[0x00, 0x00]);
    // Addr1: BSSID
    frame.extend_from_slice(&bssid);
    // Addr2: source
    frame.extend_from_slice(&client_mac);
    // Addr3: BSSID
    frame.extend_from_slice(&bssid);
    // Sequence control
    frame.extend_from_slice(&[0x00, 0x00]);

    // Capability: ESS=1, privacy=0 initially
    frame.extend_from_slice(&[0x01, 0x00]);
    // Listen interval
    frame.extend_from_slice(&[0x0A, 0x00]);

    // SSID
    frame.push(0x00);
    frame.push(ssid.len() as u8);
    frame.extend_from_slice(&ssid.0[..ssid.len()]);

    // Supported rates
    frame.push(0x01);
    frame.push(0x08);
    frame.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]);

    frame
}

/// Build a deauthentication frame.
pub fn build_deauth(bssid: Bssid, client_mac: Bssid, reason: u16) -> Vec<u8> {
    let mut frame = Vec::new();

    // Frame control: type=management(0), subtype=deauth(12)
    frame.push(0xC0);
    frame.push(0x00);
    frame.extend_from_slice(&[0x00, 0x00]);
    frame.extend_from_slice(&bssid);
    frame.extend_from_slice(&client_mac);
    frame.extend_from_slice(&bssid);
    frame.extend_from_slice(&[0x00, 0x00]);

    // Reason code
    frame.extend_from_slice(&reason.to_le_bytes());

    frame
}
