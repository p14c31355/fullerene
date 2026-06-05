use crate::memory_management::process_memory::ProcessMemoryManagerImpl;
use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::graphics::framebuffer_mapper::{CacheMode, FramebufferMapper};
use petroleum::initializer::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager, ProcessMemoryManager,
};
use petroleum::mem_debug;
use petroleum::page_table::{
    BitmapFrameAllocator, BootInfoFrameAllocator, FrameAllocatorExt, PageTableHelper,
    ProcessPageTable,
};
use x86_64::{
    PhysAddr,
    structures::paging::{
        FrameAllocator as X86FrameAllocator, Mapper, PageTableFlags as PageFlags, Size4KiB,
    },
};

const MAX_PROCESS_MANAGERS: usize = 16;

pub struct UnifiedMemoryManager {
    pub(crate) page_table_manager: ProcessPageTable,
    pub(crate) kernel_pml4_phys: usize,
    pub(crate) process_managers: alloc::vec::Vec<Option<ProcessMemoryManagerImpl>>,
    pub(crate) current_process: usize,
    pub(crate) initialized: bool,
}

impl UnifiedMemoryManager {
    pub fn safe_map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        // Snapshot current page table root BEFORE creating mapper,
        // which borrows self.page_table_manager.mapper mutably.
        let current_pml4 = self.page_table_manager.current_page_table();
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let virt = x86_64::VirtAddr::new(virtual_addr as u64);
        let phys = x86_64::PhysAddr::new(physical_addr as u64);
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt);
        let frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(phys);
        let frame_alloc = petroleum::page_table::constants::get_frame_allocator_mut();

        // Unmap any existing 4KB page first
        let _ = self.page_table_manager.unmap_page(virtual_addr);

        let mapper = self
            .page_table_manager
            .mapper
            .as_mut()
            .ok_or(SystemError::InternalError)?;

        match unsafe { mapper.map_to(page, frame, flags, frame_alloc) } {
            Ok(flush) => {
                flush.flush();
                Ok(())
            }
            Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {
                // The virtual address is covered by a 2MB huge page (from the
                // kernel area WB mapping).  We must split the huge page into
                // 512 4KB pages before we can overlay MMIO mappings with
                // different cache attributes (UC/WC).
                //
                // Page table walk for x86-64 4-level paging:
                //   P4 (PML4) → P3 (PDPT) → P2 (PD) → P1 (PT) → 4KB page
                //
                // The 2MB huge-page flag (PS=1) lives on the P2 (Page Directory)
                // entry, NOT on the P3 (PDPT) entry.  A P3 HUGE_PAGE would be a
                // 1GB huge page, which the kernel area mapping does NOT use.
                //
                // Strategy: walk P4→P3→P2, read the 2MB phys frame from the P2
                // entry, allocate a 4KB P1 page table with 512 4KB entries,
                // then replace the P2 entry to point at the new P1 table.
                let l4_table = unsafe {
                    &*((current_pml4 as u64 + off)
                        as *const x86_64::structures::paging::page_table::PageTable)
                };

                let p4_idx = virt.p4_index();
                let p3_idx = virt.p3_index();
                let p2_idx = virt.p2_index();
                let p1_idx = virt.p1_index();

                // Walk: P4 → P3
                let p4_entry = &l4_table[p4_idx];
                if p4_entry.is_unused() {
                    return Err(SystemError::MappingFailed);
                }
                let p3_phys = p4_entry.addr().as_u64();
                let p3_table = unsafe {
                    &*((p3_phys + off)
                        as *const x86_64::structures::paging::page_table::PageTable)
                };

                // Walk: P3 → P2
                let p3_entry = &p3_table[p3_idx];
                if p3_entry.is_unused() || p3_entry.flags().contains(PageFlags::HUGE_PAGE) {
                    // P3 has HUGE_PAGE → 1GB page, which won't happen for our
                    // kernel area mapping
                    return Err(SystemError::MappingFailed);
                }
                let p2_phys = p3_entry.addr().as_u64();
                let p2_table = unsafe {
                    &*((p2_phys + off)
                        as *const x86_64::structures::paging::page_table::PageTable)
                };

                // P2 entry: the actual 2MB huge-page descriptor
                let p2_entry = &p2_table[p2_idx];
                if !p2_entry.flags().contains(PageFlags::HUGE_PAGE) {
                    // Not a huge page — something is inconsistent
                    return Err(SystemError::MappingFailed);
                }

                let huge_phys = p2_entry.addr(); // 2MB-aligned physical base
                let huge_flags = p2_entry.flags();

                // Allocate a new P1 page table for the 512 4KB entries
                let p1_frame = frame_alloc
                    .allocate_frame()
                    .ok_or(SystemError::FrameAllocationFailed)?;
                let p1_phys = p1_frame.start_address().as_u64();
                let p1_table = unsafe {
                    &mut *((p1_phys + off)
                        as *mut x86_64::structures::paging::page_table::PageTable)
                };
                p1_table.zero();

                // Fill 512 P1 entries, each pointing to a 4KB slice of the
                // 2MB physical range, EXCEPT the target entry — leave it
                // NOT_PRESENT so the subsequent mapper.map_to can install
                // the new UC/WC flags without triggering PageAlreadyMapped.
                for i in 0..512 {
                    if i as u16 == p1_idx.into() {
                        continue;
                    }
                    let sub_phys = x86_64::PhysAddr::new(
                        huge_phys.as_u64() + (i * 4096),
                    );
                    let sub_frame =
                        x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
                            sub_phys,
                        );
                    let sub_flags = huge_flags
                        & !PageFlags::HUGE_PAGE
                        & !PageFlags::GLOBAL;
                    p1_table[i as usize]
                        .set_addr(sub_frame.start_address(), sub_flags);
                }

                // Replace the P2 entry: point to the new P1 table instead of
                // the 2MB huge frame
                let new_p2_flags = huge_flags
                    & !PageFlags::HUGE_PAGE
                    & !PageFlags::GLOBAL;
                let p2_table_mut = unsafe {
                    &mut *((p2_phys + off)
                        as *mut x86_64::structures::paging::page_table::PageTable)
                };
                p2_table_mut[p2_idx].set_addr(
                    p1_frame.start_address(),
                    new_p2_flags,
                );

                // Flush TLB for the 2MB range
                x86_64::instructions::tlb::flush_all();

                // Now retry the 4KB mapping over the freshly-split range.
                // The target P1 entry was left NOT_PRESENT so mapper.map_to
                // can install the new flags without PageAlreadyMapped.
                match unsafe { mapper.map_to(page, frame, flags, frame_alloc) } {
                    Ok(flush) => {
                        flush.flush();
                        Ok(())
                    }
                    Err(e) => {
                        petroleum::serial::serial_log(format_args!(
                            "[safe_map_page] Retry after 2MB split failed virt={:#x}: {:?}\n",
                            virtual_addr, e
                        ));
                        Err(SystemError::MappingFailed)
                    }
                }
            }
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                // Already mapped — acceptable for MMIO regions
                Ok(())
            }
            Err(e) => {
                petroleum::serial::serial_log(format_args!(
                    "[safe_map_page] Failed virt={:#x} phys={:#x}: {:?}\n",
                    virtual_addr, physical_addr, e
                ));
                Err(SystemError::MappingFailed)
            }
        }
    }

    pub fn safe_unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Ok(frame) = self.page_table_manager.unmap_page(virtual_addr) {
            self.free_frame(frame.start_address().as_u64() as usize)?;
        }
        Ok(())
    }

    /// Unmap a page without freeing the underlying physical frame.
    /// Used for device-backed memory (MMIO/framebuffer) that should not be returned to RAM allocator.
    pub fn safe_unmap_page_no_free(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        // Only remove the page table entry, don't free the physical frame
        let _ = self.page_table_manager.unmap_page(virtual_addr);
        Ok(())
    }

    /// Legacy MMIO mapping — delegates to [`FramebufferMapper::map_framebuffer`].
    pub fn map_mmio_region(
        &mut self,
        physical_addr: usize,
        virtual_addr: usize,
        size: usize,
    ) -> SystemResult<()> {
        if !self.map_framebuffer_region(
            physical_addr as u64,
            virtual_addr as u64,
            size,
            CacheMode::Uncached,
        ) {
            return Err(SystemError::MappingFailed);
        }
        Ok(())
    }

    // ── FramebufferMapper internals ────────────────────────────

    fn map_framebuffer_region(
        &mut self,
        phys_addr: u64,
        virt_addr: u64,
        size: usize,
        cache: CacheMode,
    ) -> bool {
        if !self.initialized {
            return false;
        }
        let flags = Self::cache_flags(cache)
            | PageFlags::PRESENT
            | PageFlags::WRITABLE
            | PageFlags::NO_EXECUTE;
        let page_size = self.page_size();
        let pages = (size + page_size - 1) / page_size;
        let mut mapped_pages: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
        for i in 0..pages {
            let v = virt_addr + (i * page_size) as u64;
            let p = phys_addr + (i * page_size) as u64;
            if self.safe_map_page(v as usize, p as usize, flags).is_err() {
                // Rollback: unmap all successfully mapped pages without freeing frames
                for &mapped_v in &mapped_pages {
                    let _ = self.safe_unmap_page_no_free(mapped_v);
                }
                let _ = self.flush_tlb_all();
                return false;
            }
            mapped_pages.push(v as usize);
        }
        let _ = self.flush_tlb_all();
        true
    }

    fn cache_flags(mode: CacheMode) -> PageFlags {
        match mode {
            // Uncached: PCD=1 (NO_CACHE) + PWT=0 → UC- (or UC if MTRR says UC)
            CacheMode::Uncached => PageFlags::NO_CACHE,
            // WriteCombining: PCD=0 + PWT=1 (WRITE_THROUGH) → WC via PAT default
            // (PAT reset-default PA1 = 0b001 = WC).  Combined with MTRR UC on
            // PCI MMIO frames, the effective type is WC — safe for framebuffer
            // and won't #GP on InsydeH2O.
            CacheMode::WriteCombining => PageFlags::WRITE_THROUGH,
            // WriteBack: both PCD/PWT=0 → WB via PAT default
            CacheMode::WriteBack => PageFlags::empty(),
        }
    }

    pub fn new() -> Self {
        Self {
            page_table_manager: ProcessPageTable::new(),
            kernel_pml4_phys: 0,
            process_managers: alloc::vec::Vec::new(),
            current_process: 0,
            initialized: false,
        }
    }

    fn find_process_index(&self, process_id: usize) -> Option<usize> {
        self.process_managers
            .iter()
            .position(|pm| pm.as_ref().map_or(false, |m| m.process_id() == process_id))
    }

    pub fn init(
        &mut self,
        memory_map: &[impl petroleum::page_table::types::MemoryDescriptorValidator],
    ) -> SystemResult<()> {
        mem_debug!("UMM: init start\n");
        {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let heap_allocator = fa_guard
                .take()
                .expect("Frame allocator must be initialized by uefi_init");
            petroleum::page_table::constants::init_frame_allocator(heap_allocator);
        }
        mem_debug!("UMM: Frame allocator transferred\n");

        let phys_offset =
            x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
        let kernel_virt: u64;
        unsafe {
            core::arch::asm!("lea {}, [rip]", out(reg) kernel_virt);
        }
        let kernel_phys = (kernel_virt - phys_offset.as_u64()) & !4095;

        self.page_table_manager.initialize_with_frame_allocator(
            phys_offset,
            petroleum::page_table::constants::get_frame_allocator_mut(),
            kernel_phys,
        )?;
        self.kernel_pml4_phys = self.page_table_manager.current_page_table();

        let kernel_reserve_pages = (16 * 1024 * 1024) / 4096;
        let _ = petroleum::page_table::constants::get_frame_allocator_mut()
            .reserve_frames(kernel_phys, kernel_reserve_pages);
        mem_debug!("UMM: Kernel memory reserved\n");

        mem_debug!("UMM: Mapping physical memory direct map\n");
        let phys_offset_virt =
            x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
        let frame_alloc = petroleum::page_table::constants::get_frame_allocator_mut();

        for descriptor in memory_map {
            let phys_addr = descriptor.get_physical_start();
            let pages = descriptor.get_page_count();
            let base_virt_addr =
                (phys_offset_virt + PhysAddr::new(phys_addr).as_u64()).as_u64() as usize;

            for i in 0..pages {
                let page_size = self.page_size();
                let i_usize = i as usize;
                let virt = base_virt_addr + (i_usize * page_size);
                let phys = (phys_addr + (i * page_size as u64)) as usize;
                let virt_addr = x86_64::VirtAddr::new(virt as u64);
                let phys_addr_val = x86_64::PhysAddr::new(phys as u64);
                let page =
                    x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt_addr);
                let frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
                    phys_addr_val,
                );
                let flags = PageFlags::PRESENT | PageFlags::WRITABLE;

                let mapper = self
                    .page_table_manager
                    .mapper
                    .as_mut()
                    .ok_or(SystemError::InternalError)?;
                match unsafe { mapper.map_to(page, frame, flags, frame_alloc) } {
                    Ok(flush) => flush.flush(),
                    Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage)
                    | Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    }
                    Err(_) => return Err(SystemError::MappingFailed),
                }
            }
        }
        mem_debug!("UMM: Physical memory direct mapping complete\n");

        self.create_address_space(0)?;
        mem_debug!("UMM: Kernel address space created\n");
        self.initialized = true;
        mem_debug!("UMM: Fully initialized\n");
        Ok(())
    }

    pub fn frame_allocator(&self) -> &BitmapFrameAllocator {
        unsafe {
            &*(petroleum::page_table::constants::get_frame_allocator() as *const _
                as *const BitmapFrameAllocator)
        }
    }
    pub fn frame_allocator_mut(&mut self) -> &mut BitmapFrameAllocator {
        petroleum::page_table::constants::get_frame_allocator_mut()
    }
    pub fn page_table_manager(&self) -> &ProcessPageTable {
        &self.page_table_manager
    }
    pub fn page_table_manager_mut(&mut self) -> &mut ProcessPageTable {
        &mut self.page_table_manager
    }
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

