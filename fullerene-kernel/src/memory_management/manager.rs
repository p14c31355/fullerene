use crate::memory_management::process_memory::ProcessMemoryManagerImpl;
use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::initializer::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager, ProcessMemoryManager,
};
use petroleum::mem_debug;
use petroleum::page_table::{
    BitmapFrameAllocator, BootInfoFrameAllocator, FrameAllocatorExt, MemoryMapDescriptor,
    PageTableHelper, ProcessPageTable,
};
use x86_64::{PhysAddr, structures::paging::{
    FrameAllocator as X86FrameAllocator, PageTableFlags as PageFlags, Size4KiB,
}};

/// Unified memory manager implementing all memory management traits
pub struct UnifiedMemoryManager {
    pub(crate) page_table_manager: ProcessPageTable,
    pub(crate) kernel_pml4_phys: usize,
    // Temporarily use a fixed array to avoid BTreeMap allocation during early boot
    pub(crate) process_managers: [Option<ProcessMemoryManagerImpl>; 16],
    pub(crate) current_process: usize,
    pub(crate) initialized: bool,
}

impl UnifiedMemoryManager {
    /// Safely maps a page, ensuring any existing mapping is removed first.
    pub fn safe_map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // Remove existing mapping if present
        let _ = self.page_table_manager.unmap_page(virtual_addr);

