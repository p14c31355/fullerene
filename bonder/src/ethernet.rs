//! Ethernet frame layer.
//!
//! Supports Ethernet II frames (DIX). IEEE 802.3 LLC/SNAP is not supported.
//! MAC addresses are represented as `[u8; 6]`.

/// MAC address (EUI-48).
pub type MacAddress = [u8; 6];

/// EtherType value embedded in an Ethernet frame.
///
/// Values ≤ 1500 are interpreted as frame length (IEEE 802.3 compatibility mode).
/// Larger values serve as protocol identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum EtherType {
    /// IPv4 (0x0800)
    Ipv4 = 0x0800,
    /// ARP  (0x0806)
    Arp = 0x0806,
}

impl EtherType {
    /// Convert from a `u16`. Returns `None` for unknown values.
    pub const fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x0800 => Some(Self::Ipv4),
            0x0806 => Some(Self::Arp),
            _ => None,
        }
    }

    pub const fn to_u16(self) -> u16 {
        self as u16
    }
}

/// Ethernet II frame header.
///
/// Field order:
/// - dst_mac (6 bytes)
/// - src_mac (6 bytes)
/// - ethertype (2 bytes, big-endian)
///
/// Total: 14 bytes.
#[repr(C, packed)]
pub struct EthernetHeader {
    pub dst_mac: MacAddress,
    pub src_mac: MacAddress,
    /// network byte order (big-endian)
    pub ethertype: u16,
}

impl EthernetHeader {
    pub const SIZE: usize = 14;

    /// Serialize the header into a byte buffer.
    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..6].copy_from_slice(&self.dst_mac);
        buf[6..12].copy_from_slice(&self.src_mac);
        buf[12] = (self.ethertype >> 8) as u8;
        buf[13] = self.ethertype as u8;
    }
}

/// Build an Ethernet frame and write it into `dst`.
///
/// Returns the total byte count (Ethernet header + payload).
/// Returns `None` if `dst` is too small.
pub fn build_frame(
    dst_mac: MacAddress,
    src_mac: MacAddress,
    ethertype: EtherType,
    payload: &[u8],
    dst: &mut [u8],
) -> Option<usize> {
    let total = EthernetHeader::SIZE + payload.len();
    if dst.len() < total {
        return None;
    }
    dst[0..6].copy_from_slice(&dst_mac);
    dst[6..12].copy_from_slice(&src_mac);
    let et = ethertype.to_u16();
    dst[12] = (et >> 8) as u8;
    dst[13] = et as u8;
    dst[14..total].copy_from_slice(payload);
    Some(total)
}
