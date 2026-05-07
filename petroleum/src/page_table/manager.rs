use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, Translate, Mapper,
        mapper::TranslateResult,
    },
};
use alloc::collections::BTreeMap;
use crate::page_table::constants::{BootInfoFrameAllocator};
use crate::{with_temp_mapping, extract_frame_if_present, safe_cr3_write};

pub trait PageTableHelper {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<()>;
    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PhysFrame>;
    fn translate_address(&self, virtual_addr: usize)
    -> crate::common::logging::SystemResult<usize>;
    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: PageTableFlags,
    ) -> crate::common::logging::SystemResult<()>;
    fn get_page_flags(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PageTableFlags>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()>;
    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn destroy_page_table(
        &mut self,
        table_addr: usize,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()>;
    fn clone_page_table(
        &mut self,
        source_table: usize,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

pub struct PageTableManager {
    pub current_page_table: usize,
    pub initialized: bool,
    pub pml4_frame: Option<PhysFrame>,
    pub mapper: Option<OffsetPageTable<'static>>,
    pub allocated_tables: BTreeMap<usize, PhysFrame>,
}

impl PageTableManager {
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            initialized: false,
            pml4_frame: None,
            mapper: None,
            allocated_tables: BTreeMap::new(),
        }
    }

    pub fn new_with_frame(pml4_frame: x86_64::structures::paging::PhysFrame) -> Self {
        Self {
            current_page_table: pml4_frame.start_address().as_u64() as usize,
            initialized: false,
            pml4_frame: Some(pml4_frame),
            mapper: None,
            allocated_tables: BTreeMap::new(),
        }
    }

    pub fn init_paging(&mut self) -> crate::common::logging::SystemResult<()> {
        Ok(())
    }

    pub fn initialize_with_frame_allocator(
        &mut self,
        phys_offset: VirtAddr,
        frame_allocator: &mut BootInfoFrameAllocator,
        kernel_phys_start: u64,
    ) -> crate::common::logging::SystemResult<()> {
        if self.initialized {
            return Ok(());
        }

        let mut mapper = unsafe { crate::page_table::utils::init(phys_offset, frame_allocator, kernel_phys_start) };

        let (current_pml4, _) = Cr3::read();
        let virt = phys_offset + current_pml4.start_address().as_u64();
        let page = Page::containing_address(virt);

        let _ = unsafe {
            mapper.map_to(
                page,
                current_pml4,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                frame_allocator,
            )
        };

        self.mapper = Some(mapper);
        self.pml4_frame = Some(current_pml4);
        self.current_page_table = current_pml4.start_address().as_u64() as usize;
        self.initialized = true;

        Ok(())
    }

    fn clone_page_table_recursive<'a>(
        mapper: &mut OffsetPageTable<'a>,
        frame_alloc: &mut impl FrameAllocator<Size4KiB>,
        source_table_phys: PhysAddr,
        level: usize,
        allocated_frames: &mut alloc::vec::Vec<PhysFrame>,
        cloned_tables: &mut BTreeMap<PhysAddr, PhysAddr>,
    ) -> crate::common::logging::SystemResult<PhysAddr> {
        if level == 0 || level > 4 {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }
        
        if let Some(&cloned_phys) = cloned_tables.get(&source_table_phys) {
            return Ok(cloned_phys);
        }

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] allocating frame\n");
        let dest_frame: PhysFrame = match frame_alloc.allocate_frame() {
            Some(frame) => frame,
            None => {
                crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] frame allocation failed\n");
                return Err(crate::common::logging::SystemError::FrameAllocationFailed)
            },
        };
        let dest_phys = dest_frame.start_address();
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] inserting into cloned_tables\n");
        cloned_tables.insert(source_table_phys, dest_phys);

        let phys_offset = mapper.phys_offset();
        let source_va = phys_offset + source_table_phys.as_u64();
        let dest_va = phys_offset + dest_frame.start_address().as_u64();

        unsafe {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] zeroing dest_table\n");
            let dest_ptr = dest_va.as_mut_ptr() as *mut u8;
            core::ptr::write_bytes(dest_ptr, 0, 4096);

            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] accessing tables\n");
            
            // Log addresses to verify they are within the mapped range
            let mut buf = [0u8; 16];
            let len = crate::serial::format_hex_to_buffer(source_va.as_u64(), &mut buf, 16);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: source_va=0x");
            crate::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

            let len = crate::serial::format_hex_to_buffer(dest_va.as_u64(), &mut buf, 16);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: dest_va=0x");
            crate::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

            let source_table = &*(source_va.as_ptr() as *const PageTable);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] source_table deref success\n");
            let dest_table = &mut *(dest_va.as_mut_ptr() as *mut PageTable);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table_recursive] dest_table deref success\n");

            for (i, (source_entry, dest_entry)) in source_table.iter().zip(dest_table.iter_mut()).enumerate() {
                if source_entry.flags().contains(PageTableFlags::PRESENT) {
                    if level > 1 && !source_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                        if let Some(child_frame) = extract_frame_if_present!(source_entry) {
                            let cloned_child_phys = Self::clone_page_table_recursive(
                                mapper,
                                frame_alloc,
                                child_frame.start_address(),
                                level - 1,
                                allocated_frames,
                                cloned_tables,
                            )?;
                            dest_entry.set_addr(cloned_child_phys, source_entry.flags());
                        }
                    } else {
                        dest_entry.set_addr(source_entry.addr(), source_entry.flags());
                    }
                }
            }
        }

        allocated_frames.push(dest_frame);
        Ok(dest_frame.start_address())
    }

    pub fn pml4_frame(&self) -> Option<x86_64::structures::paging::PhysFrame> {
        self.pml4_frame
    }
}

