//! Replay tests for the iwlwifi connection state machine.
//!
//! These tests inject synthetic 802.11 frames (beacons, auth responses,
//! assoc responses, EAPOL-Key messages, DHCP) directly into the RX processing
//! path and verify that the driver makes the correct state transitions and
//! builds the correct TX frames.

use super::device::IwlWifiDevice;
use super::types::*;
use alloc::vec;
use alloc::vec::Vec;
use bonder::wifi::{self, Bssid, Security, Ssid};
use bonder::wpa::{
    KEY_INFO_ACK, KEY_INFO_INSTALL, KEY_INFO_KEY_TYPE, KEY_INFO_MIC, KEY_INFO_PAIRWISE, WpaState,
};

const CLIENT_MAC: Bssid = [0x94, 0x65, 0x9C, 0x44, 0x73, 0xD4];
const AP_BSSID: Bssid = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
const SSID: &[u8] = b"TestAP";

// ── Frame builders ──────────────────────────────────────────────

fn build_beacon(
    bssid: Bssid,
    ssid: &[u8],
    channel: u8,
    capability: u16,
    rsn_ie: Option<&[u8]>,
) -> Vec<u8> {
    let mut f = Vec::new();
    f.push(0x80); // FC: management, beacon
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x00]); // duration
    f.extend_from_slice(&[0xFF; 6]); // addr1: broadcast
    f.extend_from_slice(&bssid); // addr2: SA
    f.extend_from_slice(&bssid); // addr3: BSSID
    f.extend_from_slice(&[0x10, 0x00]); // seq ctrl
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

fn rsn_ie_wpa2_psk() -> Vec<u8> {
    let mut ie = Vec::new();
    ie.push(0x30);
    ie.push(20);
    ie.extend_from_slice(&1u16.to_le_bytes());
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // CCMP
    ie.extend_from_slice(&1u16.to_le_bytes());
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
    ie.extend_from_slice(&1u16.to_le_bytes());
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]); // PSK
    ie.extend_from_slice(&0u16.to_le_bytes());
    ie
}

fn build_auth_response(bssid: Bssid, client: Bssid, status: u16) -> Vec<u8> {
    let mut f = Vec::new();
    f.push(0xB0); // FC: management, auth
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x00]);
    f.extend_from_slice(&client); // addr1: client
    f.extend_from_slice(&bssid); // addr2: AP
    f.extend_from_slice(&bssid); // addr3: BSSID
    f.extend_from_slice(&[0x00, 0x00]); // seq
    // Auth body
    f.extend_from_slice(&0u16.to_le_bytes()); // algorithm: open
    f.extend_from_slice(&2u16.to_le_bytes()); // transaction seq: 2
    f.extend_from_slice(&status.to_le_bytes());
    f
}

fn build_assoc_response(bssid: Bssid, client: Bssid, status: u16, aid: u16) -> Vec<u8> {
    let mut f = Vec::new();
    f.push(0x10); // FC: management, assoc response
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x00]);
    f.extend_from_slice(&client); // addr1: client
    f.extend_from_slice(&bssid); // addr2: AP
    f.extend_from_slice(&bssid); // addr3: BSSID
    f.extend_from_slice(&[0x00, 0x00]); // seq
    // Assoc body
    f.extend_from_slice(&0x0001u16.to_le_bytes()); // capability
    f.extend_from_slice(&status.to_le_bytes());
    f.extend_from_slice(&aid.to_le_bytes());
    // Supported rates IE
    f.push(0x01);
    f.push(0x04);
    f.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96]);
    f
}

