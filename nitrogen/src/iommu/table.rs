use crate::DriverContext;
use crate::DriverContextError;
use alloc::vec::Vec;

// ── IOMMU Page Table Entry flags (VT-d SL page tables) ─────────
pub const IOPTE_R: u64 = 1 << 0;  // Read (must be set)
pub const IOPTE_W: u64 = 1 << 1;  // Write
pub const IOPTE_S: u64 = 1 << 7;  // Page Size (2MB at SL1, 1GB at SL2)
pub const IOPTE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

fn iopte_addr(entry: u64) -> u64 {
    entry & IOPTE_ADDR_MASK
}

fn iopte_is_present(entry: u64) -> bool {
    entry & IOPTE_R != 0
}

fn iopte_is_huge(entry: u64) -> bool {
    entry & IOPTE_S != 0
}

// ── IOMMU Page Table (3-level, 4KB pages) ──────────────────────
// Level 2 (SL2): PML4-like, maps 512GB (512 × 512 × 512 × 4KB)
// Level 1 (SL1): PDP-like,  maps   1GB (512 × 512 × 4KB)
// Level 0 (SL0): PT-like,   maps   2MB (512 × 4KB)
//
// IOVA bits:   [38:30] → SL2 index
//              [29:21] → SL1 index
//              [20:12] → SL0 index
//              [11:0]  → page offset

const IOVA_BITS: u8 = 48; // 48-bit IOVA space
const SL2_SHIFT: u8 = 30;
const SL1_SHIFT: u8 = 21;
const SL0_SHIFT: u8 = 12;
const PAGE_SHIFT: u8 = 12;

fn sl2_index(iova: u64) -> usize {
    ((iova >> SL2_SHIFT) & 0x1ff) as usize
}

fn sl1_index(iova: u64) -> usize {
    ((iova >> SL1_SHIFT) & 0x1ff) as usize
}

fn sl0_index(iova: u64) -> usize {
    ((iova >> SL0_SHIFT) & 0x1ff) as usize
}

/// IOMMU 3-level second-level page table manager.
pub struct IommuPageTable {
    /// Physical address of the root (SL2) table
    root_phys: u64,
    /// Virtual address of the root (SL2) table
    root_virt: *mut u64,
    /// Domain ID assigned to this page table
    domain_id: u16,
    /// Allocated page frames (for cleanup)
    allocated_pages: Vec<u64>,
}

unsafe impl Send for IommuPageTable {}
unsafe impl Sync for IommuPageTable {}

impl IommuPageTable {
    pub fn new(ctx: &dyn DriverContext, domain_id: u16) -> Result<Self, DriverContextError> {
        let root_phys = ctx.allocate_frame()?;
        let root_virt = ctx.phys_to_virt(root_phys) as *mut u64;
        // Zero the root table
        unsafe {
            core::ptr::write_bytes(root_virt, 0, 4096);
        }
        let mut allocated = Vec::new();
        allocated.push(root_phys);
        Ok(Self {
            root_phys,
            root_virt,
            domain_id,
            allocated_pages: allocated,
        })
    }

    pub fn root_phys(&self) -> u64 {
        self.root_phys
    }

    pub fn domain_id(&self) -> u16 {
        self.domain_id
    }

    fn alloc_sl_table(&mut self, ctx: &dyn DriverContext) -> Result<(u64, *mut u64), DriverContextError> {
        let phys = ctx.allocate_frame()?;
        let virt = ctx.phys_to_virt(phys) as *mut u64;
        unsafe { core::ptr::write_bytes(virt, 0, 4096); }
        self.allocated_pages.push(phys);
        Ok((phys, virt))
    }

    /// Map IOVA → phys (4KB page). Allocates intermediate tables as needed.
    pub fn map_page(
        &mut self,
        ctx: &dyn DriverContext,
        iova: u64,
        phys: u64,
    ) -> Result<(), DriverContextError> {
        let sl2_virt = self.root_virt;
        let sl2_idx = sl2_index(iova);

        // Walk SL2 entry
        let sl2_entry = unsafe { &mut *sl2_virt.add(sl2_idx) };
        let sl1_virt = if *sl2_entry & IOPTE_R == 0 {
            let (tbl_phys, tbl_virt) = self.alloc_sl_table(ctx)?;
            *sl2_entry = (tbl_phys & IOPTE_ADDR_MASK) | IOPTE_R | IOPTE_W;
            tbl_virt
        } else {
            ctx.phys_to_virt(iopte_addr(*sl2_entry)) as *mut u64
        };

        let sl1_idx = sl1_index(iova);
        let sl1_entry = unsafe { &mut *sl1_virt.add(sl1_idx) };
        let sl0_virt = if *sl1_entry & IOPTE_R == 0 {
            let (tbl_phys, tbl_virt) = self.alloc_sl_table(ctx)?;
            *sl1_entry = (tbl_phys & IOPTE_ADDR_MASK) | IOPTE_R | IOPTE_W;
            tbl_virt
        } else {
            ctx.phys_to_virt(iopte_addr(*sl1_entry)) as *mut u64
        };

        let sl0_idx = sl0_index(iova);
        let sl0_entry = unsafe { &mut *sl0_virt.add(sl0_idx) };
        *sl0_entry = (phys & IOPTE_ADDR_MASK) | IOPTE_R | IOPTE_W;

        Ok(())
    }

