//! Bonder CLI — UDP log sink test tool
//!
//! ## Receiver
//!
//! ```bash
//! nc -u -l 51400
//! ```
//!
//! ## Sender
//!
//! ```bash
//! cargo run -p bonder -- --target 127.0.0.1:51400
//! ```
//!
//! This tool sends raw Ethernet frames over UDP sockets.
//! The receiving side (`nc`) receives the raw Ethernet frames, so in practice
//! you'll want Wireshark or a custom parser for inspection.
//!
//! Pass `--raw` to skip the Ethernet/IP/UDP stack and send the log string
//! directly over UDP (simple mode).

use std::env;
use std::net::UdpSocket;

use bonder::ipv4::Ipv4Addr;
use bonder::logger::UdpLogger;
use bonder::{NetDevice, NetError};
use spin::Mutex as SpinMutex;

/// Test wrapper that implements `NetDevice` over `std::net::UdpSocket`.
///
/// Raw Ethernet frames are sent as UDP datagrams.
/// `poll_frame` always returns `Ok(None)` (receiving is not needed).
struct SocketDevice {
    sock: UdpSocket,
    mac: [u8; 6],
}

impl SocketDevice {
    fn new(target: &str) -> Result<Self, std::io::Error> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(target)?;
        Ok(Self {
            sock,
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], // dummy MAC
        })
    }
}

impl NetDevice for SocketDevice {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        self.sock.send(frame).map_err(|_| NetError::SendFailed)?;
        Ok(())
    }

    fn poll_frame(&mut self, _buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

/// Simple mode: skip Ethernet/IP/UDP layers and send only the log string over UDP.
struct RawUdpSender {
    sock: UdpSocket,
}

impl RawUdpSender {
    fn new(target: &str) -> Result<Self, std::io::Error> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(target)?;
        Ok(Self { sock })
    }
}

impl NetDevice for RawUdpSender {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        self.sock.send(frame).map_err(|_| NetError::SendFailed)?;
        Ok(())
    }

    fn poll_frame(&mut self, _buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        [0; 6]
    }
}

/// Send a raw Ethernet frame.
fn send_raw_frame(target: &str, message: &str) -> Result<(), String> {
    let dev = SocketDevice::new(target).map_err(|e| format!("connection failed: {e}"))?;
    let dev = SpinMutex::new(dev);

    // Leak for 'static lifetime
    let dev: &'static SpinMutex<SocketDevice> = Box::leak(Box::new(dev));

    // Source: 192.168.1.100 (arbitrary)
    let src_ip = Ipv4Addr::new(192, 168, 1, 100);
    let src_port = 51401u16;
    // Destination: 127.0.0.1:51400 (parsed from CLI argument)
    let (dst_ip_str, dst_port_str) = target.split_once(':').unwrap_or(("127.0.0.1", "51400"));
    let dst_ip = Ipv4Addr::parse(dst_ip_str).ok_or("failed to parse destination IP")?;
    let dst_port: u16 = dst_port_str.parse().map_err(|_| "failed to parse destination port")?;

    let logger = UdpLogger::new(dev, dst_ip, dst_port, src_ip, src_port);
    let logger: &'static UdpLogger = Box::leak(Box::new(logger));

    bonder::logger::init(logger, log::LevelFilter::Info)
        .map_err(|e| format!("logger init: {e}"))?;

    log::info!("{}", message);
    log::error!("test error: {}", message);
    log::warn!("test warn: {}", message);

    println!("Ethernet frame sent to {target} (check with nc -u -l {dst_port} or Wireshark)");
    Ok(())
}

/// Simple mode: send only the log string directly over UDP.
fn send_simple(target: &str, message: &str) -> Result<(), String> {
    let sender = RawUdpSender::new(target).map_err(|e| format!("connection failed: {e}"))?;
    let sender = SpinMutex::new(sender);
    let sender: &'static SpinMutex<RawUdpSender> = Box::leak(Box::new(sender));

    let src_ip = Ipv4Addr::new(192, 168, 1, 100);
    let src_port = 51401u16;
    let (dst_ip_str, dst_port_str) = target.split_once(':').unwrap_or(("127.0.0.1", "51400"));
    let dst_ip = Ipv4Addr::parse(dst_ip_str).ok_or("failed to parse destination IP")?;
    let dst_port: u16 = dst_port_str.parse().map_err(|_| "failed to parse destination port")?;

    let logger = UdpLogger::new(sender, dst_ip, dst_port, src_ip, src_port);
    let logger: &'static UdpLogger = Box::leak(Box::new(logger));

    bonder::logger::init(logger, log::LevelFilter::Info)
        .map_err(|e| format!("logger init: {e}"))?;

    log::info!("{}", message);
    log::error!("test error: {}", message);
    log::warn!("test warn: {}", message);

    println!("Log string sent to {target} (check with nc -u -l {dst_port})");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // Argument parsing
    let mut target: Option<String> = None;
    let mut message = String::from("Hello from bonder!");
    let mut raw_mode = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--target" | "-t" => {
                i += 1;
                if i < args.len() {
                    target = Some(args[i].clone());
                }
            }
            "--msg" | "-m" => {
                i += 1;
                if i < args.len() {
                    message = args[i].clone();
                }
            }
            "--raw" | "-r" => {
                raw_mode = true;
            }
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                if !other.starts_with('-') && target.is_none() {
                    target = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    let target = target.unwrap_or_else(|| "127.0.0.1:51400".to_string());

    let result = if raw_mode {
        send_simple(&target, &message)
    } else {
        send_raw_frame(&target, &message)
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    println!("Bonder CLI — UDP log sink test tool");
    println!();
    println!("Usage:");
    println!("  bonder [OPTIONS] [TARGET]");
    println!();
    println!("OPTIONS:");
    println!("  -t, --target ADDR   Destination address (default: 127.0.0.1:51400)");
    println!("  -m, --msg MESSAGE   Message to send (default: 'Hello from bonder!')");
    println!("  -r, --raw           Simple mode (send only the log string, no Ethernet frame)");
    println!("  -h, --help          Show this help");
    println!();
    println!("Examples:");
    println!("  # Start the receiver first");
    println!("  nc -u -l 51400");
    println!();
    println!("  # Send");
    println!("  cargo run -p bonder");
    println!("  cargo run -p bonder -- --target 192.168.1.10:51400 --msg 'PCI scan started'");
}