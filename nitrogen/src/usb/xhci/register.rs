//! xHCI register context — structured MMIO access layer.
//!
//! All raw reads/writes to xHCI MMIO registers are confined to
//! [`Mmio`], [`OperationalRegisters`], [`RuntimeRegisters`],
//! and [`DoorbellRegisters`].

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
        pub fn $rd(&self) -> u32 {
            self.0.read32($off)
        }
        pub fn $wr(&self, val: u32) {
            self.0.write32($off, val);
        }
    };
}
macro_rules! reg64 {
    ($rd:ident, $wr:ident, $off:expr) => {
        pub fn $rd(&self) -> u64 {
            self.0.read64($off)
        }
        pub fn $wr(&self, val: u64) {
            self.0.write64($off, val);
        }
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
    pub fn ccs(&self) -> bool {
        self.0 & PORTSC_CCS != 0
    }
    pub fn ped(&self) -> bool {
        self.0 & PORTSC_PED != 0
    }
    pub fn pr(&self) -> bool {
        self.0 & PORTSC_PR != 0
    }
    pub fn pp(&self) -> bool {
        self.0 & PORTSC_PP != 0
    }
    pub fn pls(&self) -> u32 {
        (self.0 & PORTSC_PLS_MASK) >> 5
    }
    pub fn speed(&self) -> u32 {
        (self.0 & PORTSC_SPEED_MASK) >> 10
    }
    pub fn wpr(&self) -> bool {
        self.0 & PORTSC_WPR != 0
    }
    pub fn csc(&self) -> bool {
        self.0 & PORTSC_CSC != 0
    }
    pub fn pec(&self) -> bool {
        self.0 & PORTSC_PEC != 0
    }
}

// ══════════════════════════════════════════════════════════════
//  OperationalRegisters
// ══════════════════════════════════════════════════════════════

pub struct OperationalRegisters(Mmio);

impl OperationalRegisters {
    pub unsafe fn new(base: *mut u8) -> Self {
        Self(Mmio(base))
    }

    pub fn read(&self, off: usize) -> u32 {
        self.0.read32(off)
    }
    pub fn write(&self, off: usize, val: u32) {
        self.0.write32(off, val);
    }

    // ── Registers ─────────────────────────────────────────────────
    pub fn usbcmd(&self) -> u32 {
        self.0.read32(OP_USBCMD)
    }
    pub fn set_usbcmd(&self, val: u32) {
        self.0.write32(OP_USBCMD, val);
    }
    pub fn set_usbcmd_bits(&self, bits: u32) {
        self.0.write32(OP_USBCMD, self.0.read32(OP_USBCMD) | bits);
    }
    pub fn clear_usbcmd_bits(&self, bits: u32) {
        self.0.write32(OP_USBCMD, self.0.read32(OP_USBCMD) & !bits);
    }

    pub fn usbsts(&self) -> u32 {
        self.0.read32(OP_USBSTS)
    }
    pub fn clear_usbsts_bits(&self, bits: u32) {
        self.0.write32(OP_USBSTS, bits);
    }

    pub fn crcr(&self) -> u64 {
        self.0.read64(OP_CRCR)
    }
    pub fn set_crcr(&self, val: u64) {
        self.0.write64(OP_CRCR, val);
    }
    pub fn dcbaap(&self) -> u64 {
        self.0.read64(OP_DCBAAP)
    }
    pub fn set_dcbaap(&self, val: u64) {
        self.0.write64(OP_DCBAAP, val);
    }

    pub fn config(&self) -> u32 {
        self.0.read32(OP_CONFIG)
    }
    pub fn set_config(&self, val: u32) {
        self.0.write32(OP_CONFIG, val);
    }

    pub fn portsc(&self, port: u32) -> PortSc {
        PortSc(
            self.0
                .read32(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE),
        )
    }
    pub fn write_portsc(&self, port: u32, val: u32) {
        self.0
            .write32(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE, val);
    }
    pub fn update_portsc(&self, port: u32, set: u32, clear: u32) {
        let off = OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE;
        // CRITICAL: Mask out RW1C bits when reading to prevent accidental
        // acknowledgment of status change events. RW1C bits (CSC, PEC, WRC,
        // OCC, PRC, PLC, CEC) are write-1-to-clear, so if we read them as 1
        // and write them back as 1, we inadvertently clear pending events.
        // Only the explicitly requested set/clear operations should affect
        // non-RW1C bits like PLS, PP, etc.
        let current = self.0.read32(off) & !PORTSC_RW1C_MASK;
        self.0.write32(off, (current & !clear) | set);
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

    pub fn ring(&self, slot: u32, target: u32) {
        let off = slot as usize * 4;
        // DB Target (DCI) in bits [7:0]; stream ID is zero for these endpoints.
        let val = target & 0xFF;
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
        if iters > 64 {
            log::warn!("xHCI: EC list exceeded max iterations");
            break;
        }
        let ec_id = m.read32(off * 4) as u8;
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        let ec_dw1 = m.read32(off * 4 + 4);
        log::info!(
            "xHCI EC: id={} next={} DWORD1=0x{:08X} (offset 0x{:04x})",
            ec_id,
            ec_next,
            ec_dw1,
            off * 4
        );
        if ec_id == 1 {
            let legsup = m.read32(off * 4);
            let legctl = m.read32(off * 4 + 4);
            log::info!(
                "  → USB Legacy Support: BIOS_SEM={} OS_SEM={} SMI_en=0x{:03x}",
                (legsup >> 16) & 1,
                (legsup >> 24) & 1,
                legctl & 0x1F
            );
        } else if ec_id == 2 {
            let dw2 = m.read32(off * 4 + 8);
            let port_off = (dw2 & 0xFF) as u32;
            let port_cnt = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev = m.read32(off * 4) >> 24;
            log::info!(
                "  → Supported Protocol: ports {}-{} rev={}.0 {}",
                port_off,
                port_off + port_cnt - 1,
                major_rev,
                if major_rev >= 3 { "USB 3.x" } else { "USB 2.0" }
            );
        }
        if ec_next == 0 {
            break;
        }
        off += ec_next as usize;
    }
}

pub fn parse_port_protocols(
    mmio_base: *mut u8,
    ext_cap_ptr: u16,
    n_ports: u32,
) -> alloc::vec::Vec<u32> {
    let m = Mmio(mmio_base);
    let n_words = ((n_ports + 31) / 32).max(1) as usize;
    let mut bitmap = alloc::vec![0xFFFFFFFFu32; n_words];
    let mut off = ext_cap_ptr as usize;
    let mut iters = 0;
    while off != 0 && off < 0x100000 {
        iters += 1;
        if iters > 64 {
            log::warn!("xHCI: parse_port_protocols exceeded max iterations");
            break;
        }
        let ec_id = m.read32(off * 4) as u8;
        if ec_id == 2 {
            let dw2 = m.read32(off * 4 + 8);
            let port_off = (dw2 & 0xFF) as u32;
            if port_off == 0 {
                let next = (m.read32(off * 4) >> 8) as u8;
                if next == 0 {
                    break;
                }
                off += next as usize;
                continue;
            }
            let port_cnt = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev = m.read32(off * 4) >> 24;
            let is_usb3 = major_rev >= 3;
            for p in 0..port_cnt {
                let port_idx = port_off + p - 1;
                if port_idx < n_ports {
                    let word = (port_idx / 32) as usize;
                    let bit = port_idx % 32;
                    if word < bitmap.len() {
                        if is_usb3 {
                            bitmap[word] |= 1 << bit;
                        } else {
                            bitmap[word] &= !(1 << bit);
                        }
                    }
                }
            }
        }
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        if ec_next == 0 {
            break;
        }
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
        if iters > 64 {
            return Err("circular capability list");
        }
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
                if (m.read32(cap_base) & (1 << 16)) == 0 {
                    ok = true;
                    break;
                }
                core::hint::spin_loop();
            }
            if !ok {
                return Err("legacy handoff timed out");
            }
            m.write32(cap_base + 4, m.read32(cap_base + 4) & !0x00F8001F);
            return Ok(false);
        }
        let ec_next = (m.read32(off * 4) >> 8) as u8;
        if ec_next == 0 {
            break;
        }
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
    use alloc::boxed::Box;

    // ── Simulated xHCI MMIO region ───────────────────────────────
    //
    // Layout (per xHCI spec §5.2):
    //   [0x0000..0x001C] Capability Registers
    //   [0x0020..0x03FF] Operational Registers (when CAPLENGTH=0x20)
    //   [0x0400..0x07FF] Port Register Set (16 bytes × N ports)
    //   [0x1000..0x101F] Runtime Registers (when RTSOFF=0x1000)
    //   [0x2000..0x2003] Doorbell Array (when DBOFF=0x2000)
    //   [0x3000..0x3FFF] Extended Capabilities (when XECP=0x3000)
    //
    // We use a single 16KB page to simulate the entire MMIO space.

    const MMIO_SIZE: usize = 0x4000;

    /// A simulated xHCI MMIO region backed by real memory.
    /// Tracks reads/writes and can auto-respond to certain patterns.
    struct SimHc {
        mem: Box<[u8; MMIO_SIZE]>,
    }

    impl SimHc {
        fn new(n_ports: u32) -> Self {
            let mut mem = Box::new([0u8; MMIO_SIZE]);

            // ── Capability Registers (§5.2.1) ─────────────────
            // CAPLENGTH = 0x20 (offset 0x00)
            mem[0x00] = 0x20;
            // HCIVERSION = 0x0110 (xHCI 1.1) at offset 0x02
            mem[0x02..0x04].copy_from_slice(&0x0110u16.to_le_bytes());
            // HCSPARAMS1 at offset 0x04: bits[7:0]=MaxSlots, bits[31:24]=NumPorts
            let hcs1 = (64u32) | (n_ports << 24);
            mem[0x04..0x08].copy_from_slice(&hcs1.to_le_bytes());
            // HCCPARAMS1 at offset 0x10: 64-bit + ext cap ptr
            let hcc1 = 1u32 | (0x0C00 << 16); // AC64=1, XECP byte offset 0x3000 / 4 = 0x0C00
            mem[0x10..0x14].copy_from_slice(&hcc1.to_le_bytes());
            // DBOFF at 0x14
            let db_off = 0x2000u32;
            mem[0x14..0x18].copy_from_slice(&db_off.to_le_bytes());
            // RTSOFF at 0x18
            let rt_off = 0x1000u32;
            mem[0x18..0x1C].copy_from_slice(&rt_off.to_le_bytes());

            // ── Operational Registers start at offset 0x20 ───
            // Default USBCMD=0, USBSTS=0 (HCH=1 → halted initially)
            let op_base = 0x20usize;
            // Set HCHalted (bit 0 of USBSTS)
            mem[op_base + OP_USBSTS] = 0x01;

            // ── Port registers at 0x400 (offset from op_base=0x20) ──
            for i in 0..n_ports as usize {
                let port_off = op_base + OP_PORTSC_BASE + i * OP_PORTSC_STRIDE;
                // PP=1, PLS=RxDetect(5), no CCS
                let portsc = PORTSC_PP | (5 << 5);
                mem[port_off..port_off + 4].copy_from_slice(&portsc.to_le_bytes());
            }

            // ── Extended Capabilities at 0x3000 ─────────────
            // USB Legacy Support (ECID=1) → nothing (skip handoff)
            let ec_base = 0x3000usize;
            mem[ec_base] = 2; // ECID = Supported Protocol
            mem[ec_base + 1] = 0; // next = 0 (last)
            // DWORD2 at offset 8: port_offset=1, port_count=n_ports
            let dw2 = 1u32 | (n_ports << 8) | (3u32 << 24); // USB 3.0
            mem[ec_base + 8..ec_base + 12].copy_from_slice(&dw2.to_le_bytes());

            Self { mem }
        }

        fn base(&self) -> *mut u8 {
            self.mem.as_ptr() as *mut u8
        }

        // Simulate a write that might trigger HW auto-response
        fn poke(&mut self, offset: usize, val: u32) {
            let old = self.read_hw(offset);
            self.write_hw(offset, val);

            // PORTSC auto-response: PP=1 after delay → CCS=1
            // This simulates a connected device training the link
            let op_base = 0x20usize;
            let port_start = op_base + OP_PORTSC_BASE;
            let port_end = port_start + 32 * OP_PORTSC_STRIDE;
            if (port_start..port_end).contains(&offset)
                && (offset - port_start) % OP_PORTSC_STRIDE == 0
            {
                let pp_was_set = (old & PORTSC_PP) == 0 && (val & PORTSC_PP) != 0;
                let pr_set = (val & PORTSC_PR) != 0;
                if pp_was_set {
                    // Device connects after power-up
                    self.write_hw(
                        offset,
                        (val & !PORTSC_RW1C_MASK) | PORTSC_CCS | PORTSC_PED | PORTSC_PP,
                    );
                    self.write_hw(offset + 4, 0); // PORTPMSC = 0
                } else if pr_set && (val & PORTSC_PR) == 0 {
                    // Port reset completing → CCS=1
                    self.write_hw(
                        offset,
                        (val & !PORTSC_PR & !PORTSC_RW1C_MASK)
                            | PORTSC_CCS
                            | PORTSC_PED
                            | PORTSC_PP,
                    );
                }
            }
        }

        fn read_hw(&self, offset: usize) -> u32 {
            u32::from_le_bytes(self.mem[offset..offset + 4].try_into().unwrap())
        }

        fn write_hw(&mut self, offset: usize, val: u32) {
            self.mem[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
        }
    }

    /// Simulated DriverContext for test use.
    struct TestDriver;
    impl crate::DriverContext for TestDriver {
        fn phys_to_virt(&self, _phys: u64) -> usize {
            0
        }
        fn allocate_frame(&self) -> Result<u64, crate::DriverContextError> {
            Ok(0x1000)
        }
        fn allocate_contiguous_frames(&self, _n: usize) -> Result<u64, crate::DriverContextError> {
            Ok(0x1000)
        }
        fn map_mmio_region(
            &self,
            _phys: usize,
            _virt: usize,
            _size: usize,
        ) -> Result<(), crate::DriverContextError> {
            Ok(())
        }
        fn map_page(
            &self,
            _virt: usize,
            _phys: usize,
            _flags: crate::PageFlags,
        ) -> Result<(), crate::DriverContextError> {
            Ok(())
        }
        fn free_frame(&self, _phys: u64) {}
        fn free_contiguous_frames(&self, _phys: u64, _count: usize) {}
        fn dma_map(
            &self,
            _device_id: u16,
            _phys: u64,
            _size: usize,
        ) -> Result<u64, crate::DriverContextError> {
            Ok(_phys)
        }
        fn dma_unmap(&self, _iova: u64, _size: usize) {}
    }

    #[test]
    fn test_register_read_write() {
        let op_base = 0x20usize;
        let sim = SimHc::new(2);
        let regs = unsafe { OperationalRegisters::new(sim.base().add(op_base)) };

        // USBCMD read/write
        assert_eq!(regs.usbcmd(), 0);
        regs.set_usbcmd(0xDEAD_BEEF);
        assert_eq!(regs.usbcmd(), 0xDEAD_BEEF);

        // USBSTS
        assert_eq!(regs.usbsts(), 0x01); // HCHalted
        regs.clear_usbsts_bits(USBSTS_HCH);
        assert_eq!(regs.usbsts(), 0x01); // RW1C: write 1 to clear → writing 0 does nothing

        // set_usbcmd_bits
        regs.set_usbcmd(0);
        regs.set_usbcmd_bits(USBCMD_RS);
        assert_eq!(regs.usbcmd(), USBCMD_RS);
        regs.clear_usbcmd_bits(USBCMD_RS);
        assert_eq!(regs.usbcmd(), 0);
    }

    #[test]
    fn test_portsc_power_and_ccs() {
        let op_base = 0x20usize;
        let mut sim = SimHc::new(2);
        let regs = unsafe { OperationalRegisters::new(sim.base().add(op_base)) };

        // Initial state: PP=1, PLS=RxDetect(5), CCS=0
        let ps = regs.portsc(0);
        assert!(ps.pp(), "port 0 should have PP=1");
        assert!(!ps.ccs(), "port 0 should have CCS=0 (no device)");
        assert_eq!(ps.pls(), 5, "port 0 should be in RxDetect");

        // Simulate device connection: hardware sets CCS+PED after link training
        let port0_off = op_base + OP_PORTSC_BASE + 0 * OP_PORTSC_STRIDE;
        let cur = sim.read_hw(port0_off);
        sim.write_hw(port0_off, cur | PORTSC_CCS | PORTSC_PED);

        // Now CCS=1 is reflected through the OperationalRegisters
        let ps = regs.portsc(0);
        assert!(
            ps.ccs(),
            "port 0 should detect device after simulated connection"
        );
        assert!(ps.ped(), "port 0 should be enabled");
    }

    #[test]
    fn test_capability_registers_read() {
        let sim = SimHc::new(4);
        let cap = unsafe { CapabilityRegisters::read(sim.base()) };
        assert_eq!(cap.caplength, 0x20);
        assert_eq!(cap.hci_version, 0x0110);
        assert_eq!(cap.db_offset, 0x2000);
        assert_eq!(cap.rt_offset, 0x1000);

        let hcs = cap.hcs_params1();
        assert_eq!(hcs.max_slots, 64);
        assert_eq!(hcs.n_ports, 4);
    }

    #[test]
    fn test_parse_port_protocols() {
        let sim = SimHc::new(4);
        let bitmap = parse_port_protocols(sim.base(), 0x3000, 4);
        assert!(!bitmap.is_empty());
        // All 4 ports should be USB 3.0
        assert_eq!(bitmap[0] & 0xF, 0xF);
    }

    #[test]
    fn test_update_portsc_preserves_rw1c() {
        let op_base = 0x20usize;
        let mut sim = SimHc::new(1);
        let port_off = op_base + OP_PORTSC_BASE;

        // Set some RW1C bits
        let initial = PORTSC_PP | PORTSC_CSC | PORTSC_PEC;
        sim.write_hw(port_off, initial);

        let regs = unsafe { OperationalRegisters::new(sim.base().add(op_base)) };
        regs.update_portsc(0, PORTSC_PED, PORTSC_PP); // set PED, clear PP

        // After update: PP cleared, PED set, RW1C bits should be PRESERVED (not cleared)
        // because update_portsc masks them out during read to avoid accidental acknowledgment
        let val = regs.portsc(0).0;
        assert_eq!(val & PORTSC_PP, 0, "PP should be cleared");
        assert_ne!(val & PORTSC_PED, 0, "PED should be set");
        assert_ne!(
            val & PORTSC_CSC,
            0,
            "CSC should be preserved (not accidentally cleared)"
        );
        assert_ne!(
            val & PORTSC_PEC,
            0,
            "PEC should be preserved (not accidentally cleared)"
        );
    }

    #[test]
    fn test_legacy_handoff_no_bios() {
        let sim = SimHc::new(2);
        let result = try_legacy_handoff(sim.base(), 0x3000);
        // No legacy support capability → Ok(true) = OS owns controller
        assert!(result.is_ok());
    }

    #[test]
    fn test_runtime_registers() {
        let rt_base = 0x1000usize;
        let sim = SimHc::new(2);
        let rt = unsafe { RuntimeRegisters::new(sim.base().add(rt_base)) };

        // Initially 0
        assert_eq!(rt.iman(), 0);
        assert_eq!(rt.erstsz(), 0);

        // Write and read back
        rt.set_iman(IMAN_IE);
        assert_eq!(rt.iman(), IMAN_IE);

        rt.set_erstsz(1);
        assert_eq!(rt.erstsz(), 1);

        // 64-bit ERDP
        let val64 = 0xDEAD_BEEF_CAFE_BABE;
        rt.set_erdp(val64);
        assert_eq!(rt.erdp(), val64);
    }

    #[test]
    fn test_doorbell_ring() {
        let db_base = 0x2000usize;
        let sim = SimHc::new(2);
        let db = unsafe { DoorbellRegisters::new(sim.base().add(db_base)) };

        db.ring(0, 0);
        let val = unsafe { ptr::read_volatile(sim.base().add(db_base) as *const u32) };
        assert_eq!(val, 0);

        // Ring doorbell for slot 5, DCI=1 (EP0): DB Target in bits [7:0]
        db.ring(5, 1);
        let val = unsafe { ptr::read_volatile(sim.base().add(db_base + 5 * 4) as *const u32) };
        assert_eq!(val, 1); // DB Target=1, stream ID=0
    }

    #[test]
    fn test_full_port_init_flow() {
        // Tests the complete init_ports sequence against simulated hardware:
        //   write PP → hardware sets CCS → RxDetect kick
        let op_base = 0x20usize;
        let mut sim = SimHc::new(2);
        let op = unsafe { OperationalRegisters::new(sim.base().add(op_base)) };

        // Step 1: Simulate what init_ports does — write PORTSC with PP
        for p in 0..2 {
            let ps = op.portsc(p).0;
            op.write_portsc(p, (ps & !PORTSC_RW1C_MASK) | PORTSC_PP);
        }

        // Verify the write reached the correct offset
        for p in 0..2 {
            let port_off = op_base + OP_PORTSC_BASE + p as usize * OP_PORTSC_STRIDE;
            let raw = sim.read_hw(port_off);
            assert_ne!(raw & PORTSC_PP, 0, "port {} PP should be set in HW", p);
        }

        // Simulate hardware response: device connected → CCS=1, PED=1
        for p in 0..2 {
            let port_off = op_base + OP_PORTSC_BASE + p as usize * OP_PORTSC_STRIDE;
            let cur = sim.read_hw(port_off);
            sim.write_hw(port_off, cur | PORTSC_CCS | PORTSC_PED);
        }

        // Verify driver can read CCS=1
        for p in 0..2 {
            let ps = op.portsc(p);
            assert!(
                ps.ccs(),
                "port {} should have CCS=1 after simulated connect",
                p
            );
            assert!(ps.ped(), "port {} should be enabled", p);
        }

        // Step 2: Simulate RxDetect kick via update_portsc
        const PLS_RXDETECT: u32 = 5 << 5;
        for p in 0..2 {
            op.update_portsc(p, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
        }

        // Verify the link state was set to RxDetect while preserving CCS
        for p in 0..2 {
            let ps = op.portsc(p);
            assert!(ps.ccs(), "port {} CCS must survive RxDetect", p);
            assert_eq!(ps.pls(), 5, "port {} PLS should be RxDetect", p);
        }
    }

    #[test]
    fn test_usbsts_halted_transition() {
        let op_base = 0x20usize;
        let mut sim = SimHc::new(1);
        let op = unsafe { OperationalRegisters::new(sim.base().add(op_base)) };

        // Initially halted
        assert_ne!(op.usbsts() & USBSTS_HCH, 0);

        // Simulate hardware clearing HCHalted after RS=1
        let usbsts_off = op_base + OP_USBSTS;
        sim.poke(usbsts_off, 0); // HCH=0 → running

        assert_eq!(op.usbsts() & USBSTS_HCH, 0, "controller should be running");
    }
}
