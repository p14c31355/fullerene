pub mod acpi;
pub mod vtd;
pub mod table;

use alloc::vec::Vec;
use spin::Mutex;

use crate::DriverContext;
use crate::DriverContextError;
use crate::pci::PciScanner;
use vtd::VtdRegisters;
use table::{IommuPageTable, IommuRootTable};

// ── IOVA Allocator ──────────────────────────────────────────────────

struct IovaInterval {
    start: u64,
    end: u64,
}

/// Simple interval-based IOVA allocator.
struct IovaAllocator {
    base: u64,
    max: u64,
    free: Vec<IovaInterval>,
}

impl IovaAllocator {
    fn new(iova_bits: u8) -> Self {
        // IOVA space: start at 1MB to avoid conflicts with low DMA
        // Max is based on the address width, capped at 512GB
        let max_addr = (1u64 << iova_bits.min(48)) - 1;
        let start = 0x10_0000u64; // 1MB
        Self {
            base: start,
            max: max_addr,
            free: alloc::vec![IovaInterval { start, end: max_addr }],
        }
    }

    fn alloc(&mut self, size: usize) -> Option<u64> {
        let aligned_size = (size as u64 + 4095) & !4095;
        for i in 0..self.free.len() {
            let iv = &self.free[i];
            let range = iv.end - iv.start;
            if range >= aligned_size {
                let addr = iv.start;
                let new_start = iv.start + aligned_size;
                if new_start < iv.end {
                    self.free[i].start = new_start;
                } else {
                    self.free.remove(i);
                }
                return Some(addr);
            }
        }
        None
    }

    fn free(&mut self, addr: u64, size: usize) {
        let aligned_size = (size as u64 + 4095) & !4095;
        let start = addr;
        let end = addr + aligned_size;
        let mut new_free = Vec::new();
        let mut inserted = false;

        for iv in self.free.iter() {
            if !inserted && end < iv.start {
                new_free.push(IovaInterval { start, end });
                new_free.push(IovaInterval { start: iv.start, end: iv.end });
                inserted = true;
            } else if !inserted && start <= iv.end && end >= iv.start {
                // Merge adjacent
                let merged_start = start.min(iv.start);
                let merged_end = end.max(iv.end);
                new_free.push(IovaInterval { start: merged_start, end: merged_end });
                inserted = true;
            } else if inserted && start <= iv.start && end >= iv.start {
                // If the last merged entry overlaps the next one, coalesce
                if let Some(last) = new_free.last_mut() {
                    if last.end >= iv.start {
                        last.end = last.end.max(iv.end);
                        continue;
                    }
                }
                new_free.push(IovaInterval { start: iv.start, end: iv.end });
            } else {
                new_free.push(IovaInterval { start: iv.start, end: iv.end });
            }
        }
        if !inserted {
            new_free.push(IovaInterval { start, end });
        }
        // Sort by start (should already be sorted but be safe)
        new_free.sort_by_key(|iv| iv.start);
        self.free = new_free;
    }
}

// ── IOMMU Engine ────────────────────────────────────────────────────

pub struct IommuEngine {
    registers: VtdRegisters,
    root_table: IommuRootTable,
    page_table: IommuPageTable,
    iova: IovaAllocator,
}

impl IommuEngine {
    fn new(
        mmio_base: *mut u8,
        ctx: &dyn DriverContext,
        iova_bits: u8,
    ) -> Result<Self, DriverContextError> {
        let registers = VtdRegisters::new(mmio_base);

        // Build root table
        let mut root_table = IommuRootTable::new(ctx)?;

        // Build IOMMU page table for domain 0
        let page_table = IommuPageTable::new(ctx, 0)?;

        // IOVA allocator
        let iova = IovaAllocator::new(iova_bits);

        Ok(Self { registers, root_table, page_table, iova })
    }

