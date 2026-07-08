use alloc::vec::Vec;
use crate::iommu::MemCallbacks;

pub const IOPTE_R: u64 = 1 << 0;
pub const IOPTE_W: u64 = 1 << 1;
pub const IOPTE_S: u64 = 1 << 7;
pub const IOPTE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

fn iopte_addr(entry: u64) -> u64 {
    entry & IOPTE_ADDR_MASK
}

/// TT field values (bits 3:2 of ContextEntry.lo)
pub const TT_HOST_WITH_STRUCTURES: u64 = 0 << 2;  // 00b — host translation with SL page tables
pub const TT_PASS_THROUGH: u64 = 2 << 2;           // 10b — pass-through, no translation
pub const TT_GUEST: u64 = 3 << 2;                  // 11b — guest translation

pub const CTX_AW_3LEVEL: u64 = 2 << 8;
pub const CTX_AW_4LEVEL: u64 = 3 << 8;
pub const CTX_FPD: u64 = 1 << 1;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct ContextEntry(u64, u64);

pub const CTX_TT_MASK: u64 = 0b11 << 2;

impl ContextEntry {
    pub fn is_present(&self) -> bool {
        self.0 & 1 != 0
    }

    pub fn translation_type(&self) -> u64 {
        self.0 & CTX_TT_MASK
    }

    pub fn is_pass_through(&self) -> bool {
        self.is_present() && self.translation_type() == TT_PASS_THROUGH
    }

    pub fn is_blocked(&self) -> bool {
        self.is_present() && self.translation_type() == TT_HOST_WITH_STRUCTURES && self.1 == 0
    }

    /// Host translation entry: TT=00b with valid second-level page table.
    /// AW (Address Width) and Domain ID live in the high qword per VT-d spec.
    pub fn new_host(second_level_pt_phys: u64, address_width: u64) -> Self {
        let lo = (second_level_pt_phys & 0x000f_ffff_ffff_f000)
            | 1                 // Present
            | TT_HOST_WITH_STRUCTURES
            | CTX_FPD;
        let hi = address_width >> 8;
        Self(lo, hi)
    }

    /// Blocked entry: present + TT=00b with zero page table → DMA fault.
    pub fn new_blocked() -> Self {
        Self(1, 0)
    }

