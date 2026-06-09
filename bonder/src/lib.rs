//! # Bonder — Network Protocol Stack Sub-crate
//!
//! ## Architecture
//!
//! ```text
//! logger  (UdpLogger: log::Log impl)
//!   ↓
//! udp     (UDP header + pseudo-header checksum)
//!   ↓
//! ipv4    (IPv4 header + IP checksum)
//!   ↓
//! ethernet (Ethernet header)
//!   ↓
//! NetDevice trait → {VirtioNetDevice, E1000, LoopbackUdpDevice, ...}
//! ```
//!
//! Bonder does not control NICs directly.
//! Instead, it abstracts through the `NetDevice` trait; actual hardware operations are
//! handled by nitrogen.
//!
//! `no_std` compatible (depends on `alloc`).

#![no_std]

extern crate alloc;

pub mod ethernet;
pub mod ipv4;
pub mod udp;
pub mod logger;

/// Trait abstracting frame send/receive to a NIC.
///
/// Implementations:
/// - `nitrogen::virtio::net::VirtioNetDevice` (real virtio-net hardware)
/// - `LoopbackUdpDevice` in bonder's main (test helper using `std::net::UdpSocket`)
///
/// Uses poll semantics instead of blocking recv because VirtIO RX queues are
/// interrupt + poll hybrids. Returns `Ok(None)` when no packet is available.
pub trait NetDevice {
    /// Send an Ethernet frame to the NIC.
    ///
    /// `frame` contains the complete frame from Ethernet header to data.
    /// Returns `Ok(())` on success, `NetError` on failure.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError>;

    /// If a received frame is available, copy it into `buf` and return its byte count.
    /// Returns `Ok(None)` when no frame is available.
    ///
    /// Returns `NetError::BufferTooSmall` when `buf` is smaller than the frame.
    fn poll_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError>;

    /// The MAC address of this NIC.
    fn mac_address(&self) -> [u8; 6];
}

/// Errors produced by network operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Send failure (queue full, device not initialized, etc.)
    SendFailed,
    /// Receive buffer is too small
    BufferTooSmall,
    /// Frame exceeds MTU
    FrameTooLarge,
    /// Device not initialized
    NotInitialized,
    /// Invalid parameter (null MAC, unspecified IP, etc.)
    InvalidParameter,
}