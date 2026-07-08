pub mod table;
pub mod vtd;

use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use crate::acpi;
use crate::pci::PciScanner;
use table::{IommuPageTable, IommuRootTable};
use vtd::VtdRegisters;

#[derive(Clone, Copy)]
pub struct MemCallbacks {
    pub alloc_frame: fn() -> Option<u64>,
    pub free_frame: fn(u64),
    pub phys_to_virt: fn(u64) -> usize,
    pub map_mmio: fn(phys: usize, size: usize) -> Result<usize, ()>,
}

static MEM: Mutex<Option<MemCallbacks>> = Mutex::new(None);

pub fn set_mem_callbacks(cbs: MemCallbacks) {
    *MEM.lock() = Some(cbs);
}

struct IovaInterval { start: u64, end: u64 }

struct IovaAllocator {
    free: Vec<IovaInterval>,
}

impl IovaAllocator {
    fn new(iova_bits: u8) -> Self {
        // Cap at 39 bits (3-level page table limit)
        let bits = iova_bits.min(39);
        let start: u64 = 1 << 12;
        let max_addr: u64 = (1u64 << bits) - 1;
        Self {
            free: alloc::vec![IovaInterval { start, end: max_addr }],
        }
    }

    fn alloc(&mut self, size: usize) -> Option<u64> {
        let aligned = (size + 4095) & !4095;
        if aligned == 0 { return None; }
        let need = aligned as u64;
        let mut best: Option<(usize, u64)> = None;
        for (i, iv) in self.free.iter().enumerate() {
            let avail = iv.end - iv.start + 1;
            if avail >= need {
                let fits = match best {
                    Some((_, addr)) => iv.start < addr,
                    None => true,
                };
                if fits { best = Some((i, iv.start)); }
            }
        }
        let (idx, addr) = best?;
        let iv = &mut self.free[idx];
        iv.start += need;
        if iv.start > iv.end { self.free.remove(idx); }
        Some(addr)
    }

    fn free(&mut self, addr: u64, size: usize) {
        let aligned = (size + 4095) & !4095;
        if aligned == 0 { return; }
        let start = addr;
        let end = addr + aligned as u64 - 1;
        let mut intervals = core::mem::take(&mut self.free);
        intervals.push(IovaInterval { start, end });
        intervals.sort_by_key(|iv| iv.start);
        let mut merged: Vec<IovaInterval> = Vec::new();
        for iv in intervals {
            if let Some(last) = merged.last_mut() {
                if iv.start <= last.end.saturating_add(1) {
                    last.end = last.end.max(iv.end);
                    continue;
                }
            }
            merged.push(iv);
        }
        self.free = merged;
    }
}

struct IommuEngine {
    registers: VtdRegisters,
    root_table: IommuRootTable,
    page_table: IommuPageTable,
    iova: IovaAllocator,
}

unsafe impl Send for IommuEngine {}

impl IommuEngine {
    fn new(regs: VtdRegisters, iova_bits: u8) -> Result<Self, ()> {
        let cbs = MEM.lock();
        let ctx = cbs.as_ref().ok_or(())?;
        let root_table = IommuRootTable::new(ctx)?;
        let page_table = IommuPageTable::new(ctx, 0)?;
        let iova = IovaAllocator::new(iova_bits);
        Ok(Self { registers: regs, root_table, page_table, iova })
    }

    /// Populate pass-through entries for all discovered PCI devices.
    fn setup_pass_through_all(&mut self) -> Result<(), ()> {
        let mut scanner = PciScanner::new();
        scanner.scan_all_buses().map_err(|_| ())?;
        if scanner.get_devices().is_empty() {
            log::warn!("IOMMU: no PCI devices found, skipping pass-through setup");
            return Ok(());
        }
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(())?;
        for dev_info in scanner.get_devices() {
            let entry = self.root_table.get_context_entry(ctx, dev_info.bus, dev_info.device, dev_info.function)?;
            *entry = table::ContextEntry::new_pass_through();
        }
        Ok(())
    }

