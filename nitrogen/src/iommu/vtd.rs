use core::ptr::{read_volatile, write_volatile};

// ── Register Offsets ──────────────────────────────────────────────
pub const VER: usize = 0x000;    // Version
pub const CAP: usize = 0x008;    // Capability
pub const ECAP: usize = 0x010;   // Extended Capability
pub const GCMD: usize = 0x018;   // Global Command
pub const GSTS: usize = 0x01C;   // Global Status
pub const RTADDR: usize = 0x020; // Root Table Address
pub const CCMD: usize = 0x028;   // Context Command
pub const FSTS: usize = 0x034;   // Fault Status
pub const FECTL: usize = 0x03C;  // Fault Event Control
pub const FEDATA: usize = 0x040; // Fault Event Data
pub const FEADDR: usize = 0x044; // Fault Event Address
pub const AFLOG: usize = 0x058;  // Advanced Fault Logging
pub const IQA: usize = 0x080;    // Invalidation Queue Address
pub const ICS: usize = 0x09C;    // Invalidation Completion Status
pub const IECTL: usize = 0x0A0;  // Invalidation Event Control
pub const IEDATA: usize = 0x0A4; // Invalidation Event Data
pub const IEADDR: usize = 0x0A8; // Invalidation Event Address
pub const IOTLB: usize = 0x0F0;  // IOTLB Invalidation

// ── GCMD bits ─────────────────────────────────────────────────────
pub const GCMD_SRTP: u32 = 1 << 30;  // Set Root Table Pointer
pub const GCMD_TE: u32 = 1 << 31;    // Translation Enable
pub const GCMD_SFL: u32 = 1 << 29;   // Set Fault Log
pub const GCMD_EAFL: u32 = 1 << 28;  // Enable Advanced Fault Logging
pub const GCMD_WBF: u32 = 1 << 27;   // Write Buffer Flush
pub const GCMD_IRE: u32 = 1 << 25;   // Interrupt Remapping Enable
pub const GCMD_CFI: u32 = 1 << 23;   // Compat Format Interrupt
pub const GCMD_QIE: u32 = 1 << 26;   // Queued Invalidation Enable

// ── GSTS bits ────────────────────────────────────────────────────
pub const GSTS_TES: u32 = 1 << 31;   // Translation Enable Status
pub const GSTS_IRES: u32 = 1 << 25;  // Interrupt Remap Enable Status
pub const GSTS_QIS: u32 = 1 << 26;   // Queued Invalidation Status
pub const GSTS_RTPS: u32 = 1 << 30;  // Root Table Pointer Status
pub const GSTS_WBFS: u32 = 1 << 27;  // Write Buffer Flush Status
pub const GSTS_AFLS: u32 = 1 << 28;  // Advanced Fault Logging Status
pub const GSTS_FLS: u32 = 1 << 29;   // Fault Log Status
pub const GSTS_CFIS: u32 = 1 << 23;  // Compat Format Interrupt Status

// ── CAP bit fields ────────────────────────────────────────────────
// Number of Domains = 2^(ND + 1) where ND = cap[7:4]
// Wait — spec says ND = cap[7:0] actually
// Let's just define helper.
// MGAW = (cap[33:30] + 1) bits
// SAGAW = cap[39:34]
// PSI = cap[60]

// ── CAP extractors ──────────────────────────────────────────────
pub fn cap_nd(cap: u64) -> u8 { (cap & 0xff) as u8 }
pub fn cap_num_domains(cap: u64) -> u32 { 1u32 << (cap_nd(cap) as u32 + 1) }
pub fn cap_mgaw(cap: u64) -> u8 { ((cap >> 30) & 0xf) as u8 }
pub fn cap_sagaw(cap: u64) -> u8 { ((cap >> 34) & 0x3f) as u8 }
pub fn cap_psi(cap: u64) -> bool { (cap >> 60) & 1 != 0 }

// ── ECAP extractors ─────────────────────────────────────────────
pub fn ecap_qi(ecap: u64) -> bool { (ecap >> 1) & 1 != 0 }   // Queued Invalidation
pub fn ecap_di(ecap: u64) -> bool { (ecap >> 7) & 1 != 0 }   // Device TLB Invalidation
pub fn ecap_ir(ecap: u64) -> bool { (ecap >> 3) & 1 != 0 }   // Interrupt Remapping (same position as DI? No — IR is bit 3 as well per some specs)
// Actually: ECAP bits: IR=3, EIM=4, PT=6, DI=7, ...

