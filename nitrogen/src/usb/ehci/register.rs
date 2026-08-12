//! EHCI register context — structured MMIO access layer.

use crate::mmio::{MemRegion, SafeReadResult};
use crate::pci_health::PciHealth;

pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
pub const OP_USBINTR: usize = 0x08;
pub const OP_ASYNCLISTADDR: usize = 0x18;
pub const OP_PORTSC_BASE: usize = 0x44;

pub const USBCMD_RS: u32 = 1 << 0;
pub const USBCMD_HCRESET: u32 = 1 << 1;
pub const USBCMD_ASSE: u32 = 1 << 5;
pub const USBCMD_IAAD: u32 = 1 << 6;

pub const USBSTS_HCH: u32 = 1 << 0;
pub const USBSTS_PCD: u32 = 1 << 2;
pub const USBSTS_AAINT: u32 = 1 << 5;

pub const PORTSC_CCS: u32 = 1 << 0;
pub const PORTSC_PE: u32 = 1 << 2;
pub const PORTSC_RESET: u32 = 1 << 8;

// ══════════════════════════════════════════════════════════════

/// Minimal register backend used by controller state transitions.
pub trait RegisterBackend {
    fn read_register(&self, offset: usize) -> u32;
    fn write_register(&mut self, offset: usize, value: u32);
}

pub struct EhciOperationalRegisters {
    region: MemRegion,
    health: Option<PciHealth>,
}

fn checked_read(region: &MemRegion, off: usize, health: &PciHealth) -> SafeReadResult<u32> {
    crate::mmio::checked_read32_with_watchdog(region, off, health)
}

impl EhciOperationalRegisters {
    /// Create an operational-register view over a validated BAR sub-window.
    ///
    /// # Safety
    ///
    /// `base..base + size` must be a live, readable and writable MMIO mapping.
    pub unsafe fn new(base: usize, size: usize) -> Self {
        Self {
            region: unsafe { MemRegion::new(base, size) },
            health: None,
        }
    }

    /// Create an operational-register view with PCIe/MMIO hang protection.
    pub unsafe fn new_checked(base: usize, size: usize, health: PciHealth) -> Self {
        Self {
            region: unsafe { MemRegion::new(base, size) },
            health: Some(health),
        }
    }

    pub fn read(&self, off: usize) -> u32 {
        let value = match self.health.as_ref() {
            Some(health) => {
                let result = checked_read(&self.region, off, health);
                match result {
                    SafeReadResult::Value(value) => value,
                    SafeReadResult::DeviceGone => {
                        log::warn!(
                            "EHCI: PCI device disappeared during MMIO read at {:#x}",
                            off
                        );
                        u32::MAX
                    }
                    SafeReadResult::MasterAbort => {
                        log::warn!("EHCI: MMIO master abort at offset {:#x}", off);
                        u32::MAX
                    }
                }
            }
            None => self.region.read32(off),
        };
        if value == u32::MAX {
            log::warn!(
                "EHCI: MMIO read at offset {:#x} completed with all ones",
                off
            );
        }
        value
    }
    pub fn write(&self, off: usize, val: u32) {
        self.region.write32(off, val);
    }

    pub fn usbcmd(&self) -> u32 {
        self.read(OP_USBCMD)
    }
    pub fn set_usbcmd(&self, val: u32) {
        self.write(OP_USBCMD, val);
    }
    pub fn set_usbcmd_bits(&self, bits: u32) {
        self.write(OP_USBCMD, self.read(OP_USBCMD) | bits);
    }

    pub fn usbsts(&self) -> u32 {
        self.read(OP_USBSTS)
    }
    pub fn write_usbsts(&self, val: u32) {
        self.write(OP_USBSTS, val);
    }

    pub fn set_async_list_addr(&self, val: u32) {
        self.write(OP_ASYNCLISTADDR, val);
    }

    pub fn portsc(&self, port: u32) -> u32 {
        self.read(OP_PORTSC_BASE + port as usize * 4)
    }
    pub fn write_portsc(&self, port: u32, val: u32) {
        self.write(OP_PORTSC_BASE + port as usize * 4, val);
    }
}

impl RegisterBackend for EhciOperationalRegisters {
    fn read_register(&self, offset: usize) -> u32 {
        self.read(offset)
    }

    fn write_register(&mut self, offset: usize, value: u32) {
        self.write(offset, value);
    }
}

/// Pure state-machine façade shared by MMIO and fake register backends.
pub struct EhciStateMachine<'a, B: RegisterBackend> {
    backend: &'a mut B,
}