    fn dma_map(&mut self, device_id: u16, phys: u64, size: usize) -> Result<u64, ()> {
        let bus = (device_id >> 8) as u8;
        let device = ((device_id >> 3) & 0x1F) as u8;
        let function = (device_id & 0x7) as u8;

        {
            let mut cbs = MEM.lock();
            let ctx = cbs.as_mut().ok_or(())?;
            let entry = self.root_table.get_context_entry(ctx, bus, device, function)?;
            if !entry.is_blocked() {
                *entry = table::ContextEntry::new_host(
                    self.page_table.root_phys(),
                    table::CTX_AW_3LEVEL,
                );
            }
        }

        let iova = self.iova.alloc(size).ok_or(())?;
        let pages = (size + 4095) / 4096;
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(())?;

        for i in 0..pages {
            let iova_page = iova + (i as u64) * 4096;
            let phys_page = phys + (i as u64) * 4096;
            if self.page_table.map_page(ctx, iova_page, phys_page).is_err() {
                for j in 0..i {
                    self.page_table.unmap_page(ctx, iova + (j as u64) * 4096);
                }
                self.iova.free(iova, size);
                return Err(());
            }
        }

        self.registers.context_cache_invalidate_domain(self.page_table.domain_id());
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
        Ok(iova)
    }

    fn dma_unmap(&mut self, iova: u64, size: usize) {
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(()).unwrap();
        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            let iova_page = iova + (i as u64) * 4096;
            self.page_table.unmap_page(ctx, iova_page);
        }
        self.iova.free(iova, size);
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
    }
}

static GLOBAL_IOMMU: Mutex<Option<IommuEngine>> = Mutex::new(None);
static IOMMU_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init(rsdp_phys: u64) -> Result<(), &'static str> {
    if IOMMU_INITIALIZED.load(core::sync::atomic::Ordering::Relaxed) {
        log::info!("IOMMU: already initialized, re-init skipped");
        return Ok(());
    }

    let dmar = acpi::dmar::parse_dmar(rsdp_phys).ok_or("failed to parse DMAR table")?;
    if dmar.drhd_units.is_empty() {
        return Err("IOMMU: no DRHD units found");
    }

    let drhd = &dmar.drhd_units[0];
    let mmio_base = drhd.phys_base;
    let iova_bits = dmar.host_address_width;

    log::info!("IOMMU: mmio_base={:#x} host_addr_width={} drhd_units={}",
        mmio_base, iova_bits, dmar.drhd_units.len());

    let mmio_virt = {
        let guard = MEM.lock();
        let cbs = guard.as_ref().ok_or("IOMMU: MemCallbacks not set")?;
        // Map VT-d MMIO region as uncached (required by VT-d spec)
        let virt = (cbs.map_mmio)(mmio_base as usize, 4096).map_err(|_| "IOMMU: MMIO mapping failed")?;
        virt as *mut u8
    };
    let regs = VtdRegisters::new(mmio_virt);
    let ver = regs.version();
    let cap = regs.cap();
    let ecap = regs.ecap();
    log::info!("IOMMU: version={:#x} cap={:#x} ecap={:#x} domains={} mgaw={} sagaw={}",
        ver, cap, ecap,
        vtd::cap_num_domains(cap),
        vtd::cap_mgaw(cap),
        vtd::cap_sagaw(cap));

    let mut engine = IommuEngine::new(regs, iova_bits)
        .map_err(|_| "IOMMU engine creation failed")?;
    engine.setup_pass_through_all()
        .map_err(|_| "IOMMU pass-through setup failed")?;

    if engine.registers.gsts() & vtd::GSTS_TES != 0 {
        log::info!("IOMMU: already enabled by firmware");
    }

    if !engine.registers.write_buffer_flush() {
        return Err("IOMMU: write buffer flush failed");
    }
    engine.registers.set_rtaddr(engine.root_table.root_phys());
    engine.registers.set_root_table_ptr();
    if !engine.registers.wait_for_root_table_ptr() {
        return Err("IOMMU: root table pointer setup timed out");
    }

    engine.registers.enable_translation();
    if !engine.registers.wait_for_translation_enable() {
        return Err("IOMMU: translation enable timed out");
    }

    // TODO: Support multiple DRHD remapping units. Currently only the first
    // DRHD is initialized; devices behind other units will not get context
    // setup and will fault on DMA. dmar.drhd_units.len() logged above.
    *GLOBAL_IOMMU.lock() = Some(engine);
    IOMMU_INITIALIZED.store(true, core::sync::atomic::Ordering::Release);
    log::info!("IOMMU: initialized with pass-through for all devices");
    Ok(())
}

pub fn dma_map(device_id: u16, phys: u64, size: usize) -> Result<u64, ()> {
    let mut guard = GLOBAL_IOMMU.lock();
    if let Some(ref mut engine) = *guard {
        engine.dma_map(device_id, phys, size)
    } else {
        Ok(phys) // identity fallback when no IOMMU
    }
}

pub fn dma_unmap(iova: u64, size: usize) {
    let mut guard = GLOBAL_IOMMU.lock();
    if let Some(ref mut engine) = *guard {
        engine.dma_unmap(iova, size);
    }
}