    pub fn set_device_context(
        &mut self,
        ctx: &dyn DriverContext,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Result<(), DriverContextError> {
        let entry = self.root_table.get_context_entry(ctx, bus, device, function)?;
        let aw_bits = table::CTX_AW_3LEVEL;
        *entry = table::ContextEntry::new_host(self.page_table.root_phys(), aw_bits);
        Ok(())
    }

    pub fn enable(&self) {
        let regs = &self.registers;

        // 1. Set root table address
        regs.set_root_table(self.root_table.root_table_phys());
        regs.set_root_table_ptr();
        regs.wait_for_root_table_ptr();

        // 2. Flush write buffer
        regs.write_buffer_flush();

        // 3. Enable DMA remapping
        regs.enable_translation();
        regs.wait_for_translation_enable();
    }

    pub fn dma_map(
        &mut self,
        ctx: &dyn DriverContext,
        device_id: u16,
        phys: u64,
        size: usize,
    ) -> Result<u64, DriverContextError> {
        if size == 0 {
            return Err(DriverContextError::InvalidArgument);
        }
        if phys & 0xFFF != 0 {
            return Err(DriverContextError::InvalidArgument);
        }

        // Validate before mutating state
        let iova = self.iova.alloc(size).ok_or(DriverContextError::OutOfMemory)?;
        let pages = (size + 4095) / 4096;
        // Pre-validate by checking page table walk doesn't fail early (no-op for now)

        // Switch this device's context entry from pass-through to host translation
        let bus = (device_id >> 8) as u8;
        let dev = ((device_id >> 3) & 0x1f) as u8;
        let func = (device_id & 7) as u8;
        let entry = self.root_table.get_context_entry(ctx, bus, dev, func)?;
        *entry = table::ContextEntry::new_host(self.page_table.root_phys(), table::CTX_AW_3LEVEL);

        // Map pages with rollback on failure
        let mut mapped = 0usize;
        let result = (|| -> Result<(), DriverContextError> {
            for i in 0..pages {
                let iova_page = iova + (i as u64) * 4096;
                let phys_page = phys + (i as u64) * 4096;
                self.page_table.map_page(ctx, iova_page, phys_page)?;
                mapped = i + 1;
            }
            Ok(())
        })();
        if result.is_err() {
            // Rollback: unmap any pages that were already mapped
            for i in 0..mapped {
                let iova_page = iova + (i as u64) * 4096;
                self.page_table.unmap_page(ctx, iova_page);
            }
            self.iova.free(iova, size);
            return Err(result.unwrap_err());
        }

        // Flush context cache and IOTLB
        self.registers.context_cache_invalidate_domain(self.page_table.domain_id());
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
        Ok(iova)
    }

    pub fn dma_unmap(&mut self, ctx: &dyn DriverContext, iova: u64, size: usize) {
        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            let iova_page = iova + (i as u64) * 4096;
            self.page_table.unmap_page(ctx, iova_page);
        }
        self.iova.free(iova, size);
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
    }

    /// Set up pass-through context entries for all devices found by a PCI scan.
    /// This ensures existing (non-IOMMU-aware) drivers can keep working after
    /// translation is enabled.
    pub fn setup_pass_through_all(
        &mut self,
        ctx: &dyn DriverContext,
    ) -> Result<(), DriverContextError> {
        let mut scanner = PciScanner::new();
        if scanner.scan_all_buses().is_err() {
            // Fall back to bus 0 only if scan fails
            for dev in 0..32 {
                for func in 0..8 {
                    let entry = self.root_table.get_context_entry(ctx, 0, dev, func)?;
                    *entry = table::ContextEntry::new_pass_through();
                }
            }
            return Ok(());
        }
        for dev_info in scanner.get_devices() {
            let entry = self.root_table.get_context_entry(
                ctx, dev_info.bus, dev_info.device, dev_info.function,
            )?;
            *entry = table::ContextEntry::new_pass_through();
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.registers.is_enabled()
    }
}

// ── Global singleton ────────────────────────────────────────────────

static GLOBAL_IOMMU: Mutex<Option<IommuEngine>> = Mutex::new(None);

/// Physical-to-virtual mapping function set by [`init`] and used by ACPI parsing.
/// Defaults to the hardcoded offset for backward compatibility.
pub(crate) static PHYS_TO_VIRT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0xFFFF_8000_0000_0000);

