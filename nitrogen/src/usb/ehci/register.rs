//! EHCI register context — structured MMIO access layer.

use core::ptr;
use crate::mmio::detect_abort_read_u32;

pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
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

pub struct EhciOperationalRegisters(*mut u8);

impl EhciOperationalRegisters {
    pub unsafe fn new(base: *mut u8) -> Self {
        Self(base)
    }

    pub fn read(&self, off: usize) -> u32 {
        let p = unsafe { self.0.add(off) as *const u32 };
        match detect_abort_read_u32(p) {
            Some(v) => v,
            None => {
                log::warn!("EHCI: MMIO read at offset {:#x} returned 0xFFFF_FFFF (master abort)", off);
                0xFFFF_FFFF
            }
        }
    }
    pub fn write(&self, off: usize, val: u32) {
        unsafe {
            ptr::write_volatile(self.0.add(off) as *mut u32, val);
        }
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

pub struct EhciRegisterContext {
    pub mmio_base: *mut u8,
    pub caplength: u8,
    pub op: EhciOperationalRegisters,
}

impl EhciRegisterContext {
    pub unsafe fn new(mmio_base: *mut u8) -> Self {
        unsafe {
            crate::debug::hint(b"eh_cap");
            let caplength = ptr::read_volatile(mmio_base as *const u8);
            Self {
                mmio_base,
                caplength,
                op: EhciOperationalRegisters::new(mmio_base.add(caplength as usize)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_portsc_bits() {
        assert_eq!(PORTSC_CCS, 1);
        assert_eq!(PORTSC_PE, 4);
        assert_eq!(PORTSC_RESET, 256);
    }
}