// ── FramebufferMapper impl ────────────────────────────────────

impl FramebufferMapper for UnifiedMemoryManager {
    fn map_framebuffer(&mut self, phys_addr: u64, size: usize, cache: CacheMode) -> Option<u64> {
        if !self.initialized {
            return None;
        }
        let pages = (size + 4095) / 4096;
        let virt_base = crate::memory_management::kernel_space::find_free_virtual_address(
            (pages * 4096) as u64,
        )? as u64;
        if self.map_framebuffer_region(phys_addr, virt_base, size, cache) {
            Some(virt_base)
        } else {
            None
        }
    }

    fn unmap_framebuffer(&mut self, virt_addr: u64, size: usize) {
        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            // Use non-freeing unmap for device-backed memory
            let _ = self.safe_unmap_page_no_free((virt_addr + (i * 4096) as u64) as usize);
        }
    }
}

// ── MemoryManager trait ──────────────────────────────────────

impl MemoryManager for UnifiedMemoryManager {
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let page_size = self.page_size();
        let total_virt_pages = count + 2;
        let virtual_addr_base = crate::memory_management::kernel_space::find_free_virtual_address(
            total_virt_pages as u64 * page_size as u64,
        )
        .ok_or(SystemError::MemOutOfMemory)?;

        let frame_addr = petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(count)? as usize;
        let data_virt_addr = virtual_addr_base + page_size;
        for i in 0..count {
            self.safe_map_page(
                data_virt_addr + i * page_size,
                frame_addr + i * page_size,
                PageFlags::PRESENT | PageFlags::WRITABLE,
            )?;
        }
        Ok(data_virt_addr)
    }

    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let page_size = self.page_size();
        for i in 0..count {
            let virt_addr = address + (i * page_size);
            if let Ok(frame) = self.page_table_manager.unmap_page(virt_addr) {
                self.free_frame(frame.start_address().as_u64() as usize)?;
            }
        }
        let _ = self
            .page_table_manager
            .unmap_page(address.saturating_sub(page_size));
        let _ = self
            .page_table_manager
            .unmap_page(address + (count * page_size));
        Ok(())
    }

    fn total_memory(&self) -> usize {
        self.frame_allocator().total_frames() * self.frame_allocator().frame_size()
    }
    fn available_memory(&self) -> usize {
        self.frame_allocator().available_frames() * self.frame_allocator().frame_size()
    }

    fn map_address(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        count: usize,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let page_size = self.page_size();
        for i in 0..count {
            self.safe_map_page(
                virtual_addr + i * page_size,
                physical_addr + i * page_size,
                PageFlags::PRESENT | PageFlags::WRITABLE,
            )?;
        }
        Ok(())
    }

    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        for i in 0..count {
            let _ = self.page_table_manager.unmap_page(virtual_addr + i * 4096);
        }
        Ok(())
    }

    fn virtual_to_physical(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.translate_address(virtual_addr)
    }

    fn init_paging(&mut self) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.init_paging()
    }

    fn page_size(&self) -> usize {
        4096
    }
}