fn wrap_eapol(pdu: &[u8]) -> Vec<u8> {
    let mut f = Vec::new();
    // 802.11 data frame, FromDS=1 (AP → station)
    f.push(0x08); // FC: data type
    f.push(0x02); // FromDS
    f.extend_from_slice(&[0x00, 0x00]); // duration
    f.extend_from_slice(&CLIENT_MAC); // addr1: DA (client)
    f.extend_from_slice(&AP_BSSID); // addr2: BSSID (AP)
    f.extend_from_slice(&AP_BSSID); // addr3: SA (AP)
    f.extend_from_slice(&[0x00, 0x00]); // seq
    // LLC/SNAP
    f.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8E]);
    // EAPOL PDU
    f.extend_from_slice(pdu);
    f
}

fn build_eapol_msg1(anonce: &[u8; 32], replay_counter: u64) -> Vec<u8> {
    let mut pdu = vec![0u8; 4 + 95];
    pdu[0] = 0x02;
    pdu[1] = 0x03;
    pdu[2..4].copy_from_slice(&95u16.to_be_bytes());
    pdu[4] = 0x02;
    let key_info = KEY_INFO_KEY_TYPE | KEY_INFO_PAIRWISE | KEY_INFO_ACK;
    pdu[5..7].copy_from_slice(&key_info.to_be_bytes());
    pdu[7..9].copy_from_slice(&0x0010u16.to_be_bytes());
    pdu[9..17].copy_from_slice(&replay_counter.to_be_bytes());
    pdu[17..49].copy_from_slice(anonce);
    wrap_eapol(&pdu)
}

fn build_eapol_msg3(
    anonce: &[u8; 32],
    replay_counter: u64,
    ptk: &[u8; 48],
    gtk: &[u8; 16],
    gtk_key_id: u8,
) -> Vec<u8> {
    let mut key_data = Vec::new();
    key_data.push(0xDD);
    key_data.push(22);
    key_data.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x01]);
    key_data.push(gtk_key_id);
    key_data.push(0x00);
    key_data.extend_from_slice(gtk);

    let key_data_len = key_data.len() as u16;
    let eapol_len = 95u16 + key_data_len;

    let mut pdu = Vec::new();
    pdu.push(0x03);
    pdu.push(0x03);
    pdu.extend_from_slice(&eapol_len.to_be_bytes());
    pdu.push(0x02);
    let key_info =
        KEY_INFO_KEY_TYPE | KEY_INFO_PAIRWISE | KEY_INFO_MIC | KEY_INFO_ACK | KEY_INFO_INSTALL;
    pdu.extend_from_slice(&key_info.to_be_bytes());
    pdu.extend_from_slice(&0x0010u16.to_be_bytes());
    pdu.extend_from_slice(&replay_counter.to_be_bytes());
    pdu.extend_from_slice(anonce);
    pdu.extend_from_slice(&[0u8; 16]); // IV
    pdu.extend_from_slice(&[0u8; 8]); // RSC
    pdu.extend_from_slice(&[0u8; 8]); // key ID
    pdu.extend_from_slice(&[0u8; 16]); // MIC placeholder
    pdu.extend_from_slice(&key_data_len.to_be_bytes());
    pdu.extend_from_slice(&key_data);

    // Compute and fill MIC over the EAPOL PDU
    let mic = bonder::wpa::compute_mic(ptk, &pdu);
    pdu[81..97].copy_from_slice(&mic);

    wrap_eapol(&pdu)
}

// ── Tests ───────────────────────────────────────────────────────

#[test]
fn beacon_replay_populates_scan_results() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let rsn = rsn_ie_wpa2_psk();
    let beacon = build_beacon(AP_BSSID, SSID, 11, 0x0011, Some(&rsn));
    dev.inject_rx_frame(&beacon);

    let aps = dev.scan_results.as_slice();
    assert_eq!(aps.len(), 1);
    assert_eq!(aps[0].ssid.as_str(), "TestAP");
    assert_eq!(aps[0].bssid, AP_BSSID);
    assert_eq!(aps[0].channel, 11);
    assert_eq!(aps[0].security, Security::Wpa2Psk);
}