    /// Pass-through entry: TT=10b — DMA bypasses IOMMU translation.
    pub fn new_pass_through() -> Self {
        Self(1 | TT_PASS_THROUGH, 0)
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct RootEntry(u64);

impl RootEntry {
    pub fn is_present(&self) -> bool {
        self.0 & 1 != 0
    }

    pub fn new(ctx_table_phys: u64) -> Self {
        Self(ctx_table_phys | 1)
    }

    pub fn context_table_phys(&self) -> u64 {
        self.0 & 0xffff_ffff_ffff_f000
    }
}

pub struct IommuPageTable {
    root_phys: u64,
    root_virt: *mut u64,
    domain_id: u16,
    allocated_pages: Vec<u64>,
}

unsafe impl Send for IommuPageTable {}

impl IommuPageTable {
    pub fn new(ctx: &MemCallbacks, domain_id: u16) -> Result<Self, ()> {
        let phys = (ctx.alloc_frame)().ok_or(())?;
        let virt = (ctx.phys_to_virt)(phys) as *mut u64;
        unsafe { core::ptr::write_bytes(virt, 0, 4096); }
        Ok(Self {
            root_phys: phys,
            root_virt: virt,
            domain_id,
            allocated_pages: alloc::vec![phys],
        })
    }

    pub fn root_phys(&self) -> u64 {
        self.root_phys
    }

    pub fn domain_id(&self) -> u16 {
        self.domain_id
    }

    fn alloc_sl_table(&mut self, ctx: &MemCallbacks) -> Result<(u64, *mut u64), ()> {
        let phys = (ctx.alloc_frame)().ok_or(())?;
        let virt = (ctx.phys_to_virt)(phys) as *mut u64;
        unsafe { core::ptr::write_bytes(virt, 0, 4096); }
        self.allocated_pages.push(phys);
        Ok((phys, virt))
    }

    pub fn map_page(&mut self, ctx: &MemCallbacks, iova: u64, phys: u64) -> Result<(), ()> {
        if iova & 0xFFF != 0 || phys & 0xFFF != 0 || iova >> 39 != 0 {
            return Err(());
        }
        let sl2_virt = self.root_virt;
        let sl2_idx = ((iova >> 30) & 0x1FF) as usize;
        let sl2_entry = unsafe { &mut *sl2_virt.add(sl2_idx) };

        let sl1_virt: *mut u64;
        if *sl2_entry & IOPTE_R == 0 {
            let (p, v) = self.alloc_sl_table(ctx)?;
            *sl2_entry = p | IOPTE_R | IOPTE_W;
            sl1_virt = v;
        } else {
            sl1_virt = (ctx.phys_to_virt)(iopte_addr(*sl2_entry)) as *mut u64;
        }

        let sl1_idx = ((iova >> 21) & 0x1FF) as usize;
        let sl1_entry = unsafe { &mut *sl1_virt.add(sl1_idx) };

        let sl0_virt: *mut u64;
        if *sl1_entry & IOPTE_R == 0 {
            let (p, v) = self.alloc_sl_table(ctx)?;
            *sl1_entry = p | IOPTE_R | IOPTE_W;
            sl0_virt = v;
        } else {
            sl0_virt = (ctx.phys_to_virt)(iopte_addr(*sl1_entry)) as *mut u64;
        }

        let sl0_idx = ((iova >> 12) & 0x1FF) as usize;
        let sl0_entry = unsafe { &mut *sl0_virt.add(sl0_idx) };
        *sl0_entry = phys | IOPTE_R | IOPTE_W;
        Ok(())
    }

    pub fn unmap_page(&mut self, ctx: &MemCallbacks, iova: u64) {
        if iova & 0xFFF != 0 || iova >> 39 != 0 {
            return;
        }
        let sl2_virt = self.root_virt;
        let sl2_idx = ((iova >> 30) & 0x1FF) as usize;
        let sl2_entry = unsafe { &*sl2_virt.add(sl2_idx) };
        if *sl2_entry & IOPTE_R == 0 {
            return;
        }
        let sl1_virt = (ctx.phys_to_virt)(iopte_addr(*sl2_entry)) as *mut u64;
        let sl1_idx = ((iova >> 21) & 0x1FF) as usize;
        let sl1_entry = unsafe { &*sl1_virt.add(sl1_idx) };
        if *sl1_entry & IOPTE_R == 0 {
            return;
        }
        let sl0_virt = (ctx.phys_to_virt)(iopte_addr(*sl1_entry)) as *mut u64;
        let sl0_idx = ((iova >> 12) & 0x1FF) as usize;
        unsafe { *sl0_virt.add(sl0_idx) = 0; }
    }

    pub fn free_allocated(&self, ctx: &MemCallbacks) {
        for &p in &self.allocated_pages {
            (ctx.free_frame)(p);
        }
    }
}

pub struct IommuRootTable {
    root_table_phys: u64,
    root_table_virt: *mut RootEntry,
    context_table_pages: Vec<u64>,
}

unsafe impl Send for IommuRootTable {}

impl IommuRootTable {
    pub fn new(ctx: &MemCallbacks) -> Result<Self, ()> {
        let phys = (ctx.alloc_frame)().ok_or(())?;
        let virt = (ctx.phys_to_virt)(phys) as *mut RootEntry;
        unsafe { core::ptr::write_bytes(virt, 0, 4096); }
        Ok(Self {
            root_table_phys: phys,
            root_table_virt: virt,
            context_table_pages: Vec::new(),
        })
    }

    pub fn root_phys(&self) -> u64 {
        self.root_table_phys
    }

    pub fn get_context_entry(
        &mut self,
        ctx: &MemCallbacks,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Result<&mut ContextEntry, ()> {
        let root_entry = unsafe { &mut *self.root_table_virt.add(bus as usize) };
        let ctx_table_virt: *mut ContextEntry = if !root_entry.is_present() {
            let ct_phys = (ctx.alloc_frame)().ok_or(())?;
            let ct_virt = (ctx.phys_to_virt)(ct_phys) as *mut ContextEntry;
            unsafe { core::ptr::write_bytes(ct_virt, 0, 4096); }
            *root_entry = RootEntry::new(ct_phys);
            self.context_table_pages.push(ct_phys);
            ct_virt
        } else {
            (ctx.phys_to_virt)(root_entry.context_table_phys()) as *mut ContextEntry
        };

        let idx = ((device as usize) << 3) | (function as usize);
        Ok(unsafe { &mut *ctx_table_virt.add(idx) })
    }

    pub fn free_allocated(&self, ctx: &MemCallbacks) {
        for &p in &self.context_table_pages {
            (ctx.free_frame)(p);
        }
        (ctx.free_frame)(self.root_table_phys);
    }
}