        self.page_table_manager.map_page(
            virtual_addr,
            physical_addr,
            flags,
            petroleum::page_table::constants::get_frame_allocator_mut(),
        )
    }

    /// Safely unmaps a page, ignoring errors if the page was not mapped.
    pub fn safe_unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Ok(frame) = self.page_table_manager.unmap_page(virtual_addr) {
            self.free_frame(frame.start_address().as_u64() as usize)?;
        }
        Ok(())
    }

    /// Safely maps a physical region to a virtual address, specifically for MMIO/Framebuffer.
    /// Ensures the region is marked as NO_EXECUTE and PRESENT | WRITABLE.
    pub fn map_mmio_region(
        &mut self,
        physical_addr: usize,
        virtual_addr: usize,
        size: usize,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let page_size = self.page_size();
        let pages = (size + page_size - 1) / page_size;
        
        // MMIO regions are typically mapped as non-executable
        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;

        for i in 0..pages {
            self.safe_map_page(
                virtual_addr + i * page_size,
                physical_addr + i * page_size,
                flags,
            )?;
        }

        Ok(())
    }

    /// Create a new unified memory manager
    pub fn new() -> Self {
        const NONE_MANAGER: Option<ProcessMemoryManagerImpl> = None;
        Self {
            page_table_manager: ProcessPageTable::new(),
            kernel_pml4_phys: 0,
            process_managers: [NONE_MANAGER; 16],
            current_process: 0,
            initialized: false,
        }
    }

    /// Initialize the memory management system
    pub fn init(
        &mut self,
        memory_map: &[impl petroleum::page_table::types::MemoryDescriptorValidator],
    ) -> SystemResult<()> {
        mem_debug!("UMM: init start\n");

        // Transfer the frame allocator from heap (initialized during uefi_init) to constants.
        {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let heap_allocator = fa_guard.take()
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

        let kernel_phys_start = kernel_virt & !4095;

        self.page_table_manager.initialize_with_frame_allocator(
            phys_offset,
            petroleum::page_table::constants::get_frame_allocator_mut(),
            kernel_phys_start,
        )?;
        self.kernel_pml4_phys = self.page_table_manager.current_page_table();

        let _ = phys_offset;

        let kernel_reserve_pages = (16 * 1024 * 1024) / 4096;
        let _ = petroleum::page_table::constants::get_frame_allocator_mut()
            .reserve_frames(kernel_phys_start, kernel_reserve_pages);
        mem_debug!("UMM: Kernel memory reserved\n");

        mem_debug!("UMM: Mapping physical memory direct map\n");
        for descriptor in memory_map {
            let phys_addr = descriptor.get_physical_start();
            let pages = descriptor.get_page_count();

            let phys_offset = x86_64::VirtAddr::new(
                petroleum::common::memory::get_physical_memory_offset() as u64
            );
            let base_virt_addr = (phys_offset + PhysAddr::new(phys_addr).as_u64()).as_u64() as usize;

            for i in 0..pages {
                let page_size = self.page_size();
                let virt = base_virt_addr + (i as usize * page_size);
                let phys = (phys_addr + (i as u64 * page_size as u64)) as usize;

                let res = self.page_table_manager.map_page(
                    virt,
                    phys,
                    PageFlags::PRESENT | PageFlags::WRITABLE,
                    petroleum::page_table::constants::get_frame_allocator_mut(),
                );

                if let Err(e) = res {
                    if e == SystemError::MappingFailed {
                        continue;
                    } else {
                        return Err(e);
                    }
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
            let ptr = petroleum::page_table::constants::get_frame_allocator() as *const _ as *const BitmapFrameAllocator;
            &*ptr
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

// Implementation of base MemoryManager trait
impl MemoryManager for UnifiedMemoryManager {
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        
        let page_size = self.page_size();
        let total_virt_pages = count + 2;
        let virtual_addr_base =
            crate::memory_management::kernel_space::find_free_virtual_address(total_virt_pages as u64 * page_size as u64)
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
                let phys_addr = frame.start_address().as_u64() as usize;
                self.free_frame(phys_addr)?;
            }
        }

        let _ = self.page_table_manager.unmap_page(address.saturating_sub(page_size));
        let _ = self.page_table_manager.unmap_page(address + (count * page_size));

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
            let v = virtual_addr + i * 4096;
            let _ = self.page_table_manager.unmap_page(v);
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

// Implementation of ProcessMemoryManager trait
impl ProcessMemoryManager for UnifiedMemoryManager {
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        mem_debug!("UMM: create_address_space entered\n");

        if process_id >= 16 {
            return Err(SystemError::InvalidArgument);
        }

        let mut process_manager = ProcessMemoryManagerImpl::new(process_id);
        process_manager.init_page_table(&mut self.page_table_manager, petroleum::page_table::constants::get_frame_allocator_mut())?;

        self.process_managers[process_id] = Some(process_manager);
        mem_debug!("UMM: Created address space for process\n");
        Ok(())
    }

    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if process_id < 16 {
            if let Some(process_manager) = &self.process_managers[process_id] {
                self.current_process = process_id;
                self.page_table_manager
                    .switch_page_table(process_manager.page_table_root())?;
                return Ok(());
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if process_id < 16 {
            if let Some(mut process_manager) = self.process_managers[process_id].take() {
                process_manager.cleanup()?;
                return Ok(());
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if self.current_process < 16 {
            if let Some(pm) = &mut self.process_managers[self.current_process] {
                return pm.allocate_heap(size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if self.current_process < 16 {
            if let Some(pm) = &mut self.process_managers[self.current_process] {
                return pm.free_heap(address, size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if self.current_process < 16 {
            if let Some(pm) = &mut self.process_managers[self.current_process] {
                return pm.allocate_stack(size);
            }
        }
        Err(SystemError::NoSuchProcess)
    }

    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        if self.current_process < 16 {
            if let Some(pm) = &mut self.process_managers[self.current_process] {
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

// Implementation of PageTableHelper trait
impl PageTableHelper for UnifiedMemoryManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.map_page(virtual_addr, physical_addr, flags, frame_allocator)
    }

    fn unmap_page(&mut self, virtual_addr: usize) -> SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.unmap_page(virtual_addr)
    }

    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.translate_address(virtual_addr)
    }

    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.set_page_flags(virtual_addr, flags)
    }

    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<PageFlags> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.get_page_flags(virtual_addr)
    }

    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.flush_tlb(virtual_addr)
    }

    fn flush_tlb_all(&mut self) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.flush_tlb_all()
    }

    fn create_page_table(&mut self, frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>) -> SystemResult<usize> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.create_page_table(frame_allocator)
    }

    fn destroy_page_table(&mut self, table_addr: usize, frame_allocator: &mut BootInfoFrameAllocator) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.destroy_page_table(table_addr, frame_allocator)
    }

    fn clone_page_table(&mut self, source_table: usize, frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>) -> SystemResult<usize> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.clone_page_table(source_table, frame_allocator)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        self.page_table_manager.switch_page_table(table_addr)
    }

    fn current_page_table(&self) -> usize {
        self.page_table_manager.current_page_table()
    }
}

// Implementation of FrameAllocator trait
impl FrameAllocator for UnifiedMemoryManager {
    fn allocate_frame(&mut self) -> SystemResult<usize> {
        if !self.initialized { return Err(SystemError::InternalError); }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_frame()
            .map(|f| f.start_address().as_u64() as usize)
            .ok_or(SystemError::FrameAllocationFailed)
    }

    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .free_frame(x86_64::structures::paging::PhysFrame::containing_address(
                x86_64::PhysAddr::new(frame_addr as u64),
            ));
        Ok(())
    }

    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized { return Err(SystemError::InternalError); }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(count)
            .map(|addr| addr as usize)
    }

    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .free_contiguous_frames(start_addr as u64, count);
        Ok(())
    }

    fn total_frames(&self) -> usize { self.frame_allocator().total_frames() }
    fn available_frames(&self) -> usize { self.frame_allocator().available_frames() }

    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
        petroleum::page_table::constants::get_frame_allocator_mut()
            .reserve_frames(start_addr as u64, count)
    }

    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }
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