#[test]
fn open_network_full_flow() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    // 1. Inject beacon (open network, no RSN)
    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);
    assert_eq!(dev.scan_results.len(), 1);
    assert_eq!(dev.scan_results[0].security, Security::Open);

    // 2. Connect to the AP
    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");
    assert_eq!(dev.iwl_state, IwlState::AuthSent);
    assert!(!dev.wpa_required);

    // Auth frame should have been sent via TX
    let auth_tx = dev.last_tx_frame();
    assert_eq!(auth_tx[0], 0xB0); // auth frame

    // 3. Inject auth response (success)
    let auth_resp = build_auth_response(AP_BSSID, CLIENT_MAC, 0);
    dev.inject_rx_frame(&auth_resp);
    assert_eq!(dev.iwl_state, IwlState::AssocSent);

    // Assoc request should have been sent
    let assoc_tx = dev.last_tx_frame();
    assert_eq!(assoc_tx[0], 0x00); // assoc request

    // 4. Inject assoc response (success, AID=1)
    let assoc_resp = build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1);
    dev.inject_rx_frame(&assoc_resp);
    assert_eq!(dev.iwl_state, IwlState::Connected);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Connected);
    assert!(!dev.wpa_required);

    // DHCP discover should have been sent
    let dhcp_tx = dev.last_tx_frame();
    assert_eq!(dhcp_tx[0] & 0x0C, 0x08); // data frame
}

#[test]
fn wpa2_full_handshake_flow() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    // 1. Inject WPA2 beacon
    let rsn = rsn_ie_wpa2_psk();
    let beacon = build_beacon(AP_BSSID, SSID, 11, 0x0011, Some(&rsn));
    dev.inject_rx_frame(&beacon);
    assert_eq!(dev.scan_results[0].security, Security::Wpa2Psk);

    // 2. Connect with password
    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, Some("password")).expect("connect");
    assert_eq!(dev.iwl_state, IwlState::AuthSent);
    assert!(dev.wpa_required);
    assert_eq!(dev.wpa.state, WpaState::WaitMsg1);

    // 3. Auth response
    let auth_resp = build_auth_response(AP_BSSID, CLIENT_MAC, 0);
    dev.inject_rx_frame(&auth_resp);
    assert_eq!(dev.iwl_state, IwlState::AssocSent);

    // 4. Assoc response → Handshake
    let assoc_resp = build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1);
    dev.inject_rx_frame(&assoc_resp);
    assert_eq!(dev.iwl_state, IwlState::Connected);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Handshake);
    assert!(dev.wpa_required);

    // 5. Inject EAPOL Message 1 → sends Message 2
    let known_anonce = [0xA5u8; 32];
    let msg1 = build_eapol_msg1(&known_anonce, 1);
    let tx_head_before = dev.tx_head;
    dev.inject_rx_frame(&msg1);
    assert_eq!(dev.wpa.state, WpaState::WaitMsg3);
    assert_ne!(dev.wpa.ptk, [0u8; 48]);

    // EAPOL Message 2 should have been sent
    assert!(dev.tx_head > tx_head_before);
    let msg2_tx = dev.last_tx_frame();
    assert_eq!(msg2_tx[0] & 0x0C, 0x08); // data frame
    // Check it's EAPOL (ether_type 0x888E at LLC/SNAP offset)
    assert_eq!(msg2_tx[30], 0x88);
    assert_eq!(msg2_tx[31], 0x8E);

    // 6. Inject EAPOL Message 3 → queues key commands + defers Message 4
    let known_gtk = [0x77u8; 16];
    let msg3 = build_eapol_msg3(&known_anonce, 2, &dev.wpa.ptk, &known_gtk, 1);
    let tx_head_before_keys = dev.tx_head;
    dev.inject_rx_frame(&msg3);
    assert_eq!(dev.wpa.state, WpaState::WaitMsg4);
    assert_eq!(dev.wpa.gtk[..16], known_gtk);

    // Two ADD_STA_KEY commands should have been queued
    assert_eq!(dev.tx_head, tx_head_before_keys + 2);
    assert!(dev.wpa_key_command_end.is_some());
    assert!(dev.pending_wpa_message4.is_some());

    // 7. Simulate firmware consuming the key commands
    dev.drain_tx();
    let tx_head_after_keys = dev.tx_head;
    dev.finish_wpa_for_test();

    // WPA should be complete
    assert!(dev.wpa_keys_installed);
    assert_eq!(dev.wpa.state, WpaState::Done);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Connected);

    // After finish: EAPOL Message 4 was sent, then DHCP discover.
    // tx_head should have advanced by 2 (msg4 + DHCP discover).
    assert_eq!(dev.tx_head, tx_head_after_keys + 2);

    // EAPOL Message 4 is at tx_head - 2
    let msg4_tx = dev.tx_frame_at(dev.tx_head - 2);
    assert_eq!(msg4_tx[0] & 0x0C, 0x08); // data frame
    assert_eq!(msg4_tx[30], 0x88);
    assert_eq!(msg4_tx[31], 0x8E);

    // DHCP discover is the last TX frame (tx_head - 1)
    let dhcp_tx = dev.last_tx_frame();
    assert_eq!(dhcp_tx[0] & 0x0C, 0x08); // data frame
    assert_eq!(dhcp_tx[30], 0x08); // ether_type 0x0800 (IP)
    assert_eq!(dhcp_tx[31], 0x00);

    // IP address not yet assigned (DHCP not completed)
    assert_eq!(dev.ip_address, [0u8; 4]);
}

