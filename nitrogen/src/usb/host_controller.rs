//! USB Host Controller abstraction — trait that hides EHCI/xHCI details.
//!
//! # Architecture
//!
//! ```text
//! USB Core (msd, hub, hid, ...)
//!         │
//! HostControllerContext (trait)
//!    ├── XhciContext
//!    ├── EhciContext
//!    ├── (future: OhciContext, UhciContext, ...)
//!    └── (future: DummyHostController for testing)
//! ```
//!
//! USB drivers only see [`HostControllerContext`]; they never touch
//! registers, TRBs, qTDs, or any controller-specific structures.

use crate::usb::{UsbDevice, UsbDirection, UsbSetupPacket};

// ============================================================================
//  HostController — the trait that all USB host controllers implement
// ============================================================================

/// Abstract interface for any USB host controller (xHCI, EHCI, OHCI, …).
///
/// USB stack code (hub, mass-storage, HID, …) works exclusively through
/// this trait.  Concrete implementations own all register, ring, and
/// descriptor details.
pub trait HostController {
    /// Initialise the controller: reset hardware, configure rings, start.
    /// Returns `Ok(())` on success.
    ///
    /// xHCI controllers handle register configuration in their own `init()`
    /// method. EHCI controllers only need reset() and start().
    fn initialize(&mut self) -> Result<(), &'static str> {
        self.reset()?;
        // Note: xHCI requires register and ring configuration between reset and start.
        // This is handled in XhciContext::init(), which is called by the concrete type.
        // EHCI can proceed directly to start().
        self.start()
    }

    /// Hardware reset (HCRST / HCRESET).
    fn reset(&mut self) -> Result<(), &'static str>;

    /// Start the controller schedule (run/stop bit).
    fn start(&mut self) -> Result<(), &'static str>;

    /// Scan all root-hub ports for newly-connected devices.
    /// Returns the number of new devices discovered during this call.
    fn poll_ports(&mut self) -> usize;

    /// Clear the device list and reset port-done flags (re-scan all ports).
    fn clear_devices(&mut self);

    /// Number of root-hub ports.
    fn n_ports(&self) -> u32;

    /// Immutable accessor for discovered devices.
    fn devices(&self) -> &[UsbDevice];

    /// Mutable accessor for discovered devices (e.g. to fill in descriptors).
    fn devices_mut(&mut self) -> &mut [UsbDevice];

    // ── Transfers ─────────────────────────────────────────────

    /// Perform a USB control transfer on the default control endpoint (EP0).
    ///
    /// `dev_addr` is the USB device address (1–127).
    /// On success returns the number of bytes transferred in the data phase.
    fn control_transfer(
        &mut self,
        dev_addr: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str>;

    /// Perform a USB bulk transfer.
    ///
    /// `endpoint` is the full endpoint address (bit 7 = direction).
    /// `mps` is the maximum packet size for this endpoint.
    fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        mps: u16,
    ) -> Result<usize, &'static str>;
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    // Trait-only module; no concrete tests yet.
    // Tests live in ehci::context and xhci::context modules.
}
