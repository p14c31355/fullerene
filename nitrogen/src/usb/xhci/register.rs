//! xHCI register context — structured MMIO access layer.
//!
//! All raw reads/writes to xHCI MMIO registers are confined to
//! [`Mmio`], [`OperationalRegisters`], [`RuntimeRegisters`],
//! and [`DoorbellRegisters`].

use alloc::vec::Vec;
use core::ptr;

// ══════════════════════════════════════════════════════════════
//  Register Offsets
// ══════════════════════════════════════════════════════════════

pub const CAP_CAPLENGTH: usize = 0x00;
pub const CAP_HCSPARAMS1: usize = 0x04;
pub const CAP_HCSPARAMS2: usize = 0x08;
pub const CAP_HCSPARAMS3: usize = 0x0C;
pub const CAP_HCCPARAMS1: usize = 0x10;
pub const CAP_DBOFF: usize = 0x14;
pub const CAP_RTSOFF: usize = 0x18;

pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
pub const OP_PAGESIZE: usize = 0x08;
pub const OP_DNCTRL: usize = 0x14;
pub const OP_CRCR: usize = 0x18;
pub const OP_DCBAAP: usize = 0x30;
pub const OP_CONFIG: usize = 0x38;
pub const OP_PORTSC_BASE: usize = 0x400;

pub const RT_IMAN: usize = 0x00;
pub const RT_IMOD: usize = 0x04;
pub const RT_ERSTSZ: usize = 0x08;
pub const RT_ERSTBA: usize = 0x10;
pub const RT_ERDP: usize = 0x18;

pub const OP_PORTSC_STRIDE: usize = 0x10;
pub const RT_INTERRUPTER_STRIDE: usize = 0x20;

// ══════════════════════════════════════════════════════════════
//  Bit definitions
// ══════════════════════════════════════════════════════════════

pub const USBCMD_RS: u32 = 1 << 0;
pub const USBCMD_HCRST: u32 = 1 << 1;
pub const USBCMD_INTE: u32 = 1 << 2;
pub const USBCMD_HSEE: u32 = 1 << 3;

pub const USBSTS_HCH: u32 = 1 << 0;
pub const USBSTS_HSE: u32 = 1 << 2;
pub const USBSTS_EINT: u32 = 1 << 3;
pub const USBSTS_PCD: u32 = 1 << 4;
pub const USBSTS_SSS: u32 = 1 << 8;
pub const USBSTS_RSS: u32 = 1 << 9;
pub const USBSTS_SRE: u32 = 1 << 10;
pub const USBSTS_CNR: u32 = 1 << 11;
pub const USBSTS_HCE: u32 = 1 << 12;

pub const PORTSC_CCS: u32 = 1 << 0;
pub const PORTSC_PED: u32 = 1 << 1;
pub const PORTSC_OCA: u32 = 1 << 3;
pub const PORTSC_PR: u32 = 1 << 4;
pub const PORTSC_PLS_MASK: u32 = 0xF << 5;
pub const PORTSC_PP: u32 = 1 << 9;
pub const PORTSC_SPEED_MASK: u32 = 0xF << 10;
pub const PORTSC_PIC_MASK: u32 = 0x3 << 14;
pub const PORTSC_LWS: u32 = 1 << 16;
pub const PORTSC_CSC: u32 = 1 << 17;
pub const PORTSC_PEC: u32 = 1 << 18;
pub const PORTSC_WRC: u32 = 1 << 19;
pub const PORTSC_OCC: u32 = 1 << 20;
pub const PORTSC_PRC: u32 = 1 << 21;
pub const PORTSC_PLC: u32 = 1 << 22;
pub const PORTSC_CEC: u32 = 1 << 23;
pub const PORTSC_WPR: u32 = 1 << 31;
pub const PORTSC_RW1C_MASK: u32 =
    PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_OCC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC;

pub const IMAN_IP: u32 = 1 << 0;
pub const IMAN_IE: u32 = 1 << 1;