#[test]
fn auth_failure_sets_error_status() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);

    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");

    // Inject auth failure (status=1)
    let auth_resp = build_auth_response(AP_BSSID, CLIENT_MAC, 1);
    dev.inject_rx_frame(&auth_resp);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Error);
}

#[test]
fn assoc_failure_sets_error_status() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);

    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");

    // Auth success
    let auth_resp = build_auth_response(AP_BSSID, CLIENT_MAC, 0);
    dev.inject_rx_frame(&auth_resp);
    assert_eq!(dev.iwl_state, IwlState::AssocSent);

    // Assoc failure (status=1)
    let assoc_resp = build_assoc_response(AP_BSSID, CLIENT_MAC, 1, 0);
    dev.inject_rx_frame(&assoc_resp);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Error);
}

#[test]
fn duplicate_beacons_deduped() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);
    dev.inject_rx_frame(&beacon);
    assert_eq!(dev.scan_results.len(), 1);
}

#[test]
fn deauth_resets_state() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);

    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");

    let auth_resp = build_auth_response(AP_BSSID, CLIENT_MAC, 0);
    dev.inject_rx_frame(&auth_resp);
    let assoc_resp = build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1);
    dev.inject_rx_frame(&assoc_resp);
    assert_eq!(dev.iwl_state, IwlState::Connected);

    // Inject deauth
    let mut deauth = Vec::new();
    deauth.push(0xC0); // FC: deauth
    deauth.push(0x00);
    deauth.extend_from_slice(&[0x00, 0x00]);
    deauth.extend_from_slice(&CLIENT_MAC);
    deauth.extend_from_slice(&AP_BSSID);
    deauth.extend_from_slice(&AP_BSSID);
    deauth.extend_from_slice(&[0x00, 0x00]);
    deauth.extend_from_slice(&7u16.to_le_bytes()); // reason
    dev.inject_rx_frame(&deauth);

    assert_eq!(dev.iwl_state, IwlState::Disconnected);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Disconnected);
}

