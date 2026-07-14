//! DHCP client implementation.
//!
//! Implements the four-message DHCP handshake:
//! DISCOVER → OFFER → REQUEST → ACK
//!
//! Builds on top of bonder's UDP/IPv4/Ethernet stack.

use alloc::vec::Vec;



/// DHCP magic cookie.
pub const DHCP_MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

/// DHCP message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DhcpMessageType {
    Discover = 1,
    Offer = 2,
    Request = 3,
    Decline = 4,
    Ack = 5,
    Nak = 6,
    Release = 7,
}

impl DhcpMessageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(DhcpMessageType::Discover),
            2 => Some(DhcpMessageType::Offer),
            3 => Some(DhcpMessageType::Request),
            4 => Some(DhcpMessageType::Decline),
            5 => Some(DhcpMessageType::Ack),
            6 => Some(DhcpMessageType::Nak),
            7 => Some(DhcpMessageType::Release),
            _ => None,
        }
    }
}

/// DHCP option codes.
pub const OPTION_SUBNET_MASK: u8 = 1;
pub const OPTION_ROUTER: u8 = 3;
pub const OPTION_DNS_SERVER: u8 = 6;
pub const OPTION_HOST_NAME: u8 = 12;
pub const OPTION_DOMAIN_NAME: u8 = 15;
pub const OPTION_BROADCAST_ADDR: u8 = 28;
pub const OPTION_NTP_SERVER: u8 = 42;
pub const OPTION_REQUESTED_IP: u8 = 50;
pub const OPTION_IP_LEASE_TIME: u8 = 51;
pub const OPTION_MESSAGE_TYPE: u8 = 53;
pub const OPTION_SERVER_ID: u8 = 54;
pub const OPTION_PARAM_LIST: u8 = 55;
pub const OPTION_CLIENT_ID: u8 = 61;
pub const OPTION_END: u8 = 255;

/// DHCP header (fixed part, 240 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct DhcpHeader {
    pub op: u8,
    pub htype: u8,
    pub hlen: u8,
    pub hops: u8,
    pub xid: [u8; 4],
    pub secs: [u8; 2],
    pub flags: [u8; 2],
    pub ciaddr: [u8; 4],
    pub yiaddr: [u8; 4],
    pub siaddr: [u8; 4],
    pub giaddr: [u8; 4],
    pub chaddr: [u8; 16],
    pub sname: [u8; 64],
    pub file: [u8; 128],
    pub magic: [u8; 4],
}

impl DhcpHeader {
    pub const SIZE: usize = 240;

    pub fn new(op: u8) -> Self {
        Self {
            op,
            htype: 1,  // Ethernet
            hlen: 6,   // MAC address length
            hops: 0,
            xid: [0u8; 4],
            secs: [0u8; 2],
            flags: [0x80, 0x00], // Broadcast flag
            ciaddr: [0u8; 4],
            yiaddr: [0u8; 4],
            siaddr: [0u8; 4],
            giaddr: [0u8; 4],
            chaddr: [0u8; 16],
            sname: [0u8; 64],
            file: [0u8; 128],
            magic: DHCP_MAGIC_COOKIE,
        }
    }
}

/// Parsed DHCP lease information.
#[derive(Debug, Clone, Default)]
pub struct DhcpLease {
    pub ip_address: [u8; 4],
    pub subnet_mask: [u8; 4],
    pub router: [u8; 4],
    pub dns_server: [u8; 4],
    pub server_id: [u8; 4],
    pub lease_time: u32,
}

impl DhcpLease {
    pub fn new() -> Self {
        Self::default()
    }
}

/// DHCP client state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpState {
    Initial,
    Selecting,
    Requesting,
    Bound,
    Renewing,
    Released,
}

/// DHCP client.
#[derive(Debug)]
pub struct DhcpClient {
    pub state: DhcpState,
    pub xid: u32,
    pub lease: DhcpLease,
    pub client_mac: [u8; 6],
    pub retries: u32,
    pub max_retries: u32,
}

impl DhcpClient {
    pub fn new(client_mac: [u8; 6]) -> Self {
        Self {
            state: DhcpState::Initial,
            xid: {
                #[cfg(target_arch = "x86_64")]
                {
                    (unsafe { core::arch::x86_64::_rdtsc() } & 0xFFFF_FFFF) as u32
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    0x12345678
                }
            },
            lease: DhcpLease::new(),
            client_mac,
            retries: 0,
            max_retries: 3,
        }
    }

