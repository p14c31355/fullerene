//! UDP logger — `log` crate backend.
//!
//! Writing `log::info!("PCI scan started")` sends the log message as a
//! UDP packet automatically.
//!
//! ## Usage (kernel side)
//!
//! ```ignore
//! use bonder::logger::UdpLogger;
//! use bonder::NetDevice;  // VirtioNetDevice, etc.
//!
//! let dev = ...;  // &dyn NetDevice + Send
//! let logger = UdpLogger::new(
//!     dev,
//!     Ipv4Addr::new(192, 168, 1, 10),  // dev PC
//!     51400,                            // destination port
//!     Ipv4Addr::new(192, 168, 1, 100),  // self IP
//!     51401,                            // self port
//! );
//! log::set_max_level(log::LevelFilter::Info);
//! ```

use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicU16, Ordering};

use log::{LevelFilter, Log, Metadata, Record};
use spin::Mutex as SpinMutex;

use crate::NetDevice;
use crate::ethernet::{EtherType, MacAddress};
use crate::ipv4::{IpProtocol, Ipv4Addr};

/// Maximum log payload = Ethernet MTU (1500) minus header overhead.
const MAX_LOG_PAYLOAD: usize = 1500
    - crate::ethernet::EthernetHeader::SIZE
    - crate::ipv4::Ipv4Header::SIZE
    - crate::udp::UdpHeader::SIZE;

/// Maximum log message length. Longer messages are truncated.
const MAX_LOG_MESSAGE: usize = MAX_LOG_PAYLOAD - 64; // headroom for timestamp / level / newline

/// UDP logger.
///
/// Functions as a `log` crate backend, converting log messages to
/// UDP packet → IPv4 → Ethernet frame and sending them via a `NetDevice`.
pub struct UdpLogger<'a> {
    dev: &'a SpinMutex<dyn NetDevice + Send + 'a>,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    src_ip: Ipv4Addr,
    src_port: u16,
    /// Per-packet identification field increment
    ip_id: AtomicU16,
    /// Output buffer (MTU-sized)
    frame_buf: SpinMutex<[u8; 1500]>,
    /// String buffer
    msg_buf: SpinMutex<String>,
}

impl<'a> UdpLogger<'a> {
    /// Create a new UDP logger.
    ///
    /// - `dev` is a `&SpinMutex<dyn NetDevice + Send>`; the caller is expected
    ///   to wrap the device in a `SpinMutex`.
    /// - `dst_ip` / `dst_port` — the dev PC's receiving address.
    /// - `src_ip` / `src_port` — the logger's own address (Fullerene's IP).
    pub fn new(
        dev: &'a SpinMutex<dyn NetDevice + Send + 'a>,
        dst_ip: Ipv4Addr,
        dst_port: u16,
        src_ip: Ipv4Addr,
        src_port: u16,
    ) -> Self {
        Self {
            dev,
            dst_ip,
            dst_port,
            src_ip,
            src_port,
            ip_id: AtomicU16::new(1),
            frame_buf: SpinMutex::new([0u8; 1500]),
            msg_buf: SpinMutex::new(String::with_capacity(MAX_LOG_MESSAGE)),
        }
    }

    /// Send a single log entry.
    fn emit(&self, level: log::Level, msg: &str) {
        // Build the formatted string
        let mut msg_buf = self.msg_buf.lock();
        msg_buf.clear();
        // format: "[I] PCI scan started\n"
        let level_char = match level {
            log::Level::Error => 'E',
            log::Level::Warn => 'W',
            log::Level::Info => 'I',
            log::Level::Debug => 'D',
            log::Level::Trace => 'T',
        };

        // Truncate the message safely on UTF-8 character boundaries
        let max_payload = MAX_LOG_MESSAGE - 6; // "[X] \n" overhead
        let mut limit = max_payload;
        while limit > 0 && !msg.is_char_boundary(limit) {
            limit -= 1;
        }
        let content = &msg[..limit];

        // write! into a String never fails (infallible allocator)
        let _ = writeln!(msg_buf, "[{}] {}", level_char, content);

        // UDP payload
        let payload = msg_buf.as_bytes();

        // Acquire the frame buffer
        let mut frame = self.frame_buf.lock();

        // Layered construction:
        // 1. UDP
        let udp_buf = &mut frame[..];
        let udp_len = match crate::udp::build_datagram(
            self.src_ip,
            self.dst_ip,
            self.src_port,
            self.dst_port,
            payload,
            udp_buf,
        ) {
            Some(len) => len,
            None => return, // buffer too small (should never happen)
        };
        let udp_slice = &udp_buf[..udp_len];

        // Release the message buffer to keep the lock scope short
        drop(msg_buf);

        // 2. IPv4
        // The UDP datagram becomes the IPv4 payload
        // Rebuild into a separate buffer
        let mut ip_buf = [0u8; 1500];
        let ip_len = match crate::ipv4::build_packet(
            self.src_ip,
            self.dst_ip,
            IpProtocol::Udp,
            self.ip_id.fetch_add(1, Ordering::Relaxed),
            64, // TTL
            udp_slice,
            &mut ip_buf,
        ) {
            Some(len) => len,
            None => return,
        };
        let ip_slice = &ip_buf[..ip_len];

        // 3. Ethernet
        frame.fill(0);
        // Destination MAC: the dev PC's MAC.
        // ARP will provide the real MAC later; use broadcast for now.
        let dst_mac: MacAddress = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]; // broadcast
        let src_mac: MacAddress = {
            let dev = self.dev.lock();
            dev.mac_address()
        };

        let frame_len = match crate::ethernet::build_frame(
            dst_mac,
            src_mac,
            EtherType::Ipv4,
            ip_slice,
            &mut *frame,
        ) {
            Some(len) => len,
            None => return,
        };

        // Send
        let mut dev = self.dev.lock();
        let _ = dev.send_frame(&frame[..frame_len]);
    }
}

impl<'a> Log for UdpLogger<'a> {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // Format record.args() using core::fmt::Write
            let mut buf = String::with_capacity(MAX_LOG_MESSAGE);
            let _ = write!(buf, "{}", record.args());
            self.emit(record.level(), &buf);
        }
    }

    fn flush(&self) {
        // UDP is stateless; flush is a no-op
    }
}

/// Helper to install a `UdpLogger` as the global logger.
///
/// The `logger` must have a `'static` lifetime; the caller should use
/// `Box::leak` or a `static` variable for that purpose.
///
/// ```ignore
/// let dev = ...;  // SpinMutex<dyn NetDevice + Send>
/// let leaked_dev: &'static SpinMutex<dyn NetDevice + Send> = Box::leak(Box::new(dev));
/// let logger: &'static UdpLogger = Box::leak(Box::new(UdpLogger::new(...)));
/// bonder::logger::init(logger, log::LevelFilter::Info);
/// ```
pub fn init(logger: &'static dyn Log, max_level: LevelFilter) -> Result<(), log::SetLoggerError> {
    log::set_logger(logger)?;
    log::set_max_level(max_level);
    Ok(())
}