#[test]
fn fiveghz_beacon_uses_phy_channel() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;
    dev.last_rx_phy_channel = 36;

    // 5GHz beacon: no DS Parameter Set IE
    let mut f = Vec::new();
    f.push(0x80); // FC: beacon
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x00]);
    f.extend_from_slice(&[0xFF; 6]);
    f.extend_from_slice(&AP_BSSID);
    f.extend_from_slice(&AP_BSSID);
    f.extend_from_slice(&[0x10, 0x00]);
    f.extend_from_slice(&0u64.to_le_bytes());
    f.extend_from_slice(&100u16.to_le_bytes());
    f.extend_from_slice(&0x0011u16.to_le_bytes()); // capability
    // SSID IE
    f.push(0x00);
    f.push(5);
    f.extend_from_slice(b"5GHz!");
    // Rates IE (no DS channel IE)
    f.push(0x01);
    f.push(0x04);
    f.extend_from_slice(&[0x0C, 0x12, 0x18, 0x24]);

    dev.inject_rx_frame(&f);
    assert_eq!(dev.scan_results.len(), 1);
    assert_eq!(dev.scan_results[0].channel, 36); // from last_rx_phy_channel
}

// ── DHCP frame builders ─────────────────────────────────────────

/// Build a raw DHCP packet (240-byte header + options) for a BOOTREPLY.
fn build_dhcp_packet(
    xid: u32,
    yiaddr: [u8; 4],
    server_ip: [u8; 4],
    client_mac: Bssid,
    msg_type: u8,
    options: &[u8],
) -> Vec<u8> {
    let mut p = vec![0u8; 240];
    p[0] = 0x02; // op = BOOTREPLY
    p[1] = 0x01; // htype = Ethernet
    p[2] = 0x06; // hlen = 6
    p[3] = 0x00; // hops
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[8..10].copy_from_slice(&[0x00, 0x00]); // secs
    p[10..12].copy_from_slice(&[0x00, 0x00]); // flags
    p[12..16].copy_from_slice(&[0; 4]); // ciaddr
    p[16..20].copy_from_slice(&yiaddr); // yiaddr
    p[20..24].copy_from_slice(&server_ip); // siaddr
    p[24..28].copy_from_slice(&[0; 4]); // giaddr
    p[28..34].copy_from_slice(&client_mac); // chaddr
    // sname (64 bytes) and file (128 bytes) already zero
    p[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]); // magic cookie

    // Options
    p.push(53); // Message Type
    p.push(1);
    p.push(msg_type);
    p.extend_from_slice(options);
    p.push(255); // END
    p
}

/// Wrap a raw DHCP packet in IP/UDP/LLC-SNAP/802.11 data frame (AP → station).
fn wrap_dhcp_response(dhcp: &[u8], server_ip: [u8; 4]) -> Vec<u8> {
    let dhcp_len = dhcp.len();
    let udp_len = 8 + dhcp_len;
    let ip_total_len = 20 + udp_len;

    // 802.11 data frame header (FromDS)
    let mut frame = Vec::new();
    frame.push(0x08); // FC: data type
    frame.push(0x02); // FromDS
    frame.extend_from_slice(&[0x00, 0x00]); // duration
    frame.extend_from_slice(&CLIENT_MAC); // addr1: DA (client)
    frame.extend_from_slice(&AP_BSSID); // addr2: BSSID
    frame.extend_from_slice(&AP_BSSID); // addr3: SA
    frame.extend_from_slice(&[0x00, 0x00]); // seq

    // LLC/SNAP
    frame.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00]);

    // IPv4 header (20 bytes)
    let ip_start = frame.len();
    frame.push(0x45); // ver=4, IHL=5
    frame.push(0x00); // DSCP/ECN
    frame.extend_from_slice(&(ip_total_len as u16).to_be_bytes());
    frame.extend_from_slice(&[0x00, 0x00]); // identification
    frame.extend_from_slice(&[0x00, 0x00]); // flags/frag
    frame.push(0x40); // TTL=64
    frame.push(0x11); // protocol=UDP
    frame.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
    frame.extend_from_slice(&server_ip); // src IP
    frame.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // dst IP (broadcast)

    // Compute IP checksum
    let checksum = ipv4_checksum(&frame[ip_start..ip_start + 20]);
    frame[ip_start + 10..ip_start + 12].copy_from_slice(&checksum.to_be_bytes());

    // UDP header (8 bytes)
    frame.extend_from_slice(&[0x00, 0x43]); // src port 67 (server)
    frame.extend_from_slice(&[0x00, 0x44]); // dst port 68 (client)
    frame.extend_from_slice(&(udp_len as u16).to_be_bytes());
    frame.extend_from_slice(&[0x00, 0x00]); // checksum = 0

    // DHCP payload
    frame.extend_from_slice(dhcp);
    frame
}

fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in header.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

// ── DHCP tests ──────────────────────────────────────────────────

#[test]
fn dhcp_full_flow_open_network() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    // 1. Beacon → scan result
    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);

    // 2. Connect (open)
    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");

    // 3. Auth response
    dev.inject_rx_frame(&build_auth_response(AP_BSSID, CLIENT_MAC, 0));
    assert_eq!(dev.iwl_state, IwlState::AssocSent);

    // 4. Assoc response → start_dhcp sends Discover
    dev.inject_rx_frame(&build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1));
    assert_eq!(dev.iwl_state, IwlState::Connected);
    assert!(dev.dhcp.is_some());

    // Read the xid so we can echo it in Offer/Ack
    assert!(dev.dhcp.is_some(), "DHCP client not initialized");
    let xid = dev.dhcp.as_ref().unwrap().xid;

    // 5. Inject DHCP Offer
    let server_ip = [192, 168, 1, 1];
    let offered_ip = [192, 168, 1, 100];
    let offer_options = {
        let mut opts = Vec::new();
        // Server ID (option 54)
        opts.extend_from_slice(&[54, 4]);
        opts.extend_from_slice(&server_ip);
        // Subnet mask (option 1)
        opts.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
        // Router (option 3)
        opts.extend_from_slice(&[3, 4]);
        opts.extend_from_slice(&server_ip);
        // DNS (option 6)
        opts.extend_from_slice(&[6, 4]);
        opts.extend_from_slice(&server_ip);
        // Lease time (option 51) = 86400 seconds
        opts.extend_from_slice(&[51, 4]);
        opts.extend_from_slice(&86400u32.to_be_bytes());
        opts
    };
    let offer = build_dhcp_packet(xid, offered_ip, server_ip, CLIENT_MAC, 2, &offer_options);
    let offer_frame = wrap_dhcp_response(&offer, server_ip);

    let tx_head_before_offer = dev.tx_head;
    dev.inject_rx_frame(&offer_frame);

    // DHCP Request should have been sent
    assert!(
        dev.tx_head > tx_head_before_offer,
        "tx_head didn't advance after Offer: {} -> {}",
        tx_head_before_offer,
        dev.tx_head
    );

    // 6. Inject DHCP ACK
    let ack = build_dhcp_packet(xid, offered_ip, server_ip, CLIENT_MAC, 5, &offer_options);
    let ack_frame = wrap_dhcp_response(&ack, server_ip);
    dev.inject_rx_frame(&ack_frame);

    // 7. Verify IP configuration
    assert_eq!(dev.ip_address, [192, 168, 1, 100]);
    assert_eq!(dev.subnet_mask, [255, 255, 255, 0]);
    assert_eq!(dev.gateway, [192, 168, 1, 1]);
    assert_eq!(dev.dns_server, [192, 168, 1, 1]);
}

