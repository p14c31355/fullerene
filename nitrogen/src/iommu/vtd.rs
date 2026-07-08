use core::ptr::{read_volatile, write_volatile};

pub const VER: usize = 0x000;
pub const CAP: usize = 0x008;
pub const ECAP: usize = 0x010;
pub const GCMD: usize = 0x018;
pub const GSTS: usize = 0x01C;
pub const RTADDR: usize = 0x020;
pub const CCMD: usize = 0x028;
pub const FSTS: usize = 0x034;
pub const FECTL: usize = 0x03C;
pub const FEDATA: usize = 0x040;
pub const FEADDR: usize = 0x044;
pub const AFLOG: usize = 0x058;
pub const IQA: usize = 0x080;
pub const ICS: usize = 0x09C;
pub const IECTL: usize = 0x0A0;
pub const IEDATA: usize = 0x0A4;
pub const IEADDR: usize = 0x0A8;
pub const IOTLB: usize = 0x0F0;

pub const GCMD_SRTP: u32 = 1 << 30;
pub const GCMD_TE: u32 = 1 << 31;
pub const GCMD_SFL: u32 = 1 << 29;
pub const GCMD_EAFL: u32 = 1 << 28;
pub const GCMD_WBF: u32 = 1 << 27;
pub const GCMD_IRE: u32 = 1 << 25;
pub const GCMD_CFI: u32 = 1 << 23;
pub const GCMD_QIE: u32 = 1 << 26;

pub const GSTS_TES: u32 = 1 << 31;
pub const GSTS_IRES: u32 = 1 << 25;
pub const GSTS_QIS: u32 = 1 << 26;
pub const GSTS_RTPS: u32 = 1 << 30;
pub const GSTS_WBFS: u32 = 1 << 27;
pub const GSTS_AFLS: u32 = 1 << 28;
pub const GSTS_FLS: u32 = 1 << 29;
pub const GSTS_CFIS: u32 = 1 << 23;

pub fn cap_nd(cap: u64) -> u8 { (cap & 0xff) as u8 }
pub fn cap_num_domains(cap: u64) -> u32 { 1u32 << (cap_nd(cap) as u32 + 1) }
pub fn cap_mgaw(cap: u64) -> u8 { ((cap >> 30) & 0xf) as u8 }
pub fn cap_sagaw(cap: u64) -> u8 { ((cap >> 34) & 0x3f) as u8 }
pub fn cap_psi(cap: u64) -> bool { (cap >> 60) & 1 != 0 }

pub fn ecap_qi(ecap: u64) -> bool { (ecap >> 1) & 1 != 0 }
pub fn ecap_di(ecap: u64) -> bool { (ecap >> 7) & 1 != 0 }
pub fn ecap_ir(ecap: u64) -> bool { (ecap >> 3) & 1 != 0 }

pub struct VtdRegisters {
    base: *mut u8,
}

unsafe impl Send for VtdRegisters {}
unsafe impl Sync for VtdRegisters {}

impl VtdRegisters {
    pub const WAIT_TIMEOUT: u32 = 100_000_000;

    pub const fn new(base: *mut u8) -> Self {
        Self { base }
    }

    unsafe fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile(self.base.add(off) as *const u32) }
    }

    unsafe fn w32(&self, off: usize, val: u32) {
        unsafe { write_volatile(self.base.add(off) as *mut u32, val) };
    }

    unsafe fn r64(&self, off: usize) -> u64 {
        unsafe { read_volatile(self.base.add(off) as *const u64) }
    }

    unsafe fn w64(&self, off: usize, val: u64) {
        unsafe { write_volatile(self.base.add(off) as *mut u64, val) };
    }

    pub fn version(&self) -> u32 {
        unsafe { self.r32(VER) }
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

    pub fn set_rtaddr(&self, phys: u64) {
        unsafe { self.w64(RTADDR, phys) }
    }

    pub fn set_root_table_ptr(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_SRTP);
    }

    pub fn enable_translation(&self) {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_TE);
    }

    pub fn wait_for_root_table_ptr(&self) -> bool {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_RTPS != 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_root_table_ptr timeout");
        false
    }

    pub fn wait_for_translation_enable(&self) -> bool {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_TES != 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_translation_enable timeout");
        false
    }

    pub fn wait_for_translation_disable(&self) -> bool {
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_TES == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: wait_for_translation_disable timeout");
        false
    }

    pub fn write_buffer_flush(&self) -> bool {
        let cmd = self.gcmd();
        self.set_gcmd(cmd | GCMD_WBF);
        for _ in 0..Self::WAIT_TIMEOUT {
            if self.gsts() & GSTS_WBFS != 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: write_buffer_flush timeout");
        false
    }

    pub fn iotlb_global_invalidate(&self) -> bool {
        // IOTLB invalidation: IVT=1 (bit 0 set), IIRG=00 (global)
        unsafe { self.w64(IOTLB, 1) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(IOTLB) };
            if val & 1 == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: iotlb_global_invalidate timeout");
        false
    }

    pub fn iotlb_domain_invalidate(&self, domain_id: u16) -> bool {
        // IOTLB invalidation: IVT=1, IIRG=01 (domain), DID=domain_id
        let val = 1u64 | (1u64 << 2) | ((domain_id as u64) << 32);
        unsafe { self.w64(IOTLB, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(IOTLB) };
            if val & 1 == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: iotlb_domain_invalidate timeout");
        false
    }

    pub fn context_cache_invalidate_all(&self) -> bool {
        // CCMD: IVT=1 (bit 63), CIRG=00 (global)
        let val = 1u64 << 63;
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_all timeout");
        false
    }

    pub fn context_cache_invalidate_domain(&self, domain_id: u16) -> bool {
        // CCMD: IVT=1 (bit 63), CIRG=01 (domain), DID=domain_id
        let val = (1u64 << 63) | (1u64 << 61) | ((domain_id as u64) << 32);
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_domain timeout");
        false
    }

    pub fn context_cache_invalidate_device(&self, sid: u16, function_mask: u8) -> bool {
        // CCMD: IVT=1 (bit 63), CIRG=10 (device), SID=sid, FM=function_mask
        let val = (1u64 << 63) | (2u64 << 61) | ((sid as u64) << 16) | ((function_mask as u64) << 8);
        unsafe { self.w64(CCMD, val) }
        for _ in 0..Self::WAIT_TIMEOUT {
            let val = unsafe { self.r64(CCMD) };
            if val & (1u64 << 63) == 0 { return true; }
            core::hint::spin_loop();
        }
        log::warn!("IOMMU: context_cache_invalidate_device timeout");
        false
    }

    pub fn is_enabled(&self) -> bool {
        self.gsts() & GSTS_TES != 0
    }
}
