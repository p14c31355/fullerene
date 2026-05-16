//! APIC (Advanced Programmable Interrupt Controller) constants and definitions.

/// APIC register offsets
pub struct ApicOffsets;
impl ApicOffsets {
    pub const BASE_MSR: u32 = 0x1B;
    pub const BASE_ADDR_MASK: u64 = !0xFFF;
    pub const SPURIOUS_VECTOR: u32 = 0x0F0;
    pub const LVT_TIMER: u32 = 0x320;
    pub const LVT_LINT0: u32 = 0x350;
    pub const LVT_LINT1: u32 = 0x360;
    pub const LVT_ERROR: u32 = 0x370;
    pub const TMRDIV: u32 = 0x3E0;
    pub const TMRINITCNT: u32 = 0x380;
    pub const TMRCURRCNT: u32 = 0x390;
    pub const EOI: u32 = 0x0B0;
    pub const ID: u32 = 0x20;
    pub const VERSION: u32 = 0x30;
}

/// APIC control bits
pub struct ApicFlags;
impl ApicFlags {
    pub const SW_ENABLE: u32 = 1 << 8;
    pub const DISABLE: u32 = 0x10000;
    pub const TIMER_PERIODIC: u32 = 1 << 17;
    pub const TIMER_MASKED: u32 = 1 << 16;
}

/// Default IO APIC base address
pub const IO_APIC_BASE: u64 = 0xFEC00000;