// ── ProcessMemoryManager trait ───────────────────────────────

impl ProcessMemoryManager for UnifiedMemoryManager {
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        mem_debug!("UMM: create_address_space entered\n");
        if self.find_process_index(process_id).is_some() {
            return Ok(());
        }
        let mut process_manager = ProcessMemoryManagerImpl::new(process_id);
        process_manager.init_page_table(
            &mut self.page_table_manager,
            petroleum::page_table::constants::get_frame_allocator_mut(),
        )?;
        if self.process_managers.len() >= MAX_PROCESS_MANAGERS {
            return Err(SystemError::TooManyProcesses);
        }
        self.process_managers.push(Some(process_manager));
        mem_debug!("UMM: Created address space for process\n");
        Ok(())
    }

    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(process_id) {
            let process_manager = self.process_managers[idx].as_ref().unwrap();
            self.current_process = process_id;
            self.page_table_manager
                .switch_page_table(process_manager.page_table_root())?;
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(process_id) {
            if let Some(mut process_manager) = self.process_managers[idx].take() {
                process_manager.cleanup()?;
            }
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(self.current_process) {
            if let Some(pm) = self.process_managers[idx].as_mut() {
                return pm.allocate_heap(size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(self.current_process) {
            if let Some(pm) = self.process_managers[idx].as_mut() {
                return pm.free_heap(address, size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(self.current_process) {
            if let Some(pm) = self.process_managers[idx].as_mut() {
                return pm.allocate_stack(size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if let Some(idx) = self.find_process_index(self.current_process) {
            if let Some(pm) = self.process_managers[idx].as_mut() {
                return pm.free_stack(address, size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn copy_memory_between_processes(
        &mut self,
        from_process: usize,
        to_process: usize,
        from_addr: usize,
        to_addr: usize,
        size: usize,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let current_process = self.current_process;
        self.switch_address_space(from_process)?;
        let source_data = self.copy_from_user_space(from_addr, size)?;
        self.switch_address_space(to_process)?;
        self.copy_to_user_space(to_addr, &source_data)?;
        self.switch_address_space(current_process)?;
        Ok(())
    }

    fn current_process_id(&self) -> usize {
        self.current_process
    }
}

// ── PageTableHelper trait ────────────────────────────────────

impl PageTableHelper for UnifiedMemoryManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
        frame_allocator: &mut impl X86FrameAllocator<Size4KiB>,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager
            .map_page(virtual_addr, physical_addr, flags, frame_allocator)
    }
    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.unmap_page(virtual_addr)
    }
    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.translate_address(virtual_addr)
    }
    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.set_page_flags(virtual_addr, flags)
    }
    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<PageFlags> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.get_page_flags(virtual_addr)
    }
    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.flush_tlb(virtual_addr)
    }
    fn flush_tlb_all(&mut self) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.flush_tlb_all()
    }
    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl X86FrameAllocator<Size4KiB>,
    ) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.create_page_table(frame_allocator)
    }
    fn destroy_page_table(
        &mut self,
        table_addr: usize,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager
            .destroy_page_table(table_addr, frame_allocator)
    }
    fn clone_page_table(
        &mut self,
        source_table: usize,
        frame_allocator: &mut impl X86FrameAllocator<Size4KiB>,
    ) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager
            .clone_page_table(source_table, frame_allocator)
    }
    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        self.page_table_manager.switch_page_table(table_addr)
    }
    fn current_page_table(&self) -> usize {
        self.page_table_manager.current_page_table()
    }
}

// ── FrameAllocator trait ─────────────────────────────────────

impl FrameAllocator for UnifiedMemoryManager {
    fn allocate_frame(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_frame()
            .map(|f| f.start_address().as_u64() as usize)
            .ok_or(SystemError::FrameAllocationFailed)
    }
    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut().free_frame(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(
                frame_addr as u64,
            )),
        );
        Ok(())
    }
    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(count)
            .map(|addr| addr as usize)
    }
    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .free_contiguous_frames(start_addr as u64, count);
        Ok(())
    }
    fn total_frames(&self) -> usize {
        self.frame_allocator().total_frames()
    }
    fn available_frames(&self) -> usize {
        self.frame_allocator().available_frames()
    }
    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .reserve_frames(start_addr as u64, count)
    }
    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .release_frames(start_addr as u64, count);
        Ok(())
    }
    fn is_frame_available(&self, frame_addr: usize) -> bool {
        petroleum::page_table::constants::get_frame_allocator().is_frame_available(frame_addr)
    }
    fn frame_size(&self) -> usize {
        petroleum::page_table::constants::get_frame_allocator().frame_size()
    }
}

