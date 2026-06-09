//! UDP datagram layer.
//!
//! - UDP header construction
//! - UDP pseudo-header checksum (RFC 768)
//!
//! A checksum of 0 indicates "checksum disabled", but this implementation always
//! computes one.

use crate::ipv4::{IpProtocol, Ipv4Addr};

/// UDP header (fixed 8 bytes).
#[repr(C, packed)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    /// Length of UDP header + payload in octets
    pub length: u16,
    /// Checksum (0 means disabled)
    pub checksum: u16,
}

impl UdpHeader {
    pub const SIZE: usize = 8;

    pub fn new(src_port: u16, dst_port: u16, payload_len: usize) -> Self {
        Self {
            src_port: src_port.to_be(),
            dst_port: dst_port.to_be(),
            length: (Self::SIZE + payload_len) as u16,
            checksum: 0,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        buf[4..6].copy_from_slice(&self.length.to_be_bytes());
        buf[6..8].copy_from_slice(&self.checksum.to_be_bytes());
    }
}

/// UDP pseudo-header (for checksum calculation, RFC 768).
///
/// Never transmitted on the wire.
struct PseudoHeader {
    src: [u8; 4],
    dst: [u8; 4],
    zero: u8,
    protocol: u8,
    udp_length: u16,
}

impl PseudoHeader {
    const SIZE: usize = 12;

    fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.src);
        buf[4..8].copy_from_slice(&self.dst);
        buf[8] = self.zero;
        buf[9] = self.protocol;
        buf[10..12].copy_from_slice(&self.udp_length.to_be_bytes());
    }
}

/// Compute the UDP checksum (RFC 768).
///
/// Applies the IP checksum algorithm over: pseudo-header + UDP header + payload.
/// Returns 0xFFFF when the result would be 0 (RFC 768: 0 means "checksum disabled").
fn udp_checksum(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> u16 {
    let total_len = UdpHeader::SIZE + payload.len();
    let pseudo = PseudoHeader {
        src: src.0,
        dst: dst.0,
        zero: 0,
        protocol: IpProtocol::Udp.to_u8(),
        udp_length: total_len as u16,
    };

    // Pseudo-header buffer
    let mut pseudo_buf = [0u8; PseudoHeader::SIZE];
    pseudo.write_to(&mut pseudo_buf);

    // UDP header buffer
    let mut hdr_buf = [0u8; UdpHeader::SIZE];
    let hdr = UdpHeader::new(src_port, dst_port, payload.len());
    hdr.write_to(&mut hdr_buf);

    // Concatenate: pseudo-header + UDP header + payload, then checksum
    let mut sum: u32 = 0;

    // Pseudo-header
    let mut i = 0;
    while i + 1 < pseudo_buf.len() {
        sum += u16::from_be_bytes([pseudo_buf[i], pseudo_buf[i + 1]]) as u32;
        i += 2;
    }

    // UDP header
    i = 0;
    while i + 1 < hdr_buf.len() {
        sum += u16::from_be_bytes([hdr_buf[i], hdr_buf[i + 1]]) as u32;
        i += 2;
    }

    // Payload
    i = 0;
    while i + 1 < payload.len() {
        sum += u16::from_be_bytes([payload[i], payload[i + 1]]) as u32;
        i += 2;
    }
    // Zero-pad the last byte if the length is odd
    if i < payload.len() {
        sum += (payload[i] as u32) << 8;
    }

    // Fold carry bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    let cs = !(sum as u16);
    if cs == 0 {
        0xFFFF
    } else {
        cs
    }
}

/// Build a UDP datagram and write it into `dst`.
///
/// Returns the total byte count (UDP header + payload).
/// Returns `None` if `dst` is too small.
pub fn build_datagram(
    src: Ipv4Addr,
    dst_addr: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
    dst: &mut [u8],
) -> Option<usize> {
    let total = UdpHeader::SIZE + payload.len();
    if dst.len() < total {
        return None;
    }

    let cs = udp_checksum(src, dst_addr, src_port, dst_port, payload);

    let mut hdr = UdpHeader::new(src_port, dst_port, payload.len());
    hdr.checksum = cs.to_be();
    hdr.write_to(&mut dst[..UdpHeader::SIZE]);
    dst[UdpHeader::SIZE..total].copy_from_slice(payload);

    Some(total)
}