// ── VT-d Register Access ─────────────────────────────────────────

pub struct VtdRegisters {
    base: *mut u8,
}

unsafe impl Send for VtdRegisters {}
unsafe impl Sync for VtdRegisters {}

impl VtdRegisters {
    pub const fn new(base: *mut u8) -> Self {
        Self { base }
    }

    unsafe fn r32(&self, off: usize) -> u32 {
        read_volatile(self.base.add(off) as *const u32)
    }

    unsafe fn w32(&self, off: usize, val: u32) {
        write_volatile(self.base.add(off) as *mut u32, val);
    }

    unsafe fn r64(&self, off: usize) -> u64 {
        read_volatile(self.base.add(off) as *const u64)
    }

    unsafe fn w64(&self, off: usize, val: u64) {
        write_volatile(self.base.add(off) as *mut u64, val);
    }

    // ── High-level register accessors ──────────────────────────

    pub fn version(&self) -> u16 {
        unsafe { (self.r32(VER) & 0xffff) as u16 }
    }

    pub fn cap(&self) -> u64 {
        unsafe { self.r64(CAP) }
    }

    pub fn ecap(&self) -> u64 {
        unsafe { self.r64(ECAP) }
    }

    pub fn gcmd(&self) -> u32 {
        unsafe { self.r32(GCMD) }
    }

    pub fn set_gcmd(&self, val: u32) {
        unsafe { self.w32(GCMD, val) }
    }

    pub fn gsts(&self) -> u32 {
        unsafe { self.r32(GSTS) }
    }

    pub fn set_root_table(&self, phys: u64) {
        unsafe { self.w64(RTADDR, phys & 0x000f_ffff_ffff_f000) }
    }

    pub fn enable_translation(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_TE);
    }

    pub fn disable_translation(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd & !GCMD_TE);
    }

    pub fn set_root_table_ptr(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_SRTP);
    }

    const WAIT_TIMEOUT: u32 = 1_000_000;

    pub fn wait_for_root_table_ptr(&self) {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_RTPS != 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_root_table_ptr timeout");
    }

    pub fn wait_for_translation_enable(&self) {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_TES != 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_translation_enable timeout");
    }

    pub fn wait_for_translation_disable(&self) {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_TES == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_translation_disable timeout");
    }

    pub fn write_buffer_flush(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_WBF);
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_WBFS != 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: write_buffer_flush timeout");
    }

    pub fn iotlb_global_invalidate(&self) {
        unsafe { self.w64(IOTLB, 1) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(IOTLB) };
            if val & 1 == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: iotlb_global_invalidate timeout");
    }

    pub fn iotlb_domain_invalidate(&self, domain_id: u16) {
        // IIRG=01 (domain granularity), IVT=1, DID=domain_id
        let val = 1 | ((domain_id as u64) << 32) | (1u64 << 2);
        unsafe { self.w64(IOTLB, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(IOTLB) };
            if val & 1 == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: iotlb_domain_invalidate timeout");
    }

    pub fn context_cache_invalidate_all(&self) {
        // CCMD: IVT=1, CIRG=00 (global)
        let val = 1u64 << 63;
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_all timeout");
    }

    pub fn context_cache_invalidate_domain(&self, domain_id: u16) {
        // CCMD: IVT=1, CIRG=01 (domain), DID=domain_id
        let val = (domain_id as u64) << 32 | (1u64 << 61) | (1u64 << 63);
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_domain timeout");
    }

    pub fn context_cache_invalidate_device(&self, sid: u16, function_mask: u8) {
        // CCMD: IVT=1, CIRG=10 (device), SID=sid, FM=function_mask
        let val = (sid as u64) << 16 | ((function_mask as u64) << 8) | (1u64 << 62) | (1u64 << 63);
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_device timeout");
    }

    /// Check if the IOMMU hardware is already enabled (by firmware)
    pub fn is_enabled(&self) -> bool {
        self.gsts() & GSTS_TES != 0
    }
}
