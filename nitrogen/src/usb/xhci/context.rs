//! xHCI state ownership and public controller facade.
//!
//! [`XhciContext`] is the single owner of registers, DMA rings, device
//! contexts, ports, interrupts, and discovered devices. Operational methods
//! are grouped by lifecycle boundary in sibling modules:
//!
//! - `controller`: construction, reset/start, and root-port lifecycle
//! - `command`: command-ring submission and slot/endpoint configuration
//! - `event`: transfer-event waiting
//! - `transfer`: control and bulk transfers
//! - `resources`: slot release, deferred DMA cleanup, and teardown

use alloc::vec::Vec;

use super::device::DeviceContextSet;
use super::interrupt::InterruptContext;
use super::port::PortContext;
use super::register::{RegisterContext, USBSTS_HCH};
use super::ring::RingContext;
use crate::DriverContext;
use crate::pci_health::PciHealth;
use crate::usb::host_controller::HostController;
use crate::usb::{UsbDevice, UsbDirection, UsbSetupPacket};

/// Unified xHCI host controller state.
///
/// All xHCI state is owned by this struct. Sibling modules add focused
/// inherent methods without introducing additional owners or global state.
pub struct XhciContext {
    /// MMIO register access.
    pub registers: RegisterContext,
    /// Command and event rings.
    pub rings: RingContext,
    /// Device context (DCBAA, slots, scratchpad).
    pub device: DeviceContextSet,
    /// Port management.
    pub ports: PortContext,
    /// Interrupt configuration.
    pub interrupts: InterruptContext,
    /// Discovered USB devices.
    pub devices: Vec<UsbDevice>,
    /// Driver context for memory allocation.
    pub(super) driver_ctx: &'static dyn DriverContext,
    /// PCI health monitor used before MMIO transaction cycles.
    pub health: PciHealth,
    /// Whether legacy BIOS-to-OS handoff succeeded.
    pub legacy_handoff_done: bool,
    /// ERST physical address allocated during controller setup.
    pub(super) erst_phys: Option<u64>,
    /// DMA staging buffers that cannot be released until endpoint teardown.
    pub(super) deferred_free_list: Vec<(u64, usize)>,
}

// SAFETY: xHCI is used only on the main kernel thread (single-threaded kernel).
unsafe impl Send for XhciContext {}

impl XhciContext {
    /// Get a reference to the driver context.
    pub fn driver_ctx(&self) -> &dyn DriverContext {
        self.driver_ctx
    }

    /// Read an operational register.
    pub fn op_read(&self, offset: usize) -> u32 {
        self.registers.op.read(offset)
    }

    /// Write an operational register.
    pub fn op_write(&self, offset: usize, val: u32) {
        self.registers.op.write(offset, val);
    }

    /// Ring a doorbell.
    pub fn doorbell(&self, slot: u32, stream: u32) {
        self.registers.doorbell.ring(slot, stream);
    }

    /// Read the USBSTS register.
    pub fn usbsts(&self) -> u32 {
        self.registers.op.usbsts()
    }

    /// Check whether the controller is running.
    pub fn is_running(&self) -> bool {
        self.registers.op.usbsts() & USBSTS_HCH == 0
    }

    pub fn devices(&self) -> &[UsbDevice] {
        &self.devices
    }

    pub fn devices_mut(&mut self) -> &mut [UsbDevice] {
        &mut self.devices
    }

    pub fn n_ports(&self) -> u32 {
        self.ports.n_ports
    }

    pub fn ports_done_mask(&self) -> u32 {
        self.ports.done_mask()
    }

    pub fn max_slots(&self) -> u32 {
        self.device.slots.max_slots
    }

    pub fn ppc_enabled(&self) -> bool {
        self.ports.ppc
    }

    pub fn legacy_handoff_done(&self) -> bool {
        self.legacy_handoff_done
    }

    pub fn read_cap(&self, offset: u32) -> u32 {
        self.registers.op.read(offset as usize)
    }

    pub fn read_op_reg(&self, offset: u32) -> u32 {
        self.registers.op.read(offset as usize)
    }

    pub fn read_portsc(&self, port: u32) -> u32 {
        self.registers.op.portsc(port).0
    }

    pub fn clear_devices(&mut self) {
        self.ports.clear_done_flags();
        self.devices.clear();
    }
}

impl HostController for XhciContext {
    fn reset(&mut self) -> Result<(), crate::DriverError> {
        XhciContext::reset(self)
    }

    fn start(&mut self) -> Result<(), crate::DriverError> {
        XhciContext::start(self)
    }

    fn poll_ports(&mut self) -> usize {
        XhciContext::poll_ports(self)
    }

    fn clear_devices(&mut self) {
        XhciContext::clear_devices(self)
    }

    fn n_ports(&self) -> u32 {
        XhciContext::n_ports(self)
    }

    fn devices(&self) -> &[UsbDevice] {
        XhciContext::devices(self)
    }

    fn devices_mut(&mut self) -> &mut [UsbDevice] {
        XhciContext::devices_mut(self)
    }

    fn control_transfer(
        &mut self,
        dev_addr: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, crate::DriverError> {
        XhciContext::control_transfer(self, dev_addr as u32, setup, buf)
    }

    fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        mps: u16,
    ) -> Result<usize, crate::DriverError> {
        XhciContext::bulk_transfer(self, dev_addr as u32, endpoint, buf, dir, mps)
    }
}
