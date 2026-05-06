use alloc::collections::BTreeMap;
use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::page_table::{BitmapFrameAllocator, PageTableManager, PageTableHelper};
use crate::memory_management::process_memory::ProcessMemoryManagerImpl;
use petroleum::initializer::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager, ProcessMemoryManager,
};
use x86_64::structures::paging::{Page, PageTableFlags as PageFlags, Size4KiB};

/// Unified memory manager implementing all memory management traits
pub struct UnifiedMemoryManager {
    pub(crate) frame_allocator: BitmapFrameAllocator,
    pub(crate) page_table_manager: PageTableManager<'static>,
    pub(crate) process_managers: BTreeMap<usize, ProcessMemoryManagerImpl>,
    pub(crate) current_process: usize,
    pub(crate) initialized: bool,
}

impl UnifiedMemoryManager {
    /// Create a new unified memory manager
    pub fn new() -> Self {
        Self {
            frame_allocator: BitmapFrameAllocator::new(),
            page_table_manager: PageTableManager::new(),
            process_managers: BTreeMap::new(),
            current_process: 0,
            initialized: false,
        }
    }

    /// Initialize the memory management system
    pub fn init(
        &mut self,
        memory_map: &[impl petroleum::page_table::efi_memory::MemoryDescriptorValidator],
    ) -> SystemResult<()> {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"UMM::init start\n");

        unsafe { self.frame_allocator.init_with_memory_map(memory_map)? };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Frame allocator init done\n");

        // Initialize global heap before creating any BTreeMap or using alloc
        // Use a statically allocated buffer to avoid page faults during early boot
        let heap_size = crate::heap::HEAP_SIZE;
        let heap_ptr = unsafe { core::ptr::addr_of_mut!(crate::heap::BOOT_HEAP_BUFFER) as *mut u8 };
        
        // TEST: Verify if the static buffer is actually writable before initializing the heap
        unsafe {
            core::ptr::write(heap_ptr, 0xAA);
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Static buffer raw write success\n");
        }
        
        petroleum::init_global_heap(heap_ptr, heap_size);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Global heap initialized (static buffer)\n");


        // First 1MB is already reserved inside BitmapFrameAllocator::init_with_memory_map
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"First 1MB reserved\n");

        // Use the full initialization method to set up the mapper
        let phys_offset = x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
        self.page_table_manager.initialize_with_frame_allocator(phys_offset, &mut self.frame_allocator)?;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Page table manager initialized\n");

        self.create_address_space(0)?;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Kernel address space created\n");

        self.initialized = true;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"UnifiedMemoryManager fully initialized\n");
        Ok(())
    }

    /// Get frame allocator reference
    pub fn frame_allocator(&self) -> &BitmapFrameAllocator {
        &self.frame_allocator
    }

    /// Get frame allocator mutable reference
    pub fn frame_allocator_mut(&mut self) -> &mut BitmapFrameAllocator {
        &mut self.frame_allocator
    }

    /// Get page table manager reference
    pub fn page_table_manager(&self) -> &PageTableManager {
        &self.page_table_manager
    }

    /// Get page table manager mutable reference
    pub fn page_table_manager_mut(&mut self) -> &mut PageTableManager<'static> {
        &mut self.page_table_manager
    }

    /// Check if memory manager is initialized
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
        // Allocate physical frames
        let frame_addr = self.frame_allocator.allocate_contiguous_frames(count)?;

        // Map to kernel virtual address space
        let virtual_addr = crate::memory_management::kernel_space::find_free_virtual_address(count * 4096)?;

        petroleum::map_page_range!(
            self.page_table_manager,
            &mut self.frame_allocator,
            virtual_addr,
            frame_addr,
            count,
            PageFlags::PRESENT | PageFlags::WRITABLE
        );

        Ok(virtual_addr)
    }

    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        // Get physical addresses and free frames
        for i in 0..count {
            let virt_addr = address + (i * 4096);
            let frame = self.page_table_manager.unmap_page(virt_addr)?;

            let phys_addr = frame.start_address().as_u64() as usize;
            self.frame_allocator.free_frame(phys_addr)?;
        }

        Ok(())
    }

    fn total_memory(&self) -> usize {
        self.frame_allocator.total_frames() * self.frame_allocator.frame_size()
    }

    fn available_memory(&self) -> usize {
        self.frame_allocator.available_frames() * self.frame_allocator.frame_size()
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
        petroleum::map_page_range!(
            self.page_table_manager,
            &mut self.frame_allocator,
            virtual_addr,
            physical_addr,
            count,
            PageFlags::PRESENT | PageFlags::WRITABLE
        );
        Ok(())
    }

    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }
        petroleum::unmap_page_range!(self.page_table_manager, virtual_addr, count);
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
        // Allow creation during initialization phase
        let process_manager = ProcessMemoryManagerImpl::new(process_id);
        self.process_managers.insert(process_id, process_manager);

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Created address space for process\n");
        Ok(())
    }

    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get(&process_id) {
            self.current_process = process_id;
            self.page_table_manager
                .switch_page_table(process_manager.page_table_root())?;
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Switched to process address space\n");
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(mut process_manager) = self.process_managers.remove(&process_id) {
            process_manager.cleanup()?;
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Destroyed address space for process\n");
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.allocate_heap(size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.free_heap(address, size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.allocate_stack(size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.free_stack(address, size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
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

    fn create_page_table(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.create_page_table()
    }

    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.destroy_page_table(table_addr)
    }

    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.clone_page_table(source_table)
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

// Implementation of FrameAllocator trait
impl FrameAllocator for UnifiedMemoryManager {
    fn allocate_frame(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.allocate_frame()
    }

    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.free_frame(frame_addr)
    }

    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.allocate_contiguous_frames(count)
    }

    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator
            .free_contiguous_frames(start_addr, count)
    }

    fn total_frames(&self) -> usize {
        self.frame_allocator.total_frames()
    }

    fn available_frames(&self) -> usize {
        self.frame_allocator.available_frames()
    }

    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.reserve_frames(start_addr, count)
    }

    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.release_frames(start_addr, count)
    }

    fn is_frame_available(&self, frame_addr: usize) -> bool {
        self.frame_allocator.is_frame_available(frame_addr)
    }

    fn frame_size(&self) -> usize {
        self.frame_allocator.frame_size()
    }
}

