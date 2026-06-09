use crate::page_table::allocator::traits::FrameAllocatorExt;
use crate::page_table::constants::BootInfoFrameAllocator;
use crate::page_table::types::PageTableHelper;
use crate::{extract_frame_if_present, safe_cr3_write, with_temp_mapping};
use alloc::collections::BTreeMap;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, Translate, mapper::TranslateResult,
    },
};

pub struct ProcessPageTable {
    pub current_page_table: usize,
    pub initialized: bool,
    pub pml4_frame: Option<PhysFrame>,
    pub mapper: Option<OffsetPageTable<'static>>,
    pub allocated_tables: BTreeMap<usize, PhysFrame>,
}

impl crate::initializer::Initializable for ProcessPageTable {
    fn init(&mut self) -> crate::common::logging::SystemResult<()> {
        self.initialized = true;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "ProcessPageTable"
    }

    fn priority(&self) -> i32 {
        900
    }
}

impl ProcessPageTable {
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

    pub fn pml4_frame(&self) -> Option<x86_64::structures::paging::PhysFrame> {
        self.pml4_frame
    }

    /// Accessor for allocated_tables (read-only)
    pub fn allocated_tables(&self) -> &BTreeMap<usize, PhysFrame> {
        &self.allocated_tables
    }

    /// Accessor for allocated_tables (mutable)
    pub fn allocated_tables_mut(&mut self) -> &mut BTreeMap<usize, PhysFrame> {
        &mut self.allocated_tables
    }

    /// Set current_page_table without CR3 switch
    pub fn set_current(&mut self, addr: usize) {
        self.current_page_table = addr;
    }

    pub fn initialize_with_frame_allocator(
        &mut self,
        phys_offset: VirtAddr,
        _frame_allocator: &mut BootInfoFrameAllocator,
        _kernel_phys_start: u64,
    ) -> crate::common::logging::SystemResult<()> {
        if self.initialized {
            return Ok(());
        }

        // init_and_jump already set up the page tables, so we just need to
        // create an OffsetPageTable from the current CR3.
        let (current_pml4, _) = Cr3::read();
        let l4_virt = phys_offset + current_pml4.start_address().as_u64();

        let mapper =
            unsafe { OffsetPageTable::new(&mut *(l4_virt.as_mut_ptr::<PageTable>()), phys_offset) };

        self.mapper = Some(mapper);
        self.pml4_frame = Some(current_pml4);
        self.current_page_table = current_pml4.start_address().as_u64() as usize;
        self.initialized = true;

        Ok(())
    }

    pub fn init_paging(&mut self) -> crate::common::logging::SystemResult<()> {
        Ok(())
    }
}

