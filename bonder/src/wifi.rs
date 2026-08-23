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
    pub beacon_timestamp: u64,
    pub device_timestamp: u32,
    pub dtim_count: u8,
    pub dtim_period: u8,
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
    pub dtim_count: u8,
    pub dtim_period: u8,
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
    let mut dtim_count = 0;
    let mut dtim_period = 0;
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
            5 => {
                // TIM: DTIM Count, DTIM Period, Bitmap Control, bitmap.
                if tag_len >= 4 {
                    dtim_count = frame[offset];
                    dtim_period = frame[offset + 1];
                }
            }
            48 if tag_len >= 2 => {
                // RSN Information Element
                let version = u16::from_le_bytes([frame[offset], frame[offset + 1]]);
                let mut pos = offset + 2;
                let tag_end = offset + tag_len;

                let group_cipher = if pos + 4 <= tag_end {
                    u32::from_be_bytes([frame[pos], frame[pos + 1], frame[pos + 2], frame[pos + 3]])
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
                        pair_ciphers.push(u32::from_be_bytes([
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
                        akms.push(u32::from_be_bytes([
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
        dtim_count,
        dtim_period,
        rsn,
    })
}

/// Determine security type from capability and RSN info.
pub fn security_from_beacon(capability: u16, rsn: Option<&RsnInfo>) -> Security {
    if let Some(r) = rsn {
        for akm in &r.akms {
            match akm {
                0x000FAC01 | 0x000FAC02 | 0x000FAC05 => return Security::Wpa2Psk,
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
    let mut frame = Vec::with_capacity(64 + target.map_or(0, Ssid::len));

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
    let mut frame = Vec::with_capacity(30);

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
    build_assoc_request_with_security(bssid, client_mac, ssid, false)
}

/// Build an association request, advertising the privacy capability when the
/// caller is about to run WPA2-PSK.  An AP must not be told that an encrypted
/// association is open, otherwise it may accept data before the 4-way
/// handshake has completed.
pub fn build_assoc_request_with_security(
    bssid: Bssid,
    client_mac: Bssid,
    ssid: &Ssid,
    privacy: bool,
) -> Vec<u8> {
    let mut frame = Vec::with_capacity(64 + ssid.len() + usize::from(privacy) * 22);

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

    // Capability: ESS=1, privacy=1 for WPA/WPA2 associations.
    frame.extend_from_slice(&[(0x01 | if privacy { 0x10 } else { 0x00 }), 0x00]);
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

    if privacy {
        // RSN IE: WPA2-PSK with CCMP (group and pairwise cipher).
        frame.push(0x30);
        frame.push(20);
        frame.extend_from_slice(&[
            0x01, 0x00, // RSN version
            0x00, 0x0F, 0xAC, 0x04, // group cipher: CCMP
            0x01, 0x00, // pairwise cipher count
            0x00, 0x0F, 0xAC, 0x04, // pairwise cipher: CCMP
            0x01, 0x00, // AKM count
            0x00, 0x0F, 0xAC, 0x02, // AKM: PSK
            0x00, 0x00, // RSN capabilities
        ]);
    }

    frame
}

/// Build a deauthentication frame.
pub fn build_deauth(bssid: Bssid, client_mac: Bssid, reason: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(26);

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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;

    // ── Test helpers ────────────────────────────────────────

    /// Build a beacon/probe-response frame for testing.
    fn build_beacon_frame(
        subtype: u8,
        bssid: Bssid,
        ssid: &[u8],
        channel: u8,
        capability: u16,
        rsn_ie: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut f = Vec::new();
        // Frame control: management type (0), subtype in upper nibble
        f.push(subtype << 4);
        f.push(0x00);
        // Duration
        f.extend_from_slice(&[0x00, 0x00]);
        // Addr1: broadcast
        f.extend_from_slice(&[0xFF; 6]);
        // Addr2: SA (AP)
        f.extend_from_slice(&bssid);
        // Addr3: BSSID
        f.extend_from_slice(&bssid);
        // Sequence control
        f.extend_from_slice(&[0x10, 0x00]);
        // Fixed params
        f.extend_from_slice(&0u64.to_le_bytes()); // timestamp
        f.extend_from_slice(&100u16.to_le_bytes()); // beacon interval
        f.extend_from_slice(&capability.to_le_bytes());
        // SSID IE
        f.push(0x00);
        f.push(ssid.len() as u8);
        f.extend_from_slice(ssid);
        // Rates IE
        f.push(0x01);
        f.push(0x04);
        f.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96]);
        // DS Channel IE
        f.push(0x03);
        f.push(0x01);
        f.push(channel);
        // RSN IE (optional)
        if let Some(rsn) = rsn_ie {
            f.extend_from_slice(rsn);
        }
        f
    }

    /// Standard WPA2-PSK/CCMP RSN IE bytes (element ID + length + data).
    fn rsn_ie_wpa2_psk() -> Vec<u8> {
        let mut ie = Vec::new();
        ie.push(0x30); // Element ID
        ie.push(20); // Length
        ie.extend_from_slice(&1u16.to_le_bytes()); // Version
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // Group: CCMP
        ie.extend_from_slice(&1u16.to_le_bytes()); // Pair count
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // Pair: CCMP
        ie.extend_from_slice(&1u16.to_le_bytes()); // AKM count
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]); // AKM: PSK
        ie.extend_from_slice(&0u16.to_le_bytes()); // Capabilities
        ie
    }

    /// WPA3-SAE RSN IE.
    fn rsn_ie_wpa3_sae() -> Vec<u8> {
        let mut ie = Vec::new();
        ie.push(0x30);
        ie.push(20);
        ie.extend_from_slice(&1u16.to_le_bytes());
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // CCMP
        ie.extend_from_slice(&1u16.to_le_bytes());
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        ie.extend_from_slice(&1u16.to_le_bytes());
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x08]); // SAE
        ie.extend_from_slice(&0u16.to_le_bytes());
        ie
    }

    /// RSN IE with multiple AKMs (WPA2-PSK + WPA3-SAE transition).
    fn rsn_ie_wpa3_transition() -> Vec<u8> {
        let mut ie = Vec::new();
        ie.push(0x30);
        ie.push(24); // 20 + 4 for extra AKM
        ie.extend_from_slice(&1u16.to_le_bytes());
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        ie.extend_from_slice(&1u16.to_le_bytes());
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        ie.extend_from_slice(&2u16.to_le_bytes()); // 2 AKMs
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]); // PSK
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x08]); // SAE
        ie.extend_from_slice(&0u16.to_le_bytes());
        ie
    }

    // ── Ssid tests ──────────────────────────────────────────

    #[test]
    fn ssid_basic_operations() {
        let s = Ssid::new(b"Hello");
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        assert_eq!(s.as_str(), "Hello");
        assert_eq!(format!("{}", s), "Hello");
    }

    #[test]
    fn ssid_truncation_to_32_bytes() {
        let long = b"ThisIsAVeryLongSSIDThatExceeds32Bytes!!!!";
        let s = Ssid::new(long);
        assert_eq!(s.len(), SSID_MAX_LEN);
    }

    #[test]
    fn ssid_empty() {
        let s = Ssid::new(b"");
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn ssid_non_utf8_shows_invalid() {
        let s = Ssid::new(&[0xFF, 0xFE, 0xFD]);
        assert_eq!(s.len(), 3);
        assert_eq!(s.as_str(), "");
        assert_eq!(format!("{}", s), "<invalid>");
    }

    // ── Beacon parsing tests ────────────────────────────────

    #[test]
    fn parse_open_beacon_2ghz() {
        let bssid = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let frame = build_beacon_frame(8, bssid, b"OpenNet", 6, 0x0001, None);

        let beacon = parse_beacon(&frame).expect("beacon parse");
        assert_eq!(beacon.header.mgmt_subtype(), Some(MgmtSubtype::Beacon));
        assert_eq!(beacon.ssid.as_ref().unwrap().as_str(), "OpenNet");
        assert_eq!(beacon.ds_channel, Some(6));
        assert_eq!(beacon.capability, 0x0001);
        assert!(beacon.rsn.is_none());
        assert_eq!(beacon.beacon_interval, 100);
        assert_eq!(beacon.rates, vec![0x82, 0x84, 0x8B, 0x96]);
    }

    #[test]
    fn parse_beacon_preserves_dtim_count_and_period() {
        let bssid = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let mut frame = build_beacon_frame(8, bssid, b"TimedAP", 6, 0x0001, None);
        frame.extend_from_slice(&[5, 4, 2, 3, 0, 0]);

        let beacon = parse_beacon(&frame).expect("beacon parse");
        assert_eq!(beacon.dtim_count, 2);
        assert_eq!(beacon.dtim_period, 3);
    }

    #[test]
    fn parse_wpa2_beacon() {
        let bssid = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let rsn = rsn_ie_wpa2_psk();
        let frame = build_beacon_frame(8, bssid, b"WPA2Net", 11, 0x0011, Some(&rsn));

        let beacon = parse_beacon(&frame).expect("beacon parse");
        assert_eq!(beacon.ssid.as_ref().unwrap().as_str(), "WPA2Net");
        assert_eq!(beacon.ds_channel, Some(11));
        assert_eq!(beacon.capability, 0x0011);

        let rsn = beacon.rsn.as_ref().expect("RSN IE");
        assert_eq!(rsn.version, 1);
        assert_eq!(rsn.group_cipher, 0x000FAC04); // CCMP
        assert_eq!(rsn.pair_cipher_count, 1);
        assert_eq!(rsn.pair_ciphers, vec![0x000FAC04u32]);
        assert_eq!(rsn.akm_count, 1);
        assert_eq!(rsn.akms, vec![0x000FAC02u32]); // PSK
    }

    #[test]
    fn parse_5ghz_beacon_ch36() {
        let bssid = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
        let rsn = rsn_ie_wpa2_psk();
        // Channel 36 in 5GHz band
        let frame = build_beacon_frame(8, bssid, b"Buffalo-A-2218", 36, 0x0011, Some(&rsn));

        let beacon = parse_beacon(&frame).expect("beacon parse");
        assert_eq!(beacon.ssid.as_ref().unwrap().as_str(), "Buffalo-A-2218");
        assert_eq!(beacon.ds_channel, Some(36));
        assert!(beacon.rsn.is_some());
    }

    #[test]
    fn parse_probe_response() {
        let bssid = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let frame = build_beacon_frame(5, bssid, b"ProbeTest", 1, 0x0001, None);

        let beacon = parse_beacon(&frame).expect("probe response parse");
        assert_eq!(
            beacon.header.mgmt_subtype(),
            Some(MgmtSubtype::ProbeResponse)
        );
        assert_eq!(beacon.ssid.as_ref().unwrap().as_str(), "ProbeTest");
    }

    #[test]
    fn parse_beacon_too_short() {
        assert!(parse_beacon(&[0x80, 0x00, 0x00]).is_none());
    }

    #[test]
    fn parse_beacon_wrong_subtype() {
        // Authentication frame (subtype 11) should not parse as beacon
        let frame = build_beacon_frame(11, [0; 6], b"Test", 1, 0x0001, None);
        assert!(parse_beacon(&frame).is_none());
    }

    #[test]
    fn parse_beacon_with_wpa3_transition() {
        let bssid = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let rsn = rsn_ie_wpa3_transition();
        let frame = build_beacon_frame(8, bssid, b"WPA3Trans", 36, 0x0011, Some(&rsn));

        let beacon = parse_beacon(&frame).expect("beacon parse");
        let rsn = beacon.rsn.as_ref().expect("RSN IE");
        assert_eq!(rsn.akm_count, 2);
        assert_eq!(rsn.akms.len(), 2);
        assert_eq!(rsn.akms[0], 0x000FAC02); // PSK
        assert_eq!(rsn.akms[1], 0x000FAC08); // SAE
    }

    #[test]
    fn parse_beacon_empty_ssid() {
        let bssid = [0; 6];
        let frame = build_beacon_frame(8, bssid, b"", 6, 0x0001, None);

        let beacon = parse_beacon(&frame).expect("beacon parse");
        // Empty SSID tag with length 0
        assert!(beacon.ssid.is_none() || beacon.ssid.as_ref().unwrap().is_empty());
    }

    #[test]
    fn parse_beacon_with_wpa3_sae_rsn() {
        let bssid = [0x09, 0x08, 0x07, 0x06, 0x05, 0x04];
        let rsn = rsn_ie_wpa3_sae();
        let frame = build_beacon_frame(8, bssid, b"WPA3Net", 36, 0x0011, Some(&rsn));

        let beacon = parse_beacon(&frame).expect("beacon parse");
        let rsn = beacon.rsn.as_ref().expect("RSN IE");
        assert_eq!(rsn.akm_count, 1);
        assert_eq!(rsn.akms, vec![0x000FAC08]); // SAE
        assert_eq!(
            security_from_beacon(beacon.capability, beacon.rsn.as_ref()),
            Security::Wpa3Sae
        );
    }

    #[test]
    fn parse_beacon_truncated_rsn_ie() {
        let bssid = [0; 6];
        let mut frame = build_beacon_frame(8, bssid, b"Test", 6, 0x0011, None);
        // Add truncated RSN IE (tag 48, length 2, but only version)
        frame.push(0x30);
        frame.push(2);
        frame.extend_from_slice(&1u16.to_le_bytes());

        let beacon = parse_beacon(&frame).expect("beacon parse");
        let rsn = beacon.rsn.as_ref().expect("RSN present");
        assert_eq!(rsn.version, 1);
        assert_eq!(rsn.group_cipher, 0); // not enough data
    }

    // ── security_from_beacon tests ──────────────────────────

    #[test]
    fn security_open_no_rsn_no_privacy() {
        assert_eq!(security_from_beacon(0x0001, None), Security::Open);
    }

    #[test]
    fn security_wpa2_psk_from_rsn() {
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 1,
            akms: vec![0x000FAC02],
        };
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::Wpa2Psk);
    }

    #[test]
    fn security_wpa2_psk_via_akm_05() {
        // AKM type 5 (0x000FAC05) also maps to WPA2-PSK
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 1,
            akms: vec![0x000FAC05],
        };
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::Wpa2Psk);
    }

    #[test]
    fn security_wpa2_enterprise_from_rsn() {
        // AKM type 1 (802.1X) maps to Wpa2Psk (we don't distinguish enterprise)
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 1,
            akms: vec![0x000FAC01], // 802.1X
        };
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::Wpa2Psk);
    }

    #[test]
    fn security_wpa3_sae_from_rsn() {
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 1,
            akms: vec![0x000FAC08],
        };
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::Wpa3Sae);
    }

    #[test]
    fn security_wpa3_transition_prefers_psk() {
        // With both PSK and SAE, the first match wins (PSK → Wpa2Psk)
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 2,
            akms: vec![0x000FAC02, 0x000FAC08],
        };
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::Wpa2Psk);
    }

    #[test]
    fn security_wep_fallback_privacy_bit() {
        // No RSN IE but privacy bit set → WEP (or WPA, currently maps to WpaPsk)
        assert_eq!(security_from_beacon(0x0011, None), Security::WpaPsk);
    }

    #[test]
    fn security_open_with_rsn_no_matching_akm() {
        // RSN present but no recognized AKM → falls through to privacy check
        let rsn = RsnInfo {
            version: 1,
            group_cipher: 0x000FAC04,
            pair_cipher_count: 1,
            pair_ciphers: vec![0x000FAC04],
            akm_count: 1,
            akms: vec![0x000FAC09], // unknown AKM
        };
        // Privacy bit set → WpaPsk (fallback)
        assert_eq!(security_from_beacon(0x0011, Some(&rsn)), Security::WpaPsk);
    }

    // ── Frame builder tests ─────────────────────────────────

    #[test]
    fn build_probe_request_wildcard() {
        let frame = build_probe_request(None);
        // FC: management, subtype 4
        assert_eq!(frame[0], 0x40);
        assert_eq!(frame[1], 0x00);
        // Addr1: broadcast
        assert_eq!(&frame[4..10], &[0xFF; 6]);
        // SSID IE: wildcard (length 0)
        assert_eq!(frame[24], 0x00);
        assert_eq!(frame[25], 0x00);
        // Rates IE present
        assert_eq!(frame[26], 0x01);
    }

    #[test]
    fn build_probe_request_with_ssid() {
        let ssid = Ssid::new(b"TestAP");
        let frame = build_probe_request(Some(&ssid));
        assert_eq!(frame[0], 0x40);
        // SSID IE
        assert_eq!(frame[24], 0x00); // tag 0
        assert_eq!(frame[25], 6); // length
        assert_eq!(&frame[26..32], b"TestAP");
    }

    #[test]
    fn build_auth_frame_layout() {
        let bssid = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let client = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let frame = build_auth_frame(bssid, client, 1);

        // FC: management, subtype 11 (auth)
        assert_eq!(frame[0], 0xB0);
        // Addr1 = BSSID
        assert_eq!(&frame[4..10], &bssid);
        // Addr2 = client
        assert_eq!(&frame[10..16], &client);
        // Addr3 = BSSID
        assert_eq!(&frame[16..22], &bssid);
        // Auth algorithm = 0 (open)
        assert_eq!(&frame[24..26], &[0x00, 0x00]);
        // Auth seq = 1 (LE)
        assert_eq!(&frame[26..28], &[0x01, 0x00]);
        // Status = 0
        assert_eq!(&frame[28..30], &[0x00, 0x00]);
    }

    #[test]
    fn build_assoc_request_open() {
        let bssid = [0x11; 6];
        let client = [0x22; 6];
        let ssid = Ssid::new(b"OpenAP");
        let frame = build_assoc_request(bssid, client, &ssid);

        // FC: management, subtype 0 (assoc request)
        assert_eq!(frame[0], 0x00);
        // Addr1 = BSSID
        assert_eq!(&frame[4..10], &bssid);
        // Capability: ESS=1, no privacy
        assert_eq!(frame[24], 0x01);
        assert_eq!(frame[25], 0x00);
        // Listen interval = 10
        assert_eq!(&frame[26..28], &[0x0A, 0x00]);
        // SSID IE
        assert_eq!(frame[28], 0x00);
        assert_eq!(frame[29], 6); // "OpenAP" = 6 bytes
        assert_eq!(&frame[30..36], b"OpenAP");
    }

    #[test]
    fn build_assoc_request_with_wpa2() {
        let bssid = [0x33; 6];
        let client = [0x44; 6];
        let ssid = Ssid::new(b"SecureAP");
        let frame = build_assoc_request_with_security(bssid, client, &ssid, true);

        // Capability: ESS=1 + privacy=1
        assert_eq!(frame[24], 0x01 | 0x10);
        assert_eq!(frame[25], 0x00);

        // Find RSN IE after SSID + rates
        let mut pos = 28; // after capability + listen_interval
        // Skip SSID IE
        pos += 2 + frame[pos + 1] as usize;
        // Skip Rates IE
        pos += 2 + frame[pos + 1] as usize;

        // Now at RSN IE
        assert_eq!(frame[pos], 0x30); // RSN element ID
        assert_eq!(frame[pos + 1], 20); // RSN length
        assert_eq!(&frame[pos + 2..pos + 4], &[0x01, 0x00]); // version
        assert_eq!(&frame[pos + 4..pos + 8], &[0x00, 0x0F, 0xAC, 0x04]); // CCMP
    }

    #[test]
    fn build_deauth_frame() {
        let bssid = [0x55; 6];
        let client = [0x66; 6];
        let frame = build_deauth(bssid, client, 7);

        // FC: management, subtype 12 (deauth)
        assert_eq!(frame[0], 0xC0);
        // Addr1 = BSSID
        assert_eq!(&frame[4..10], &bssid);
        // Addr2 = client
        assert_eq!(&frame[10..16], &client);
        // Reason code = 7 (LE)
        assert_eq!(&frame[24..26], &[0x07, 0x00]);
    }

    // ── WifiConnection state machine tests ──────────────────

    #[test]
    fn connection_state_machine() {
        let mut conn = WifiConnection::new();
        assert_eq!(conn.status, WifiStatus::Disconnected);
        assert!(!conn.is_connected());

        // Scan
        conn.start_scan();
        assert_eq!(conn.status, WifiStatus::Scanning);
        assert!(conn.scan_results.is_empty());

        // Add results
        conn.add_scan_result(AccessPoint {
            ssid: Ssid::new(b"AP1"),
            bssid: [1; 6],
            channel: 6,
            rssi: -50,
            security: Security::Open,
            beacon_interval: 100,
            beacon_timestamp: 0,
            device_timestamp: 0,
            dtim_count: 0,
            dtim_period: 0,
        });
        conn.add_scan_result(AccessPoint {
            ssid: Ssid::new(b"AP2"),
            bssid: [2; 6],
            channel: 11,
            rssi: -60,
            security: Security::Wpa2Psk,
            beacon_interval: 100,
            beacon_timestamp: 0,
            device_timestamp: 0,
            dtim_count: 0,
            dtim_period: 0,
        });
        assert_eq!(conn.scan_results.len(), 2);

        // Duplicate BSSID should be deduped
        conn.add_scan_result(AccessPoint {
            ssid: Ssid::new(b"AP1-dup"),
            bssid: [1; 6],
            channel: 6,
            rssi: -40,
            security: Security::Open,
            beacon_interval: 100,
            beacon_timestamp: 0,
            device_timestamp: 0,
            dtim_count: 0,
            dtim_period: 0,
        });
        assert_eq!(conn.scan_results.len(), 2);

        // Finish scan
        conn.finish_scan();
        assert_eq!(conn.status, WifiStatus::Disconnected);

        // Connect
        let ssid = Ssid::new(b"AP2");
        conn.connect(&ssid, Some("password"));
        assert_eq!(conn.status, WifiStatus::Authenticating);
        assert_eq!(conn.current_ssid.as_ref().unwrap().as_str(), "AP2");
        assert!(conn.password.is_some());

        // Disconnect
        conn.disconnect();
        assert_eq!(conn.status, WifiStatus::Disconnected);
        assert!(conn.current_ssid.is_none());
        assert!(conn.current_bssid.is_none());
    }

    // ── FrameType / MgmtSubtype tests ───────────────────────

    #[test]
    fn frame_type_from_u8() {
        assert_eq!(FrameType::from_u8(0), Some(FrameType::Management));
        assert_eq!(FrameType::from_u8(1), Some(FrameType::Control));
        assert_eq!(FrameType::from_u8(2), Some(FrameType::Data));
        assert_eq!(FrameType::from_u8(3), None);
    }

    #[test]
    fn mgmt_subtype_from_u8() {
        assert_eq!(MgmtSubtype::from_u8(8), Some(MgmtSubtype::Beacon));
        assert_eq!(MgmtSubtype::from_u8(11), Some(MgmtSubtype::Authentication));
        assert_eq!(
            MgmtSubtype::from_u8(12),
            Some(MgmtSubtype::Deauthentication)
        );
        assert_eq!(MgmtSubtype::from_u8(14), None);
    }

    #[test]
    fn security_name_and_needs_password() {
        assert_eq!(Security::Open.name(), "Open");
        assert!(!Security::Open.needs_password());
        assert_eq!(Security::Wpa2Psk.name(), "WPA2-PSK");
        assert!(Security::Wpa2Psk.needs_password());
        assert_eq!(Security::Wpa3Sae.name(), "WPA3-SAE");
        assert!(Security::Wpa3Sae.needs_password());
        assert!(Security::Wep.needs_password());
    }
}
