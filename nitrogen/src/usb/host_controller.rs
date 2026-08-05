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
    fn initialize(&mut self) -> Result<(), crate::DriverError> {
        self.reset()?;
        // Note: xHCI requires register and ring configuration between reset and start.
        // This is handled in XhciContext::init(), which is called by the concrete type.
        // EHCI can proceed directly to start().
        self.start()
    }

    /// Hardware reset (HCRST / HCRESET).
    fn reset(&mut self) -> Result<(), crate::DriverError>;

    /// Start the controller schedule (run/stop bit).
    fn start(&mut self) -> Result<(), crate::DriverError>;

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
    ) -> Result<usize, crate::DriverError>;

    /// Update the default control endpoint's maximum packet size after the
    /// first eight bytes of the device descriptor have been read.
    ///
    /// EHCI keeps this in its existing control-transfer state, while xHCI
    /// must publish the new EP0 context with Evaluate Context.  The default
    /// implementation keeps older host-controller implementations compatible.
    fn set_control_max_packet_size(
        &mut self,
        _dev_addr: u8,
        _max_packet_size: u16,
    ) -> Result<(), crate::DriverError> {
        Ok(())
    }

    /// Whether the addressed device is operating at SuperSpeed. This lets
    /// descriptor parsing distinguish USB 3.x's exponent-encoded EP0 size
    /// from an invalid USB 2.x literal.
    fn is_super_speed(&self, _dev_addr: u8) -> bool {
        false
    }

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
    ) -> Result<usize, crate::DriverError>;
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    // Trait-only module; no concrete tests yet.
    // Tests live in ehci::context and xhci::context modules.
}