#[test]
fn dhcp_full_flow_wpa2_network() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    // 1. WPA2 beacon
    let rsn = rsn_ie_wpa2_psk();
    let beacon = build_beacon(AP_BSSID, SSID, 11, 0x0011, Some(&rsn));
    dev.inject_rx_frame(&beacon);

    // 2. Connect with WPA2 password
    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, Some("password")).expect("connect");
    assert!(dev.wpa_required);

    // 3. Auth → Assoc → Handshake
    dev.inject_rx_frame(&build_auth_response(AP_BSSID, CLIENT_MAC, 0));
    dev.inject_rx_frame(&build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1));
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Handshake);

    // 4. EAPOL 4-way handshake
    let known_anonce = [0xA5u8; 32];
    dev.inject_rx_frame(&build_eapol_msg1(&known_anonce, 1));
    assert_eq!(dev.wpa.state, WpaState::WaitMsg3);

    let known_gtk = [0x77u8; 16];
    dev.inject_rx_frame(&build_eapol_msg3(
        &known_anonce,
        2,
        &dev.wpa.ptk,
        &known_gtk,
        1,
    ));
    assert_eq!(dev.wpa.state, WpaState::WaitMsg4);

    // 5. Complete WPA key installation
    dev.drain_tx();
    dev.finish_wpa_for_test();
    assert!(dev.wpa_keys_installed);
    assert_eq!(dev.wifi_conn.status, wifi::WifiStatus::Connected);

    // 6. DHCP flow
    let xid = dev.dhcp.as_ref().unwrap().xid;
    let server_ip = [10, 0, 0, 1];
    let offered_ip = [10, 0, 0, 42];
    let dhcp_opts = {
        let mut o = Vec::new();
        o.extend_from_slice(&[54, 4]);
        o.extend_from_slice(&server_ip);
        o.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
        o.extend_from_slice(&[3, 4]);
        o.extend_from_slice(&server_ip);
        o.extend_from_slice(&[6, 4]);
        o.extend_from_slice(&server_ip);
        o
    };

    // Offer → Request (inject as decrypted since WPA keys are installed)
    let offer = build_dhcp_packet(xid, offered_ip, server_ip, CLIENT_MAC, 2, &dhcp_opts);
    dev.inject_rx_frame_decrypted(&wrap_dhcp_response(&offer, server_ip));

    // ACK → IP assigned
    let ack = build_dhcp_packet(xid, offered_ip, server_ip, CLIENT_MAC, 5, &dhcp_opts);
    dev.inject_rx_frame_decrypted(&wrap_dhcp_response(&ack, server_ip));

    assert_eq!(dev.ip_address, [10, 0, 0, 42]);
    assert_eq!(dev.subnet_mask, [255, 255, 255, 0]);
    assert_eq!(dev.gateway, [10, 0, 0, 1]);
    assert_eq!(dev.dns_server, [10, 0, 0, 1]);
}

#[test]
fn dhcp_offer_with_wrong_xid_is_ignored() {
    let mut dev = IwlWifiDevice::new_for_test(CLIENT_MAC);
    dev.iwl_state = IwlState::Scanning;
    dev.scan_pending = true;

    let beacon = build_beacon(AP_BSSID, SSID, 6, 0x0001, None);
    dev.inject_rx_frame(&beacon);

    let ssid = Ssid::new(SSID);
    dev.connect(&ssid, None).expect("connect");
    dev.inject_rx_frame(&build_auth_response(AP_BSSID, CLIENT_MAC, 0));
    dev.inject_rx_frame(&build_assoc_response(AP_BSSID, CLIENT_MAC, 0, 1));

    let correct_xid = dev.dhcp.as_ref().unwrap().xid;
    let wrong_xid = correct_xid.wrapping_add(1);
    let server_ip = [192, 168, 1, 1];
    let offered_ip = [192, 168, 1, 100];
    let offer = build_dhcp_packet(wrong_xid, offered_ip, server_ip, CLIENT_MAC, 2, &[]);
    let tx_head_before = dev.tx_head;

    dev.inject_rx_frame(&wrap_dhcp_response(&offer, server_ip));

    // No DHCP Request should have been sent (xid mismatch)
    assert_eq!(dev.tx_head, tx_head_before);
    assert_eq!(dev.ip_address, [0u8; 4]);
}
