//! IPv4 packet layer.
//!
//! - IPv4 header construction (IHL=5, Flags=0, FragOff=0, TTL=64)
//! - IP header checksum (RFC 791)

/// IPv4 protocol number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IpProtocol {
    Icmp = 1,
    Tcp = 6,
    Udp = 17,
}

impl IpProtocol {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Icmp),
            6 => Some(Self::Tcp),
            17 => Some(Self::Udp),
            _ => None,
        }
    }

    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// IPv4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self([a, b, c, d])
    }

    /// Parse a dotted-decimal string (e.g. `"192.168.1.10"`).
    pub fn parse(s: &str) -> Option<Self> {
        let parts: [u8; 4] = {
            let mut it = s.split('.');
            [
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
            ]
        };
        if s.split('.').count() != 4 {
            return None;
        }
        Some(Self(parts))
    }
}

/// IPv4 header.
///
/// IHL=5 (no options, fixed 20 bytes).
#[repr(C, packed)]
pub struct Ipv4Header {
    /// version(4) | IHL(4) → 0x45
    pub ver_ihl: u8,
    /// DSCP(6) | ECN(2) → always 0
    pub dscp_ecn: u8,
    /// Total length of the IPv4 packet in octets (big-endian)
    pub total_length: u16,
    /// Identification (for fragment reassembly)
    pub identification: u16,
    /// Flags(3) | Fragment Offset(13) → always 0
    pub flags_frag: u16,
    /// Time to Live
    pub ttl: u8,
    /// Upper-layer protocol number
    pub protocol: u8,
    /// Header checksum (filled after computation)
    pub header_checksum: u16,
    /// Source IP
    pub src_addr: [u8; 4],
    /// Destination IP
    pub dst_addr: [u8; 4],
}

impl Ipv4Header {
    pub const SIZE: usize = 20;

    /// Create a populated header. Checksum is not yet computed.
    pub fn new(
        total_length: u16,
        identification: u16,
        ttl: u8,
        protocol: IpProtocol,
        src: Ipv4Addr,
        dst: Ipv4Addr,
    ) -> Self {
        Self {
            ver_ihl: 0x45,
            dscp_ecn: 0,
            total_length: total_length.to_be(),
            identification: identification.to_be(),
            flags_frag: 0,
            ttl,
            protocol: protocol.to_u8(),
            header_checksum: 0,
            src_addr: src.0,
            dst_addr: dst.0,
        }
    }

    /// Serialize the header into a byte buffer (checksum must already be computed).
    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.ver_ihl;
        buf[1] = self.dscp_ecn;
        buf[2..4].copy_from_slice(&self.total_length.to_be_bytes());
        buf[4..6].copy_from_slice(&self.identification.to_be_bytes());
        buf[6..8].copy_from_slice(&self.flags_frag.to_be_bytes());
        buf[8] = self.ttl;
        buf[9] = self.protocol;
        buf[10..12].copy_from_slice(&self.header_checksum.to_be_bytes());
        buf[12..16].copy_from_slice(&self.src_addr);
        buf[16..20].copy_from_slice(&self.dst_addr);
    }
}

/// IP checksum (16-bit one's complement sum).
///
/// `data` is treated as 16-bit boundaries. An odd-length tail is zero-padded.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    // Zero-pad the last byte if the length is odd
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    // Fold carry bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an IPv4 packet and write it into `dst`.
///
/// - `payload` is the UDP header + payload.
/// - `identification` is expected to be incremented by the host for each packet.
///
/// Returns the total byte count (IPv4 header + payload).
/// Returns `None` if `dst` is too small.
pub fn build_packet(
    src: Ipv4Addr,
    dst_addr: Ipv4Addr,
    protocol: IpProtocol,
    identification: u16,
    ttl: u8,
    payload: &[u8],
    dst: &mut [u8],
) -> Option<usize> {
    let total = Ipv4Header::SIZE + payload.len();
    if dst.len() < total || total > u16::MAX as usize {
        return None;
    }

    let mut hdr = Ipv4Header::new(total as u16, identification, ttl, protocol, src, dst_addr);

    // Serialize the header into a temporary buffer to compute the checksum
    let mut hdr_buf = [0u8; Ipv4Header::SIZE];
    hdr.write_to(&mut hdr_buf);
    // The checksum field is 0 at this point (as set by write_to)
    let cs = checksum(&hdr_buf);
    hdr.header_checksum = cs.to_be();

    // Write the final header into dst
    hdr.write_to(&mut dst[..Ipv4Header::SIZE]);
    dst[Ipv4Header::SIZE..total].copy_from_slice(payload);

    Some(total)
}