// ── Initializable + ErrorLogging ─────────────────────────────

impl Initializable for UnifiedMemoryManager {
    fn init(&mut self) -> SystemResult<()> {
        let dummy: &[petroleum::page_table::EfiMemoryDescriptor] = &[];
        self.init(dummy)
    }
    fn name(&self) -> &'static str {
        "UnifiedMemoryManager"
    }
    fn priority(&self) -> i32 {
        1000
    }
}

impl ErrorLogging for UnifiedMemoryManager {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log::error!("SystemError({}): {}", *error as u32, context);
    }
    fn log_warning(&self, message: &'static str) {
        log::warn!("{}", message);
    }
    fn log_info(&self, message: &'static str) {
        log::info!("{}", message);
    }
    fn log_debug(&self, message: &'static str) {
        log::debug!("{}", message);
    }
    fn log_trace(&self, message: &'static str) {
        log::trace!("{}", message);
    }
}

impl UnifiedMemoryManager {
    fn find_free_virtual_address(&self, size: usize) -> SystemResult<usize> {
        crate::memory_management::kernel_space::find_free_virtual_address(size as u64)
            .ok_or(SystemError::MemOutOfMemory)
    }

    fn copy_from_user_space(
        &mut self,
        user_addr: usize,
        size: usize,
    ) -> SystemResult<alloc::vec::Vec<u8>> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let mut data = alloc::vec::Vec::with_capacity(size);
        let page_size = self.page_size();
        for offset in (0..size).step_by(page_size) {
            let current_chunk_size = core::cmp::min(page_size, size - offset);
            let virt_addr = user_addr + offset;
            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                let phys_base = phys_addr + (virt_addr % page_size);
                unsafe {
                    let slice =
                        petroleum::common::memory::phys_to_slice(phys_base, current_chunk_size);
                    data.extend_from_slice(slice);
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }
        Ok(data)
    }

    fn copy_to_user_space(&mut self, user_addr: usize, data: &[u8]) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        let page_size = self.page_size();
        for (i, chunk) in data.chunks(page_size).enumerate() {
            let offset = i * page_size;
            let virt_addr = user_addr + offset;
            if self
                .page_table_manager
                .translate_address(virt_addr)
                .is_err()
            {
                let frame = petroleum::page_table::constants::get_frame_allocator_mut()
                    .allocate_frame()
                    .ok_or(SystemError::FrameAllocationFailed)?;
                self.page_table_manager.map_page(
                    virt_addr,
                    frame.start_address().as_u64() as usize,
                    PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER_ACCESSIBLE,
                    petroleum::page_table::constants::get_frame_allocator_mut(),
                )?;
            }
            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                let phys_base = phys_addr + (virt_addr % page_size);
                unsafe {
                    let slice =
                        petroleum::common::memory::phys_to_slice_mut(phys_base, chunk.len());
                    slice.copy_from_slice(chunk);
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }
        Ok(())
    }
}
