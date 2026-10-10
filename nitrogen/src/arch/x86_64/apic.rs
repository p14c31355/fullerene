//! APIC (Advanced Programmable Interrupt Controller) constants and definitions.
//!
//! Pure hardware constants — no allocation, no dependencies beyond `core`.

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
    pub const LVT_PERF_COUNT: u32 = 0x340; // Performance monitoring counter LVT
    pub const LVT_THERMAL: u32 = 0x330; // Thermal sensor LVT
    pub const TMRDIV: u32 = 0x3E0;
    pub const TMRINITCNT: u32 = 0x380;
    pub const TMRCURRCNT: u32 = 0x390;
    pub const EOI: u32 = 0x0B0;
    pub const ID: u32 = 0x20;
    pub const VERSION: u32 = 0x30;
    pub const ICR_LOW: u32 = 0x300;
    pub const ICR_HIGH: u32 = 0x310;
}

/// APIC control bits
pub struct ApicFlags;
impl ApicFlags {
    pub const SW_ENABLE: u32 = 1 << 8;
    pub const DISABLE: u32 = 0x10000;
    pub const TIMER_PERIODIC: u32 = 1 << 17;
    pub const TIMER_ONESHOT: u32 = 0; // Bit 17 = 0 → one-shot mode
    pub const TIMER_MASKED: u32 = 1 << 16;

    // LVT delivery mode bits [10:8]
    // 000 = Fixed, 010 = SMI, 100 = NMI, 101 = INIT, 111 = ExtINT
    pub const DELIVERY_MODE_FIXED: u32 = 0 << 8;
    pub const DELIVERY_MODE_NMI: u32 = 4 << 8;
    pub const DELIVERY_MODE_INIT: u32 = 5 << 8;
    pub const DELIVERY_MODE_STARTUP: u32 = 6 << 8;
    pub const DELIVERY_STATUS_PENDING: u32 = 1 << 12;
    pub const LEVEL_ASSERT: u32 = 1 << 14;
    pub const TRIGGER_LEVEL: u32 = 1 << 15;
}

/// Default IO APIC base address
pub const IO_APIC_BASE: u64 = 0xFEC00000;