/// Initialize IOMMU from ACPI DMAR table.
///
/// `rsdp_phys` — physical address of the ACPI RSDP (0 = auto-detect).
/// `phys_to_virt` — function to convert physical to virtual addresses.
/// `ctx` — DriverContext for frame allocation and MMIO mapping.
pub fn init(
    rsdp_phys: u64,
    phys_to_virt_fn: fn(u64) -> usize,
    ctx: &dyn DriverContext,
) -> Result<(), &'static str> {
    let rsdp = if rsdp_phys != 0 {
        if !acpi::find_rsdp_from_addr(rsdp_phys) {
            return Err("Invalid RSDP address");
        }
        rsdp_phys
    } else {
        acpi::find_rsdp().ok_or("RSDP not found")?
    };

    let dmar = acpi::parse_dmar(rsdp).ok_or("DMAR table not found")?;

    let drhd = dmar.drhd_units.first().ok_or("No DRHD entries")?;

    let bar_phys = drhd.phys_base;
    if bar_phys == 0 {
        return Err("Invalid DRHD register base");
    }
    let bar_size = (1 << 12) as usize; // VT-d MMIO is typically 4KB

    // Map the MMIO region
    let mmio_virt = (phys_to_virt_fn)(bar_phys);
    ctx.map_mmio_region(bar_phys as usize, mmio_virt, bar_size)
        .map_err(|_| "IOMMU BAR MMIO mapping failed")?;

    // Store the physical-to-virtual mapping for ACPI parsing
    // by encoding it as offset: phys_to_virt(phys) = phys + offset
    // We compute the offset by evaluating at phys=0: phys_to_virt_fn(0)
    let mapper_offset = (phys_to_virt_fn)(0) as u64;
    PHYS_TO_VIRT.store(mapper_offset, core::sync::atomic::Ordering::Relaxed);

    let mut engine = IommuEngine::new(mmio_virt as *mut u8, ctx, dmar.host_address_width)
        .map_err(|_| "IOMMU engine init failed")?;

    // Set up pass-through context entries for all enumerated PCI devices
    // before enabling translation, so non-IOMMU-aware drivers keep working.
    engine.setup_pass_through_all(ctx)
        .map_err(|_| "IOMMU pass-through setup failed")?;

    // Check if hardware is already enabled by firmware
    if engine.is_enabled() {
        log::warn!("IOMMU already enabled by firmware");
    }

    // Enable the IOMMU
    engine.enable();

    *GLOBAL_IOMMU.lock() = Some(engine);

    log::info!("IOMMU initialized successfully");
    Ok(())
}

/// Check if IOMMU has been successfully initialized.
pub fn is_initialized() -> bool {
    GLOBAL_IOMMU.lock().is_some()
}

/// Map a DMA buffer through the IOMMU. Falls back to identity mapping.
pub fn dma_map_with_ctx(
    ctx: &dyn DriverContext,
    device_id: u16,
    phys: u64,
    size: usize,
) -> Result<u64, DriverContextError> {
    let mut guard = GLOBAL_IOMMU.lock();
    if let Some(ref mut engine) = *guard {
        engine.dma_map(ctx, device_id, phys, size)
    } else {
        Ok(phys)
    }
}

/// Unmap a previously mapped DMA buffer. No-op if no IOMMU.
pub fn dma_unmap(ctx: &dyn DriverContext, iova: u64, size: usize) {
    let mut guard = GLOBAL_IOMMU.lock();
    if let Some(ref mut engine) = *guard {
        engine.dma_unmap(ctx, iova, size);
    }
}