impl PageTableHelper for PageTableManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] entered\n");
        let mapper = self.mapper.as_mut().unwrap();
        let virtual_addr = x86_64::VirtAddr::new(virtual_addr as u64);
        let physical_addr = x86_64::PhysAddr::new(physical_addr as u64);
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virtual_addr);
        let frame =
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(physical_addr);

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] calling mapper.map_to\n");
        unsafe {
            match mapper.map_to(page, frame, flags, frame_allocator) {
                Ok(flush) => {
                    flush.flush();
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] map_to success and flushed\n");
                }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] PageAlreadyMapped, continuing\n");
                }
                Err(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed) => {
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] map_to failed: FrameAllocationFailed\n");
                    return Err(crate::common::logging::SystemError::FrameAllocationFailed);
                }
                Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] map_to failed: ParentEntryHugePage\n");
                    return Err(crate::common::logging::SystemError::MappingFailed);
                }
            }
        }
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::map_page] map_to success and flushed\n");
        Ok(())
    }

    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PhysFrame> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }

        let mapper = self.mapper.as_mut().unwrap();
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(
            x86_64::VirtAddr::new(virtual_addr as u64),
        );

        let (frame, flush) = mapper
            .unmap(page)
            .map_err(|_| crate::common::logging::SystemError::UnmappingFailed)?;
        flush.flush();
        Ok(frame)
    }

    fn translate_address(&self, virtual_addr: usize)
    -> crate::common::logging::SystemResult<usize> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }

        let mapper = self.mapper.as_ref().unwrap();
        let virt_addr = VirtAddr::new(virtual_addr as u64);

        match mapper.translate(virt_addr) {
            TranslateResult::Mapped { frame, offset, .. } => {
                let phys_addr = frame.start_address() + offset;
                Ok(phys_addr.as_u64() as usize)
            }
            _ => Err(crate::common::logging::SystemError::InvalidArgument),
        }
    }

    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: PageTableFlags,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }

        let mapper = self.mapper.as_mut().unwrap();
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(
            x86_64::VirtAddr::new(virtual_addr as u64),
        );

        unsafe {
            mapper
                .update_flags(page, flags)
                .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }
        Ok(())
    }

    fn get_page_flags(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PageTableFlags> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }

        let phys_mem_offset = self.mapper.as_ref().unwrap().phys_offset();
        let addr = x86_64::VirtAddr::new(virtual_addr as u64);
        let (level_4_table_frame, _) = x86_64::registers::control::Cr3::read();

        let table_indexes = [
            addr.p4_index(),
            addr.p3_index(),
            addr.p2_index(),
            addr.p1_index(),
        ];
        let mut frame = level_4_table_frame;
        let mut flags = None;

        for (level, &index) in table_indexes.iter().enumerate() {
            let virt = phys_mem_offset + frame.start_address().as_u64();
            let table_ptr: *const PageTable = virt.as_ptr();
            let table = unsafe { &*table_ptr };
            let entry = &table[index];
            if level == 3 {
                if entry.flags().contains(PageTableFlags::PRESENT) {
                    flags = Some(entry.flags());
                } else {
                    return Ok(PageTableFlags::empty());
                }
            } else {
                frame = match entry.frame() {
                    Ok(frame) => frame,
                    Err(_) => return Ok(PageTableFlags::empty()),
                };
            }
        }
        Ok(flags.unwrap_or(PageTableFlags::empty()))
    }

    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        x86_64::instructions::tlb::flush(VirtAddr::new(virtual_addr as u64));
        Ok(())
    }

    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        crate::flush_tlb_safely!();
        Ok(())
    }

    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let new_frame = match frame_allocator.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };

        let mapper = self.mapper.as_mut().unwrap();
        let temp_page = unsafe {
            Page::<Size4KiB>::containing_address(VirtAddr::new(
                crate::page_table::utils::TEMP_VA_FOR_CLONE.as_u64() + 0x3000u64,
            ))
        };
        unsafe {
            mapper
                .map_to(
                    temp_page,
                    new_frame,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    frame_allocator,
                )
                .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }
        unsafe {
            let table_va = (crate::page_table::utils::TEMP_VA_FOR_CLONE.as_u64() + 0x3000) as *mut u8;
            core::ptr::write_bytes(table_va, 0, 4096);
        }
        if let Ok((_frame, flush)) = mapper.unmap(temp_page) {
            flush.flush();
        }
        let table_addr = new_frame.start_address().as_u64() as usize;
        self.allocated_tables.insert(table_addr, new_frame);
        Ok(table_addr)
    }

    fn destroy_page_table(
        &mut self,
        table_addr: usize,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let table_phys = PhysAddr::new(table_addr as u64);
        if let Some(frame) = self.allocated_tables.remove(&table_addr) {
            let mapper = self.mapper.as_mut().unwrap();
            destroy_page_table_recursive(mapper, frame_allocator, table_phys, 4, crate::page_table::utils::TEMP_VA_FOR_DESTROY)?;
            frame_allocator.deallocate_frame(frame);
            Ok(())
        } else {
            Err(crate::common::logging::SystemError::InvalidArgument)
        }
    }

    fn clone_page_table(
        &mut self,
        source_table: usize,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table] entered\n");
        let source_frame = if let Some(frame) = self.allocated_tables.get(&source_table) {
            frame
        } else if Some(source_table) == self.pml4_frame.as_ref().map(|f| f.start_address().as_u64() as usize) {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table] using pml4_frame as source\n");
            self.pml4_frame.as_ref().unwrap()
        } else {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table] source_table not found\n");
            return Err(crate::common::logging::SystemError::InvalidArgument);
        };
        let mapper = self.mapper.as_mut().unwrap();
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table] calling clone_page_table_recursive\n");
        let mut allocated_frames = alloc::vec::Vec::new();
        let mut cloned_tables = BTreeMap::new();
        let cloned_phys = Self::clone_page_table_recursive(
            mapper,
            frame_allocator,
            source_frame.start_address(),
            4,
            &mut allocated_frames,
            &mut cloned_tables,
        )?;

        for frame in allocated_frames {
            self.allocated_tables.insert(frame.start_address().as_u64() as usize, frame);
        }
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [clone_page_table] recursive clone success\n");
        Ok(cloned_phys.as_u64() as usize)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let new_frame = match self.allocated_tables.get(&table_addr) {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::InvalidArgument),
        };
        safe_cr3_write!(*new_frame);
        self.pml4_frame = Some(*new_frame);
        self.current_page_table = table_addr;
        Ok(())
    }

    fn current_page_table(&self) -> usize {
        self.current_page_table
    }
}