    /// Unmap IOVA (4KB page). Zeros the PTE; does NOT free page tables.
    pub fn unmap_page(&self, ctx: &dyn DriverContext, iova: u64) {
        let sl2_virt = self.root_virt;
        let sl2_idx = sl2_index(iova);
        let sl2_entry = unsafe { &*sl2_virt.add(sl2_idx) };
        if *sl2_entry & IOPTE_R == 0 { return; }

        let sl1_virt = ctx.phys_to_virt(iopte_addr(*sl2_entry)) as *mut u64;
        let sl1_idx = sl1_index(iova);
        let sl1_entry = unsafe { &*sl1_virt.add(sl1_idx) };
        if *sl1_entry & IOPTE_R == 0 { return; }

        let sl0_virt = ctx.phys_to_virt(iopte_addr(*sl1_entry)) as *mut u64;
        let sl0_idx = sl0_index(iova);
        unsafe {
            *sl0_virt.add(sl0_idx) = 0;
        }
    }

    /// Free all allocated IOMMU page table pages.
    pub fn destroy(&mut self, ctx: &dyn DriverContext) {
        for &phys in &self.allocated_pages {
            ctx.free_frame(phys);
        }
        self.allocated_pages.clear();
    }
}

// ── Root Entry ──────────────────────────────────────────────────
// 8 bytes per entry, 512 entries per table = 4KB page
// Bit 0: Present (R)
// Bits 12:63: Context Table physical address

pub const ROOT_ENTRY_PRESENT: u64 = 1;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct RootEntry(u64);

impl RootEntry {
    pub fn new(context_table_phys: u64) -> Self {
        Self(context_table_phys | ROOT_ENTRY_PRESENT)
    }

    pub fn is_present(&self) -> bool {
        self.0 & ROOT_ENTRY_PRESENT != 0
    }

    pub fn context_table_phys(&self) -> u64 {
        self.0 & 0x000f_ffff_ffff_f000
    }
}

// ── Context Entry ───────────────────────────────────────────────
// 8 bytes per entry, 256 entries per context table = 2KB (use 4KB page)
// Translation Type (bits 0:1):
//   00: no translation (identity / pass-through)
//   01: reserved
//   10: host translation (2nd-level page tables)
//   11: guest translation (1st-level)
// Address Width (bits 7:9 for AW in pre-3.0, bits 8:10 for AW in 3.0):
//   We use bits 8:10 for 3-level (010) or 4-level (011)
// Second Level Page Table Pointer (bits 12:63)

pub const CTX_TT_NO_TRANSLATION: u64 = 0;
pub const CTX_TT_HOST: u64 = 2;
pub const CTX_TT_GUEST: u64 = 3;
pub const CTX_AW_3LEVEL: u64 = 2 << 8;  // 010
pub const CTX_AW_4LEVEL: u64 = 3 << 8;  // 011
pub const CTX_FPD: u64 = 1 << 3;        // Fault Processing Disable

#[derive(Clone, Copy)]
#[repr(C)]
pub struct ContextEntry(u64);

impl ContextEntry {
    pub fn new_host(sl_pt_phys: u64, aw_bits: u64) -> Self {
        Self(sl_pt_phys | CTX_TT_HOST | aw_bits)
    }

    pub fn new_pass_through() -> Self {
        Self(0) // TT = 00 = no translation
    }

    pub fn is_present(&self) -> bool {
        self.0 & 3 != 0 // TT != 0 means translation is present
    }

    pub fn set_sl_pt_ptr(&mut self, phys: u64) {
        self.0 = (self.0 & !0x000f_ffff_ffff_f000) | (phys & 0x000f_ffff_ffff_f000);
    }
}

// ── Root Table Manager ──────────────────────────────────────────

pub struct IommuRootTable {
    /// Physical address of the root table
    root_table_phys: u64,
    /// Virtual address of the root table
    root_table_virt: *mut RootEntry,
    /// Allocated context table pages
    context_table_pages: Vec<u64>,
}

unsafe impl Send for IommuRootTable {}
unsafe impl Sync for IommuRootTable {}

impl IommuRootTable {
    pub fn new(ctx: &dyn DriverContext) -> Result<Self, DriverContextError> {
        let phys = ctx.allocate_frame()?;
        let virt = ctx.phys_to_virt(phys) as *mut RootEntry;
        unsafe { core::ptr::write_bytes(virt, 0, 4096); }
        Ok(Self {
            root_table_phys: phys,
            root_table_virt: virt,
            context_table_pages: Vec::new(),
        })
    }

    pub fn root_table_phys(&self) -> u64 {
        self.root_table_phys
    }

    /// Get or create the context entry for a device.
    pub fn get_context_entry(
        &mut self,
        ctx: &dyn DriverContext,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Result<&mut ContextEntry, DriverContextError> {
        let root_entry = unsafe { &mut *self.root_table_virt.add(bus as usize) };

        // Get or create the context table for this bus
        let ctx_table_virt: *mut ContextEntry = if !root_entry.is_present() {
            let ct_phys = ctx.allocate_frame()?;
            let ct_virt = ctx.phys_to_virt(ct_phys) as *mut ContextEntry;
            unsafe { core::ptr::write_bytes(ct_virt, 0, 4096); }
            *root_entry = RootEntry::new(ct_phys);
            self.context_table_pages.push(ct_phys);
            ct_virt
        } else {
            ctx.phys_to_virt(root_entry.context_table_phys()) as *mut ContextEntry
        };

        let func_idx = (device as usize) * 8 + (function as usize);
        let entry = unsafe { &mut *ctx_table_virt.add(func_idx) };
        Ok(entry)
    }

    pub fn destroy(&mut self, drv_ctx: &dyn DriverContext) {
        for &phys in &self.context_table_pages {
            drv_ctx.free_frame(phys);
        }
        self.context_table_pages.clear();
        drv_ctx.free_frame(self.root_table_phys);
    }
}