impl PageTableHelper for ProcessPageTable {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let mapper = self.mapper.as_mut().unwrap();
        let virtual_addr = x86_64::VirtAddr::new(virtual_addr as u64);
        let physical_addr = x86_64::PhysAddr::new(physical_addr as u64);
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virtual_addr);
        let frame =
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(physical_addr);

        unsafe {
            match mapper.map_to(page, frame, flags, frame_allocator) {
                Ok(flush) => {
                    flush.flush();
                }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {}
                Err(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed) => {
                    crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [map_page] FrameAllocationFailed\n");
                    return Err(crate::common::logging::SystemError::FrameAllocationFailed);
                }
                Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {
                    // Split the 2MB huge page into 512 4KB pages, then retry the mapping.
                    crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [map_page] ParentEntryHugePage, splitting...\n");
                    split_huge_page_2mb(mapper, virtual_addr, flags, frame_allocator)
                        .map_err(|_| crate::common::logging::SystemError::MappingFailed)?;
                    // Unmap the page first if it was pre-filled by the split, then map
                    let _ = mapper.unmap(page);
                    // Retry mapping now that the huge page has been split
                    match mapper.map_to(page, frame, flags, frame_allocator) {
                        Ok(flush) => {
                            flush.flush();
                        }
                        Err(_e) => {
                            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [map_page] Retry failed after split\n");
                            return Err(crate::common::logging::SystemError::MappingFailed);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PhysFrame> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

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

    fn translate_address(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

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
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

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
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

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
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
        x86_64::instructions::tlb::flush(VirtAddr::new(virtual_addr as u64));
        Ok(())
    }

    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
        crate::flush_tlb_safely!();
        Ok(())
    }

    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
        let new_frame = match frame_allocator.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };

        let mapper = self.mapper.as_mut().unwrap();
        let temp_page = unsafe {
            Page::<Size4KiB>::containing_address(VirtAddr::new(
                crate::page_table::raw::TEMP_VA_FOR_CLONE.as_u64() + 0x3000u64,
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
            let table_va = (crate::page_table::raw::TEMP_VA_FOR_CLONE.as_u64() + 0x3000) as *mut u8;
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
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
        let table_phys = PhysAddr::new(table_addr as u64);
        if let Some(frame) = self.allocated_tables.remove(&table_addr) {
            let mapper = self.mapper.as_mut().unwrap();
            destroy_page_table_recursive(
                mapper,
                frame_allocator,
                table_phys,
                4,
                crate::page_table::raw::TEMP_VA_FOR_DESTROY,
            )?;
            frame_allocator.deallocate_frame(crate::page_table::types::PhysFrame {
                start_address: frame.start_address().as_u64(),
            });
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
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Ensure the mapper points to the current CR3 page table.
        let (current_pml4, _) = x86_64::registers::control::Cr3::read();
        if self.pml4_frame.map(|f| f.start_address()) != Some(current_pml4.start_address()) {
            let phys_offset = self
                .mapper
                .as_ref()
                .map(|m| m.phys_offset())
                .unwrap_or(crate::page_table::constants::HIGHER_HALF_OFFSET);
            let pml4_virt = phys_offset + current_pml4.start_address().as_u64();
            self.mapper = Some(unsafe {
                OffsetPageTable::new(&mut *(pml4_virt.as_mut_ptr::<PageTable>()), phys_offset)
            });
            self.pml4_frame = Some(current_pml4);
        }

        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: clone_page_table minimal v2\n");

        let source_frame = if let Some(frame) = self.allocated_tables.get(&source_table) {
            *frame
        } else if Some(source_table)
            == self
                .pml4_frame
                .as_ref()
                .map(|f| f.start_address().as_u64() as usize)
        {
            self.pml4_frame.unwrap()
        } else {
            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: clone_page_table invalid source_table\n");
            return Err(crate::common::logging::SystemError::InvalidArgument);
        };
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: clone_page_table source_frame obtained\n");

        let new_frame = frame_allocator
            .allocate_frame()
            .ok_or(crate::common::logging::SystemError::FrameAllocationFailed)?;

        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: clone_page_table new_frame allocated\n");

        let mapper = self.mapper.as_mut().unwrap();
        let phys_offset = mapper.phys_offset();

        // Convert physical addresses to virtual addresses using the current physical offset
        let src_va = phys_offset + source_frame.start_address().as_u64();
        let dst_va = phys_offset + new_frame.start_address().as_u64();

        // Debugging addresses
        let mut buf = [0u8; 16];
        let len = crate::serial::format_hex_to_buffer(phys_offset.as_u64(), &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: phys_offset: 0x");
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        let len = crate::serial::format_hex_to_buffer(src_va.as_u64(), &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: src_va: 0x");
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        let len = crate::serial::format_hex_to_buffer(dst_va.as_u64(), &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: dst_va: 0x");
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        // Shallow copy: copy all entries from source to destination
        // This shares page tables between processes (kernel pages are shared, user pages will be copied on write later)
        unsafe {
            let src_table = &*(src_va.as_ptr::<PageTable>());
            let dst_table = &mut *(dst_va.as_mut_ptr::<PageTable>());

            for i in 0..512 {
                let entry = src_table[i].clone();
                if entry.flags().contains(PageTableFlags::PRESENT) {
                    dst_table[i] = entry;
                }
            }
        }

        // CRITICAL: Must track the new frame in allocated_tables so that:
        // 1. init_page_table() can find it via pt_manager.allocated_tables().get()
        // 2. The process's page_table_phys_addr is correctly set
        self.allocated_tables
            .insert(new_frame.start_address().as_u64() as usize, new_frame);

        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: clone_page_table shallow done\n");
        Ok(new_frame.start_address().as_u64() as usize)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
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

/// Split a 2MB huge page entry into 512 individual 4KB pages.
///
/// This is called when `Mapper::map_to` returns `ParentEntryHugePage`,
/// meaning the virtual address falls within a 2MB huge page mapping.
/// We allocate a new L1 page table, populate it with 512 entries
/// corresponding to the original huge page's physical base and flags,
/// then replace the L2 huge page entry with a pointer to the new L1 table.
///
/// # Safety
///
/// The caller must ensure that `virtual_addr` is mapped via a 2MB huge page
/// at the L2 level, and that the mapper's `phys_offset` is valid.
unsafe fn split_huge_page_2mb(
    mapper: &mut OffsetPageTable<'static>,
    virtual_addr: VirtAddr,
    default_flags: PageTableFlags,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    let phys_offset = mapper.phys_offset();

    // Compute the 2MB-aligned virtual address covering this page
    let huge_page_virt = VirtAddr::new(virtual_addr.as_u64() & !((2 * 1024 * 1024) - 1));

    // Walk the page tables to find the L2 entry
    let (l4_frame, _) = x86_64::registers::control::Cr3::read();
    let l4_virt = phys_offset + l4_frame.start_address().as_u64();
    let l4 = unsafe { &*(l4_virt.as_ptr::<PageTable>()) };

    let p4 = virtual_addr.p4_index();
    let p3 = virtual_addr.p3_index();
    let p2 = virtual_addr.p2_index();

    // L3 table
    if !l4[p4].flags().contains(PageTableFlags::PRESENT) {
        return Err("L4 entry not present during huge page split");
    }
    let l3_phys = l4[p4].addr();
    let l3_virt = phys_offset + l3_phys.as_u64();
    let l3 = unsafe { &*(l3_virt.as_ptr::<PageTable>()) };

    // L2 entry
    if !l3[p3].flags().contains(PageTableFlags::PRESENT) {
        return Err("L3 entry not present during huge page split");
    }
    let l2_phys = l3[p3].addr();
    let l2_virt = phys_offset + l2_phys.as_u64();
    let l2 = unsafe { &mut *(l2_virt.as_mut_ptr::<PageTable>()) };

    if !l2[p2].flags().contains(PageTableFlags::HUGE_PAGE) {
        return Err("L2 entry is not a huge page during split");
    }

    // Get the physical base address and flags of the existing huge page
    let huge_page_phys_base = l2[p2].addr();
    // Preserve the existing flags, but remove HUGE_PAGE
    let existing_flags = {
        let mut f = l2[p2].flags();
        f.remove(PageTableFlags::HUGE_PAGE);
        // Ensure PRESENT is set
        f | PageTableFlags::PRESENT
    };

    // Allocate and zero a new L1 page table
    let l1_frame = frame_allocator
        .allocate_frame()
        .ok_or("split: alloc L1 failed")?;
    let l1_phys = l1_frame.start_address();
    let l1_virt = phys_offset + l1_phys.as_u64();
    unsafe {
        core::ptr::write_bytes(l1_virt.as_mut_ptr::<u8>(), 0, 4096);
    }

    // Populate the L1 table with 512 entries mapping to the original huge page's physical range
    let l1_table = unsafe { &mut *(l1_virt.as_mut_ptr::<PageTable>()) };
    for i in 0..512u64 {
        let page_phys = PhysAddr::new(huge_page_phys_base.as_u64() + i * 4096);
        l1_table[i as usize].set_addr(page_phys, existing_flags);
    }

    // Replace the L2 huge page entry with a normal entry pointing to the new L1 table
    l2[p2].set_addr(l1_phys, default_flags | PageTableFlags::PRESENT);

    // Flush the TLB for the affected 2MB range
    x86_64::instructions::tlb::flush(huge_page_virt);
    x86_64::instructions::tlb::flush(VirtAddr::new(
        huge_page_virt.as_u64() + 2 * 1024 * 1024 - 4096,
    ));

    Ok(())
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
    let result: crate::common::logging::SystemResult<()> =
        with_temp_mapping!(mapper, frame_alloc, temp_va, frame, {
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
                    crate::page_table::raw::TEMP_VA_FOR_DESTROY,
                )?;
                frame_alloc.deallocate_frame(crate::page_table::types::PhysFrame {
                    start_address: child_frame.start_address().as_u64(),
                });
            }
            Ok(())
        });
    result
}

impl ProcessPageTable {
    fn clone_page_table_recursive_fixed<'a>(
        mapper: &mut OffsetPageTable<'a>,
        frame_alloc: &mut impl FrameAllocator<Size4KiB>,
        source_table_phys: PhysAddr,
        level: usize,
        allocated_frames: &mut [Option<PhysFrame>; 512],
        allocated_count: &mut usize,
        cloned_tables: &mut [(PhysAddr, PhysAddr); 64],
        cloned_count: &mut usize,
    ) -> crate::common::logging::SystemResult<PhysAddr> {
        if level == 0 || level > 4 {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        // Linear search for already cloned tables
        for i in 0..*cloned_count {
            if cloned_tables[i].0 == source_table_phys {
                return Ok(cloned_tables[i].1);
            }
        }

        let dest_frame = match frame_alloc.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };
        let dest_phys = dest_frame.start_address();

        if *cloned_count < 64 {
            cloned_tables[*cloned_count] = (source_table_phys, dest_phys);
            *cloned_count += 1;
        }

        let phys_offset = mapper.phys_offset();

        let source_va = phys_offset + source_table_phys.as_u64();
        let dest_va = phys_offset + dest_frame.start_address().as_u64();

        // Debug output for addresses
        let mut buf = [0u8; 16];
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] source_va: 0x");
        let len = crate::serial::format_hex_to_buffer(source_va.as_u64(), &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] dest_va: 0x");
        let len = crate::serial::format_hex_to_buffer(dest_va.as_u64(), &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        unsafe {
            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] writing dest table\n");
            let dest_ptr = dest_va.as_mut_ptr::<u8>() as *mut u8;
            core::ptr::write_bytes(dest_ptr, 0, 4096);

            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] reading source table\n");
            let source_table = &*(source_va.as_ptr::<PageTable>());
            let dest_table = &mut *(dest_va.as_mut_ptr::<PageTable>());

            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] starting loop\n");
            for (_i, (source_entry, dest_entry)) in
                source_table.iter().zip(dest_table.iter_mut()).enumerate()
            {
                if source_entry.flags().contains(PageTableFlags::PRESENT) {
                    crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] processing present entry\n");
                    if level > 1 && !source_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] recursing\n");
                        if let Some(child_frame) = extract_frame_if_present!(source_entry) {
                            let cloned_child_phys = Self::clone_page_table_recursive_fixed(
                                mapper,
                                frame_alloc,
                                child_frame.start_address(),
                                level - 1,
                                allocated_frames,
                                allocated_count,
                                cloned_tables,
                                cloned_count,
                            )?;
                            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [clone_recursive_fixed] recurse returned\n");
                            dest_entry.set_addr(cloned_child_phys, source_entry.flags());
                        }
                    } else if level == 1 {
                        // Full copy of the physical page to ensure process isolation
                        let source_phys = source_entry.addr();
                        let dest_frame = frame_alloc
                            .allocate_frame()
                            .ok_or(crate::common::logging::SystemError::FrameAllocationFailed)?;
                        let dest_phys = dest_frame.start_address();

                        let phys_offset = mapper.phys_offset();
                        let source_va: *const u8 =
                            (phys_offset + source_phys.as_u64()).as_ptr::<u8>();
                        let dest_va: *mut u8 =
                            (phys_offset + dest_phys.as_u64()).as_mut_ptr::<u8>();

                        unsafe {
                            core::ptr::copy_nonoverlapping(source_va, dest_va, 4096);
                        }

                        dest_entry.set_addr(dest_phys, source_entry.flags());
                    } else {
                        // Higher level tables or huge pages - just copy the address
                        // (Huge pages are typically kernel-only in this architecture)
                        dest_entry.set_addr(source_entry.addr(), source_entry.flags());
                    }
                }
            }
        }

        if level > 1 {
            if *allocated_count < 512 {
                allocated_frames[*allocated_count] = Some(dest_frame);
                *allocated_count += 1;
            }
        }
        Ok(dest_frame.start_address())
    }
}