fn destroy_page_table_recursive<'a>(
    mapper: &mut OffsetPageTable<'a>,
    frame_alloc: &mut BootInfoFrameAllocator,
    table_phys: PhysAddr,
    level: usize,
    temp_va: VirtAddr,
) -> crate::common::logging::SystemResult<()> {
    if level == 0 || level > 4 {
        return Ok(());
    }
    let frame = PhysFrame::<Size4KiB>::containing_address(table_phys);
    let result: crate::common::logging::SystemResult<()> = with_temp_mapping!(
        mapper,
        frame_alloc,
        temp_va,
        frame,
        {
            let table = unsafe { &*(temp_va.as_ptr() as *const PageTable) };
            let mut child_frames_to_free = alloc::vec::Vec::new();
            if level > 1 {
                for entry in table.iter() {
                    if let Some(child_frame) = extract_frame_if_present!(entry) {
                        if !entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                            child_frames_to_free.push(child_frame);
                        }
                    }
                }
            }
            for child_frame in child_frames_to_free {
                destroy_page_table_recursive(
                    mapper,
                    frame_alloc,
                    child_frame.start_address(),
                    level - 1,
                    crate::page_table::utils::TEMP_VA_FOR_DESTROY,
                )?;
                frame_alloc.deallocate_frame(child_frame);
            }
            Ok(())
        }
    );
    result
}

pub struct DummyFrameAllocator {}
impl DummyFrameAllocator {
    pub fn new() -> Self {
        Self {}
    }
}
unsafe impl FrameAllocator<Size4KiB> for DummyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None
    }
}

impl crate::initializer::Initializable for PageTableManager {
    fn init(&mut self) -> crate::common::logging::SystemResult<()> {
        self.initialized = true;
        Ok(())
    }
    fn name(&self) -> &'static str {
        "PageTableManager"
    }
    fn priority(&self) -> i32 {
        900
    }
}

pub type ProcessPageTable = PageTableManager;