// Implementation of Initializable trait
impl Initializable for UnifiedMemoryManager {
    fn init(&mut self) -> SystemResult<()> {
        let dummy_memory_map: &[petroleum::page_table::EfiMemoryDescriptor] = &[];
        self.init(dummy_memory_map)
    }

    fn name(&self) -> &'static str {
        "UnifiedMemoryManager"
    }

    fn priority(&self) -> i32 {
        1000
    }
}

// Implementation of ErrorLogging trait
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
        crate::memory_management::kernel_space::find_free_virtual_address(size)
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

        for offset in (0..size).step_by(4096) {
            let page_size = core::cmp::min(4096, size - offset);
            let virt_addr = user_addr + offset;

            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                self.page_table_manager.map_page(
                    super::TEMP_PHY_ACCESS,
                    phys_addr,
                    PageFlags::PRESENT,
                    &mut self.frame_allocator,
                )?;
                unsafe {
                    let ptr = (super::TEMP_PHY_ACCESS + (offset % 4096)) as *const u8;
                    let slice = core::slice::from_raw_parts(ptr, page_size);
                    data.extend_from_slice(slice);
                }
                let _ = self.page_table_manager.unmap_page(super::TEMP_PHY_ACCESS)?;
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

        for (i, chunk) in data.chunks(4096).enumerate() {
            let offset = i * 4096;
            let virt_addr = user_addr + offset;

            if self
                .page_table_manager
                .translate_address(virt_addr)
                .is_err()
            {
                let frame = self.frame_allocator.allocate_frame()?;
                self.page_table_manager.map_page(
                    virt_addr,
                    frame,
                    PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER_ACCESSIBLE,
                    &mut self.frame_allocator,
                )?;
            }

            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                self.page_table_manager.map_page(
                    super::TEMP_PHY_ACCESS,
                    phys_addr,
                    PageFlags::PRESENT | PageFlags::WRITABLE,
                    &mut self.frame_allocator,
                )?;
                unsafe {
                    let ptr = (super::TEMP_PHY_ACCESS + (offset % 4096)) as *mut u8;
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), ptr, chunk.len());
                }
                let _ = self.page_table_manager.unmap_page(super::TEMP_PHY_ACCESS)?;
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }

        Ok(())
    }
}