pub const CRCR_RCS: u32 = 1 << 0;
pub const CRCR_CS: u32 = 1 << 1;
pub const CRCR_CA: u32 = 1 << 2;
pub const CRCR_CRR: u32 = 1 << 3;

// ══════════════════════════════════════════════════════════════
//  Mmio — shared MMIO accessor
// ══════════════════════════════════════════════════════════════

struct Mmio(*mut u8);

impl Mmio {
    fn clflush(addr: *const u8) {
        unsafe { core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags)) }
    }

    fn read32(&self, off: usize) -> u32 {
        let p = unsafe { self.0.add(off) as *const u32 };
        Self::clflush(p as *const u8);
        unsafe { ptr::read_volatile(p) }
    }

    fn write32(&self, off: usize, val: u32) {
        let p = unsafe { self.0.add(off) as *mut u32 };
        unsafe { ptr::write_volatile(p, val) };
        Self::clflush(p as *const u8);
    }

    fn read64(&self, off: usize) -> u64 {
        (self.read32(off) as u64) | ((self.read32(off + 4) as u64) << 32)
    }

    fn write64(&self, off: usize, val: u64) {
        self.write32(off, val as u32);
        self.write32(off + 4, (val >> 32) as u32);
    }
}

// ══════════════════════════════════════════════════════════════
//  Macros for register accessors
// ══════════════════════════════════════════════════════════════

macro_rules! reg32 {
    ($rd:ident, $wr:ident, $off:expr) => {
        pub fn $rd(&self) -> u32 { self.0.read32($off) }
        pub fn $wr(&self, val: u32) { self.0.write32($off, val); }
    };
}
macro_rules! reg64 {
    ($rd:ident, $wr:ident, $off:expr) => {
        pub fn $rd(&self) -> u64 { self.0.read64($off) }
        pub fn $wr(&self, val: u64) { self.0.write64($off, val); }
    };
}

// ══════════════════════════════════════════════════════════════
//  CapabilityRegisters
// ══════════════════════════════════════════════════════════════

