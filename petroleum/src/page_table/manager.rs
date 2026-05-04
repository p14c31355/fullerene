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
    fn create_page_table(&mut self) -> crate::common::logging::SystemResult<usize>;
    fn destroy_page_table(&mut self, table_addr: usize)
    -> crate::common::logging::SystemResult<()>;
    fn clone_page_table(
        &mut self,
        source_table: usize,
    ) -> crate::common::logging::SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

pub struct PageTableManager<'a> {
    pub current_page_table: usize,
    pub initialized: bool,
    pub pml4_frame: Option<PhysFrame>,
    pub mapper: Option<OffsetPageTable<'a>>,
    pub allocated_tables: BTreeMap<usize, PhysFrame>,
    pub frame_allocator: Option<&'a mut BootInfoFrameAllocator>,
}

impl<'a> PageTableManager<'a> {
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            initialized: false,
            pml4_frame: None,
            mapper: None,
            allocated_tables: BTreeMap::new(),
            frame_allocator: None,
        }
    }

    pub fn new_with_frame(pml4_frame: x86_64::structures::paging::PhysFrame) -> Self {
        Self {
            current_page_table: pml4_frame.start_address().as_u64() as usize,
            initialized: false,
            pml4_frame: Some(pml4_frame),
            mapper: None,
            allocated_tables: BTreeMap::new(),
            frame_allocator: None,
        }
    }

    pub fn init_paging(&mut self) -> crate::common::logging::SystemResult<()> {
        Ok(())
    }

    pub fn initialize_with_frame_allocator(
        &mut self,
        phys_offset: VirtAddr,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()> {
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] entered\n");
        if self.initialized {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] already initialized\n");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let (current_pml4, _) = Cr3::read();
        let table_phys_addr = current_pml4.start_address().as_u64();
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] CR3 read successful\n");

        self.mapper = Some(unsafe {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] calling utils::init\n");
            let mut temp_mapper = unsafe { crate::page_table::utils::init(phys_offset, frame_allocator) };
            
            let virt_addr = phys_offset + table_phys_addr;
            let page = Page::containing_address(virt_addr);
            
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] mapping PML4 to higher half\n");
            match temp_mapper.map_to(
                page,
                current_pml4,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                frame_allocator,
            ) {
                Ok(flush) => {
                    flush.flush();
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] PML4 mapped and flushed\n");
                },
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] PML4 already mapped\n");
                }
                Err(_) => {
                    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] PML4 mapping failed\n");
                    return Err(crate::common::logging::SystemError::MappingFailed);
                },
            };
            
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] creating final OffsetPageTable\n");
            OffsetPageTable::new(
                &mut *(virt_addr.as_mut_ptr() as *mut PageTable),
                phys_offset,
            )
        });

        self.pml4_frame = Some(current_pml4);
        self.current_page_table = table_phys_addr as usize;
        self.allocated_tables
            .insert(table_phys_addr as usize, current_pml4);
        self.frame_allocator = Some(unsafe { core::mem::transmute::<&mut BootInfoFrameAllocator, &'static mut BootInfoFrameAllocator>(frame_allocator) });
        self.initialized = true;
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PageTableManager::init] initialization complete\n");
        Ok(())
    }

    fn clone_page_table_recursive(
        mapper: &mut OffsetPageTable<'a>,
        frame_alloc: &mut BootInfoFrameAllocator,
        source_table_phys: PhysAddr,
        level: usize,
        temp_va: VirtAddr,
        allocated_tables: &mut BTreeMap<usize, PhysFrame>,
    ) -> crate::common::logging::SystemResult<PhysAddr> {
        if level == 0 || level > 4 {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        let dest_frame: PhysFrame = match frame_alloc.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };

        unsafe {
            core::ptr::write_bytes(dest_frame.start_address().as_u64() as *mut PageTable, 0, 1);
        }

        allocated_tables.insert(dest_frame.start_address().as_u64() as usize, dest_frame);

        let source_phys_frame = PhysFrame::<Size4KiB>::containing_address(source_table_phys);
        let result: crate::common::logging::SystemResult<PhysAddr> = with_temp_mapping!(
            mapper,
            frame_alloc,
            temp_va,
            source_phys_frame,
            {
                let source_table = unsafe { &mut *(temp_va.as_mut_ptr() as *mut PageTable) };
                let dest_result: crate::common::logging::SystemResult<PhysAddr> = with_temp_mapping!(
                    mapper,
                    frame_alloc,
                    temp_va + 0x1000u64,
                    dest_frame,
                    {
                        let dest_table = unsafe { &mut *((temp_va.as_u64() + 0x1000) as *mut PageTable) };
                        let mut child_va = temp_va + 0x2000u64;
                        for (_i, (source_entry, dest_entry)) in
                            source_table.iter().zip(dest_table.iter_mut()).enumerate()
                        {
                            if source_entry.flags().contains(PageTableFlags::PRESENT) {
                                  if level > 1
                                      && !((level == 2) && source_entry.flags().contains(PageTableFlags::HUGE_PAGE))
                                  {
                                      if let Some(child_frame) = extract_frame_if_present!(source_entry) {
                                          let cloned_child_phys = Self::clone_page_table_recursive(
                                              mapper,
                                              frame_alloc,
                                              child_frame.start_address(),
                                              level - 1,
                                              child_va,
                                              allocated_tables,
                                          )?;
                                          dest_entry.set_addr(cloned_child_phys, source_entry.flags());
                                          child_va += 0x1000u64;
                                      }
                                  } else {
                                      dest_entry.set_addr(source_entry.addr(), source_entry.flags());
                                  }
                            }
                        }
                        Ok(dest_frame.start_address())
                    }
                );
                dest_result
            }
        );
        result
    }

    pub fn pml4_frame(&self) -> Option<x86_64::structures::paging::PhysFrame> {
        self.pml4_frame
    }
}

impl<'a> PageTableHelper for PageTableManager<'a> {
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

    fn create_page_table(&mut self) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let frame_alloc = self.frame_allocator.as_mut().unwrap();
        let new_frame = match frame_alloc.allocate_frame() {
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
                    frame_alloc,
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
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let table_phys = PhysAddr::new(table_addr as u64);
        if let Some(frame) = self.allocated_tables.remove(&table_addr) {
            let mapper = self.mapper.as_mut().unwrap();
            let frame_alloc = self.frame_allocator.as_deref_mut().unwrap();
            destroy_page_table_recursive(mapper, frame_alloc, table_phys, 4, crate::page_table::utils::TEMP_VA_FOR_DESTROY)?;
            frame_alloc.deallocate_frame(frame);
            Ok(())
        } else {
            Err(crate::common::logging::SystemError::InvalidArgument)
        }
    }

    fn clone_page_table(
        &mut self,
        source_table: usize,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized { return Err(crate::common::logging::SystemError::InternalError); }
        let source_frame = match self.allocated_tables.get(&source_table) {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::InvalidArgument),
        };
        let mapper = self.mapper.as_mut().unwrap();
        let frame_alloc = self.frame_allocator.as_mut().unwrap();
        let cloned_phys = Self::clone_page_table_recursive(
            mapper,
            frame_alloc,
            source_frame.start_address(),
            4,
            crate::page_table::utils::TEMP_VA_FOR_CLONE,
            &mut self.allocated_tables,
        )?;
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

impl<'a> crate::initializer::Initializable for PageTableManager<'a> {
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

pub type ProcessPageTable<'a> = PageTableManager<'a>;