// Implementation of Initializable trait
impl Initializable for UnifiedMemoryManager {
    fn init(&mut self) -> SystemResult<()> {
        let dummy_memory_map: &[petroleum::page_table::EfiMemoryDescriptor] = &[];
        self.init(dummy_memory_map)
    }

    fn name(&self) -> &'static str { "UnifiedMemoryManager" }
    fn priority(&self) -> i32 { 1000 }
}

// Implementation of ErrorLogging trait
impl ErrorLogging for UnifiedMemoryManager {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log::error!("SystemError({}): {}", *error as u32, context);
    }
    fn log_warning(&self, message: &'static str) { log::warn!("{}", message); }
    fn log_info(&self, message: &'static str) { log::info!("{}", message); }
    fn log_debug(&self, message: &'static str) { log::debug!("{}", message); }
    fn log_trace(&self, message: &'static str) { log::trace!("{}", message); }
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
        if !self.initialized { return Err(SystemError::InternalError); }

        let mut data = alloc::vec::Vec::with_capacity(size);

        let page_size = self.page_size();
        for offset in (0..size).step_by(page_size) {
            let current_chunk_size = core::cmp::min(page_size, size - offset);
            let virt_addr = user_addr + offset;

            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                let phys_base = phys_addr + (virt_addr % page_size);
                unsafe {
                    let slice = petroleum::common::memory::phys_to_slice(phys_base, current_chunk_size);
                    data.extend_from_slice(slice);
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }

        Ok(data)
    }

    fn copy_to_user_space(&mut self, user_addr: usize, data: &[u8]) -> SystemResult<()> {
        if !self.initialized { return Err(SystemError::InternalError); }

        let page_size = self.page_size();
        for (i, chunk) in data.chunks(page_size).enumerate() {
            let offset = i * page_size;
            let virt_addr = user_addr + offset;

            if self.page_table_manager.translate_address(virt_addr).is_err() {
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
                    let slice = petroleum::common::memory::phys_to_slice_mut(phys_base, chunk.len());
                    slice.copy_from_slice(chunk);
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }

        Ok(())
    }
}