#[derive(Debug)]
pub struct CapabilityRegisters {
    pub caplength: u8,
    pub hci_version: u16,
    pub hcs_params1: u32,
    pub hcs_params2: u32,
    pub hcs_params3: u32,
    pub hcc_params1: u32,
    pub db_offset: u32,
    pub rt_offset: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct HcsParams1 {
    pub max_slots: u32,
    pub max_interrupters: u32,
    pub n_ports: u32,
    pub ppc: bool,
    pub csz: bool,
    pub max_scratchpad_bufs: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct HccParams1 {
    pub ac64: bool,
    pub bnc: bool,
    pub csz: bool,
    pub ppc: bool,
    pub pind: bool,
    pub lhrc: bool,
    pub ltc: bool,
    pub nss: bool,
    pub psc: bool,
    pub ext_cap_ptr: u16,
    pub max_psa_size: u32,
}

impl CapabilityRegisters {
    pub unsafe fn read(mmio: *mut u8) -> Self {
        unsafe {
            Self {
                caplength: ptr::read_volatile(mmio as *const u8),
                hci_version: ptr::read_volatile(mmio.add(0x02) as *const u16),
                hcs_params1: ptr::read_volatile(mmio.add(CAP_HCSPARAMS1) as *const u32),
                hcs_params2: ptr::read_volatile(mmio.add(CAP_HCSPARAMS2) as *const u32),
                hcs_params3: ptr::read_volatile(mmio.add(CAP_HCSPARAMS3) as *const u32),
                hcc_params1: ptr::read_volatile(mmio.add(CAP_HCCPARAMS1) as *const u32),
                db_offset: ptr::read_volatile(mmio.add(CAP_DBOFF) as *const u32) & 0xFFFF_FFFC,
                rt_offset: ptr::read_volatile(mmio.add(CAP_RTSOFF) as *const u32) & 0xFFFF_FFFC,
            }
        }
    }

    pub fn hcs_params1(&self) -> HcsParams1 {
        HcsParams1 {
            max_slots: self.hcs_params1 & 0xFF,
            max_interrupters: (self.hcs_params1 >> 8) & 0x7FF,
            n_ports: (self.hcs_params1 >> 24) & 0xFF,
            ppc: (self.hcc_params1 >> 3) & 1 != 0,
            csz: (self.hcc_params1 >> 2) & 1 != 0,
            max_scratchpad_bufs: (self.hcs_params2 >> 27) & 0x1F
                | ((self.hcs_params2 >> 21) & 0x1F) << 5,
        }
    }

    pub fn hcc_params1(&self) -> HccParams1 {
        let raw = self.hcc_params1;
        HccParams1 {
            ac64: raw & 1 != 0,
            bnc: (raw >> 1) & 1 != 0,
            csz: (raw >> 2) & 1 != 0,
            ppc: (raw >> 3) & 1 != 0,
            pind: (raw >> 4) & 1 != 0,
            lhrc: (raw >> 5) & 1 != 0,
            ltc: (raw >> 6) & 1 != 0,
            nss: (raw >> 7) & 1 != 0,
            psc: (raw >> 9) & 1 != 0,
            ext_cap_ptr: ((raw >> 16) & 0xFFFF) as u16,
            max_psa_size: (raw >> 12) & 0xF,
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  Register value wrappers
// ══════════════════════════════════════════════════════════════

pub struct PortSc(pub u32);
impl PortSc {
    pub fn ccs(&self) -> bool { self.0 & PORTSC_CCS != 0 }
    pub fn ped(&self) -> bool { self.0 & PORTSC_PED != 0 }
    pub fn pr(&self) -> bool { self.0 & PORTSC_PR != 0 }
    pub fn pp(&self) -> bool { self.0 & PORTSC_PP != 0 }
    pub fn pls(&self) -> u32 { (self.0 & PORTSC_PLS_MASK) >> 5 }
    pub fn speed(&self) -> u32 { (self.0 & PORTSC_SPEED_MASK) >> 10 }
    pub fn wpr(&self) -> bool { self.0 & PORTSC_WPR != 0 }
    pub fn csc(&self) -> bool { self.0 & PORTSC_CSC != 0 }
    pub fn pec(&self) -> bool { self.0 & PORTSC_PEC != 0 }
}

// ══════════════════════════════════════════════════════════════
//  OperationalRegisters
// ══════════════════════════════════════════════════════════════

pub struct OperationalRegisters(Mmio);

impl OperationalRegisters {
    pub unsafe fn new(base: *mut u8) -> Self {
        unsafe { Self(Mmio(base)) }
    }

    pub fn read(&self, off: usize) -> u32 { self.0.read32(off) }
    pub fn write(&self, off: usize, val: u32) { self.0.write32(off, val); }

    // ── Registers ─────────────────────────────────────────────────
    pub fn usbcmd(&self) -> u32 { self.0.read32(OP_USBCMD) }
    pub fn set_usbcmd(&self, val: u32) { self.0.write32(OP_USBCMD, val); }
    pub fn set_usbcmd_bits(&self, bits: u32) { self.0.write32(OP_USBCMD, self.0.read32(OP_USBCMD) | bits); }
    pub fn clear_usbcmd_bits(&self, bits: u32) { self.0.write32(OP_USBCMD, self.0.read32(OP_USBCMD) & !bits); }

    pub fn usbsts(&self) -> u32 { self.0.read32(OP_USBSTS) }
    pub fn clear_usbsts_bits(&self, bits: u32) { self.0.write32(OP_USBSTS, bits); }

    pub fn crcr(&self) -> u64 { self.0.read64(OP_CRCR) }
    pub fn set_crcr(&self, val: u64) { self.0.write64(OP_CRCR, val); }
    pub fn dcbaap(&self) -> u64 { self.0.read64(OP_DCBAAP) }
    pub fn set_dcbaap(&self, val: u64) { self.0.write64(OP_DCBAAP, val); }

    pub fn config(&self) -> u32 { self.0.read32(OP_CONFIG) }
    pub fn set_config(&self, val: u32) { self.0.write32(OP_CONFIG, val); }

    pub fn portsc(&self, port: u32) -> PortSc {
        PortSc(self.0.read32(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE))
    }
    pub fn write_portsc(&self, port: u32, val: u32) {
        self.0.write32(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE, val);
    }
    pub fn update_portsc(&self, port: u32, set: u32, clear: u32) {
        let off = OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE;
        self.0.write32(off, ((self.0.read32(off) & !clear) | set) & !PORTSC_RW1C_MASK);
    }
}

// ══════════════════════════════════════════════════════════════
//  RuntimeRegisters
// ══════════════════════════════════════════════════════════════

pub struct RuntimeRegisters(Mmio);

impl RuntimeRegisters {
    pub unsafe fn new(base: *mut u8) -> Self {
        Self(Mmio(base))
    }

    reg32!(iman, set_iman, RT_IMAN);
    reg32!(imod, set_imod, RT_IMOD);
    reg32!(erstsz, set_erstsz, RT_ERSTSZ);
    reg64!(erstba, set_erstba, RT_ERSTBA);
    reg64!(erdp, set_erdp, RT_ERDP);
}

// ══════════════════════════════════════════════════════════════
//  DoorbellRegisters
// ══════════════════════════════════════════════════════════════

pub struct DoorbellRegisters(Mmio);

impl DoorbellRegisters {
    pub unsafe fn new(base: *mut u8) -> Self {
        Self(Mmio(base))
    }

    pub fn ring(&self, slot: u32, stream: u32) {
        let off = slot as usize * 4;
        let val = (stream & 0xFF) | ((stream >> 8) & 0xFF) << 16;
        self.0.write32(off, val);
    }
}

// ══════════════════════════════════════════════════════════════
//  RegisterContext
// ══════════════════════════════════════════════════════════════

pub struct RegisterContext {
    pub mmio_base: *mut u8,
    pub cap: CapabilityRegisters,
    pub op: OperationalRegisters,
    pub runtime: RuntimeRegisters,
    pub doorbell: DoorbellRegisters,
}

impl RegisterContext {
    pub unsafe fn new(mmio_base: *mut u8) -> Self {
        unsafe {
            let cap = CapabilityRegisters::read(mmio_base);
            let caplen = cap.caplength as usize;
            let rt_off = cap.rt_offset as usize;
            let db_off = cap.db_offset as usize;
            Self {
                mmio_base,
                cap,
                op: OperationalRegisters::new(mmio_base.add(caplen)),
                runtime: RuntimeRegisters::new(mmio_base.add(rt_off)),
                doorbell: DoorbellRegisters::new(mmio_base.add(db_off)),
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  Legacy / Extended Capabilities
// ══════════════════════════════════════════════════════════════

pub fn dump_extended_capabilities(mmio_base: *mut u8, ext_cap_ptr: u16) {
    let m = Mmio(mmio_base);
    let mut off = ext_cap_ptr as usize;
    let mut iters = 0;
    while off != 0 && off < 0x100000 {
        iters += 1;
        if iters > 64 { log::warn!("xHCI: EC list exceeded max iterations"); break; }
        let ec_id = m.read32(off * 4) as u8;
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        let ec_dw1 = m.read32(off * 4 + 4);
        log::info!("xHCI EC: id={} next={} DWORD1=0x{:08X} (offset 0x{:04x})", ec_id, ec_next, ec_dw1, off * 4);
        if ec_id == 1 {
            let legsup = m.read32(off * 4);
            let legctl = m.read32(off * 4 + 4);
            log::info!("  → USB Legacy Support: BIOS_SEM={} OS_SEM={} SMI_en=0x{:03x}",
                (legsup >> 16) & 1, (legsup >> 24) & 1, legctl & 0x1F);
        } else if ec_id == 2 {
            let dw2 = m.read32(off * 4 + 8);
            let port_off = (dw2 & 0xFF) as u32;
            let port_cnt = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev = m.read32(off * 4) >> 24;
            log::info!("  → Supported Protocol: ports {}-{} rev={}.0 {}",
                port_off, port_off + port_cnt - 1, major_rev,
                if major_rev >= 3 { "USB 3.x" } else { "USB 2.0" });
        }
        if ec_next == 0 { break; }
        off += ec_next as usize;
    }
}

pub fn parse_port_protocols(mmio_base: *mut u8, ext_cap_ptr: u16, n_ports: u32) -> alloc::vec::Vec<u32> {
    let m = Mmio(mmio_base);
    let n_words = ((n_ports + 31) / 32).max(1) as usize;
    let mut bitmap = alloc::vec![0xFFFFFFFFu32; n_words];
    let mut off = ext_cap_ptr as usize;
    let mut iters = 0;
    while off != 0 && off < 0x100000 {
        iters += 1;
        if iters > 64 { log::warn!("xHCI: parse_port_protocols exceeded max iterations"); break; }
        let ec_id = m.read32(off * 4) as u8;
        if ec_id == 2 {
            let dw2 = m.read32(off * 4 + 8);
            let port_off = (dw2 & 0xFF) as u32;
            if port_off == 0 { let next = (m.read32(off * 4) >> 8) as u8; if next == 0 { break; } off += next as usize; continue; }
            let port_cnt = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev = m.read32(off * 4) >> 24;
            let is_usb3 = major_rev >= 3;
            for p in 0..port_cnt {
                let port_idx = port_off + p - 1;
                if port_idx < n_ports {
                    let word = (port_idx / 32) as usize;
                    let bit  = port_idx % 32;
                    if word < bitmap.len() {
                        if is_usb3 { bitmap[word] |= 1 << bit; } else { bitmap[word] &= !(1 << bit); }
                    }
                }
            }
        }
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        if ec_next == 0 { break; }
        off += ec_next as usize;
    }
    bitmap
}

pub fn try_legacy_handoff(mmio_base: *mut u8, ext_cap_ptr: u16) -> Result<bool, &'static str> {
    let m = Mmio(mmio_base);
    let mut off = ext_cap_ptr as usize;
    let mut iters = 0;
    while off != 0 && off < 0x100000 {
        iters += 1;
        if iters > 64 { return Err("circular capability list"); }
        let ec_id = m.read32(off * 4) as u8;
        if ec_id == 1 {
            let cap_base = off * 4;
            let legsup = m.read32(cap_base);
            if (legsup >> 16) & 1 == 0 {
                m.write32(cap_base + 4, m.read32(cap_base + 4) & !0x00F8001F);
                return Ok(true);
            }
            m.write32(cap_base, legsup | (1 << 24));
            let mut ok = false;
            for _ in 0..5_000_000 {
                if (m.read32(cap_base) & (1 << 16)) == 0 { ok = true; break; }
                core::hint::spin_loop();
            }
            if !ok { return Err("legacy handoff timed out"); }
            m.write32(cap_base + 4, m.read32(cap_base + 4) & !0x00F8001F);
            return Ok(false);
        }
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        if ec_next == 0 { break; }
        off += ec_next as usize;
    }
    Ok(true)
}

pub fn port_speed_to_usb(speed: u32) -> crate::usb::UsbSpeed {
    match speed {
        3 => crate::usb::UsbSpeed::High,
        2 => crate::usb::UsbSpeed::Low,
        1 => crate::usb::UsbSpeed::Full,
        4 | 5 => crate::usb::UsbSpeed::SuperSpeed,
        _ => crate::usb::UsbSpeed::High,
    }
}

// ══════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_portsc_bitfields() {
        let ps = PortSc(PORTSC_CCS | PORTSC_PP | 5 << 5);
        assert!(ps.ccs());
        assert!(ps.pp());
        assert_eq!(ps.pls(), 5);
        assert!(!ps.ped());
    }

    #[test]
    fn test_hcs_params1_parsing() {
        let cap = CapabilityRegisters {
            caplength: 0x20, hci_version: 0x0100,
            hcs_params1: 0x080000FF, hcs_params2: 0, hcs_params3: 0, hcc_params1: 0,
            db_offset: 0x1000, rt_offset: 0x2000,
        };
        let p = cap.hcs_params1();
        assert_eq!(p.max_slots, 255);
        assert_eq!(p.n_ports, 8);
    }
}