    /// Build a DHCP Discover message payload (just the DHCP packet, no UDP/IP/Ethernet wrapping).
    pub fn build_discover(&mut self) -> Vec<u8> {
        self.state = DhcpState::Selecting;
        self.retries = 0;

        let mut header = DhcpHeader::new(1); // BOOTREQUEST
        header.xid = self.xid.to_be_bytes();

        let mut chaddr = [0u8; 16];
        chaddr[..6].copy_from_slice(&self.client_mac);
        header.chaddr = chaddr;

        let mut packet = Vec::new();
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &header as *const DhcpHeader as *const u8,
                DhcpHeader::SIZE,
            )
        };
        packet.extend_from_slice(header_bytes);

        // DHCP options
        packet.push(OPTION_MESSAGE_TYPE);
        packet.push(0x01);
        packet.push(DhcpMessageType::Discover as u8);

        packet.push(OPTION_PARAM_LIST);
        packet.push(0x04);
        packet.push(OPTION_SUBNET_MASK);
        packet.push(OPTION_ROUTER);
        packet.push(OPTION_DNS_SERVER);
        packet.push(OPTION_IP_LEASE_TIME);

        packet.push(OPTION_END);

        packet
    }

    /// Build a DHCP Request message.
    pub fn build_request(&mut self, offer_ip: [u8; 4], server_id: [u8; 4]) -> Vec<u8> {
        self.state = DhcpState::Requesting;

        let mut header = DhcpHeader::new(1);
        header.xid = self.xid.to_be_bytes();

        let mut chaddr = [0u8; 16];
        chaddr[..6].copy_from_slice(&self.client_mac);
        header.chaddr = chaddr;

        let mut packet = Vec::new();
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &header as *const DhcpHeader as *const u8,
                DhcpHeader::SIZE,
            )
        };
        packet.extend_from_slice(header_bytes);

        packet.push(OPTION_MESSAGE_TYPE);
        packet.push(0x01);
        packet.push(DhcpMessageType::Request as u8);

        packet.push(OPTION_REQUESTED_IP);
        packet.push(0x04);
        packet.extend_from_slice(&offer_ip);

        packet.push(OPTION_SERVER_ID);
        packet.push(0x04);
        packet.extend_from_slice(&server_id);

        packet.push(OPTION_HOST_NAME);
        let hostname = b"fullerene";
        packet.push(hostname.len() as u8);
        packet.extend_from_slice(hostname);

        packet.push(OPTION_END);

        packet
    }

    /// Build a DHCP Release message.
    pub fn build_release(&mut self) -> Vec<u8> {
        self.state = DhcpState::Released;

        let mut header = DhcpHeader::new(1);
        header.xid = self.xid.to_be_bytes();
        header.ciaddr = self.lease.ip_address;

        let mut chaddr = [0u8; 16];
        chaddr[..6].copy_from_slice(&self.client_mac);
        header.chaddr = chaddr;

        let mut packet = Vec::new();
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &header as *const DhcpHeader as *const u8,
                DhcpHeader::SIZE,
            )
        };
        packet.extend_from_slice(header_bytes);

        packet.push(OPTION_MESSAGE_TYPE);
        packet.push(0x01);
        packet.push(DhcpMessageType::Release as u8);

        packet.push(OPTION_SERVER_ID);
        packet.push(0x04);
        packet.extend_from_slice(&self.lease.server_id);

        packet.push(OPTION_END);

        packet
    }

    /// Parse a DHCP response.
    pub fn parse_response(&mut self, data: &[u8]) -> Result<DhcpMessageType, &'static str> {
        if data.len() < DhcpHeader::SIZE + 4 {
            return Err("Response too short");
        }

        let header = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const DhcpHeader) };
        let magic = header.magic;
        if magic != DHCP_MAGIC_COOKIE {
            return Err("Invalid magic cookie");
        }
        let op = header.op;
        if op != 2 {
            return Err("Not a BOOTREPLY");
        }
        let xid = header.xid;
        if xid != self.xid.to_be_bytes() {
            return Err("Transaction ID mismatch");
        }

        let mut offset = DhcpHeader::SIZE;
        let mut msg_type = None;

        while offset < data.len() {
            let opt = data[offset];
            if opt == OPTION_END {
                break;
            }
            if opt == 0 {
                offset += 1;
                continue;
            }

            if offset + 1 >= data.len() {
                break;
            }
            let opt_len = data[offset + 1] as usize;
            if offset + 2 + opt_len > data.len() {
                break;
            }
            let opt_data = &data[offset + 2..offset + 2 + opt_len];

            match opt {
                OPTION_MESSAGE_TYPE => {
                    if opt_len >= 1 {
                        msg_type = DhcpMessageType::from_u8(opt_data[0]);
                    }
                }
                OPTION_SUBNET_MASK => {
                    if opt_len >= 4 {
                        self.lease.subnet_mask.copy_from_slice(&opt_data[..4]);
                    }
                }
                OPTION_ROUTER => {
                    if opt_len >= 4 {
                        self.lease.router.copy_from_slice(&opt_data[..4]);
                    }
                }
                OPTION_DNS_SERVER => {
                    if opt_len >= 4 {
                        self.lease.dns_server.copy_from_slice(&opt_data[..4]);
                    }
                }
                OPTION_SERVER_ID => {
                    if opt_len >= 4 {
                        self.lease.server_id.copy_from_slice(&opt_data[..4]);
                    }
                }
                OPTION_IP_LEASE_TIME if opt_len >= 4 => {
                    self.lease.lease_time = u32::from_be_bytes([
                        opt_data[0], opt_data[1], opt_data[2], opt_data[3],
                    ]);
                }
                _ => {}
            }

            offset += 2 + opt_len;
        }

        let msg_type = msg_type.ok_or("No DHCP message type option")?;

        match msg_type {
            DhcpMessageType::Offer | DhcpMessageType::Ack => {
                self.lease.ip_address = header.yiaddr;
                if msg_type == DhcpMessageType::Ack {
                    self.state = DhcpState::Bound;
                }
            }
            _ => {}
        }

        Ok(msg_type)
    }
}