impl<'a, B: RegisterBackend> EhciStateMachine<'a, B> {
    pub fn new(backend: &'a mut B) -> Self {
        Self { backend }
    }

    pub fn request_reset(&mut self) {
        let command = self.backend.read_register(OP_USBCMD);
        self.backend
            .write_register(OP_USBCMD, command | USBCMD_HCRESET);
    }

    pub fn start_async_schedule(&mut self, list_address: u32) {
        self.backend.write_register(OP_ASYNCLISTADDR, list_address);
        let command = self.backend.read_register(OP_USBCMD);
        self.backend
            .write_register(OP_USBCMD, command | USBCMD_RS | USBCMD_ASSE);
    }
}

pub struct EhciRegisterContext {
    pub mmio_base: usize,
    pub caplength: u8,
    pub hcs_params: u32,
    pub op: EhciOperationalRegisters,
}

impl EhciRegisterContext {
    /// Read the EHCI capability header and construct its operational view.
    ///
    /// # Safety
    ///
    /// `mmio_base` must identify a live, readable and writable EHCI BAR
    /// mapping of at least `HOST_CONTROLLER_BAR_SIZE` bytes.
    pub unsafe fn new(mmio_base: usize, health: Option<PciHealth>) -> Option<Self> {
        crate::debug::hint(b"eh_cap");
        let region = unsafe { MemRegion::new(mmio_base, crate::usb::HOST_CONTROLLER_BAR_SIZE) };
        let header = match health.as_ref() {
            Some(health) => {
                let result = checked_read(&region, 0, health);
                match result {
                    SafeReadResult::Value(value) => value,
                    SafeReadResult::DeviceGone => {
                        log::warn!("EHCI: capability header read found device gone");
                        return None;
                    }
                    SafeReadResult::MasterAbort => {
                        log::warn!("EHCI: capability header read returned master abort");
                        return None;
                    }
                }
            }
            None => region.read32(0),
        };
        let caplength = header as u8;
        if header == u32::MAX || caplength < 0x10 || caplength & 3 != 0 {
            return None;
        }
        let hcs_params = match health.as_ref() {
            Some(health) => {
                let result = checked_read(&region, 4, health);
                match result {
                    SafeReadResult::Value(value) => value,
                    SafeReadResult::DeviceGone => {
                        log::warn!("EHCI: HCS_PARAMS read found device gone");
                        return None;
                    }
                    SafeReadResult::MasterAbort => {
                        log::warn!("EHCI: HCS_PARAMS read returned master abort");
                        return None;
                    }
                }
            }
            None => region.read32(4),
        };
        if hcs_params == u32::MAX || hcs_params & 0x0f == 0 {
            return None;
        }
        let op_base = mmio_base.checked_add(caplength as usize)?;
        let op_size = crate::usb::HOST_CONTROLLER_BAR_SIZE.checked_sub(caplength as usize)?;
        Some(Self {
            mmio_base,
            caplength,
            hcs_params,
            op: match health {
                Some(health) => unsafe {
                    EhciOperationalRegisters::new_checked(op_base, op_size, health)
                },
                None => unsafe { EhciOperationalRegisters::new(op_base, op_size) },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    #[derive(Default)]
    struct FakeRegisters {
        values: BTreeMap<usize, u32>,
    }

    impl RegisterBackend for FakeRegisters {
        fn read_register(&self, offset: usize) -> u32 {
            self.values.get(&offset).copied().unwrap_or(0)
        }

        fn write_register(&mut self, offset: usize, value: u32) {
            self.values.insert(offset, value);
        }
    }

    #[test]
    fn test_portsc_bits() {
        assert_eq!(PORTSC_CCS, 1);
        assert_eq!(PORTSC_PE, 4);
        assert_eq!(PORTSC_RESET, 256);
    }

    #[test]
    fn state_machine_resets_then_starts_async_schedule() {
        let mut registers = FakeRegisters::default();
        {
            let mut state = EhciStateMachine::new(&mut registers);
            state.request_reset();
        }
        assert_eq!(
            registers.read_register(OP_USBCMD) & USBCMD_HCRESET,
            USBCMD_HCRESET
        );

        registers.write_register(OP_USBCMD, 0);
        {
            let mut state = EhciStateMachine::new(&mut registers);
            state.start_async_schedule(0x2000);
        }
        assert_eq!(registers.read_register(OP_ASYNCLISTADDR), 0x2000);
        assert_eq!(
            registers.read_register(OP_USBCMD) & (USBCMD_RS | USBCMD_ASSE),
            USBCMD_RS | USBCMD_ASSE
        );
    }
}
