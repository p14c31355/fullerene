use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    instructions::tlb,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PhysFrame, Size4KiB, Translate,
    },
};

/// EFI Memory Descriptor as defined in UEFI spec
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub type_: crate::common::EfiMemoryType,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

/// A FrameAllocator that returns usable frames from the bootloader's memory map.
pub struct BootInfoFrameAllocator<'a> {
    memory_map: &'a [EfiMemoryDescriptor],
    next_descriptor: usize,
    next_frame_offset: u64,
}

impl<'a> BootInfoFrameAllocator<'a> {
    /// Create a FrameAllocator from the passed memory map.
    ///
    /// This function is unsafe because the caller must guarantee that the
    /// memory map is valid. The main requirement is that all frames that are marked
    /// as `USABLE` in it are really unused.
    pub unsafe fn init(memory_map: &'a [EfiMemoryDescriptor]) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next_descriptor: 0,
            next_frame_offset: 0,
        }
    }
}

unsafe impl<'a> FrameAllocator<Size4KiB> for BootInfoFrameAllocator<'a> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        const FRAME_SIZE: u64 = 4096; // 4 KiB

        while self.next_descriptor < self.memory_map.len() {
            let descriptor = &self.memory_map[self.next_descriptor];
            if descriptor.type_ == crate::common::EfiMemoryType::EfiConventionalMemory
                && descriptor.number_of_pages > 0
            {
                while self.next_frame_offset < descriptor.number_of_pages {
                    let frame_addr = PhysAddr::new(
                        descriptor.physical_start + self.next_frame_offset * FRAME_SIZE,
                    );
                    if let Ok(frame) = PhysFrame::<Size4KiB>::from_start_address(frame_addr) {
                        self.next_frame_offset += 1;
                        return Some(frame);
                    }
                    self.next_frame_offset += 1;
                }
            }
            self.next_descriptor += 1;
            self.next_frame_offset = 0;
        }

        None // No more usable frames
    }
}

/// Initialize a new OffsetPageTable.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Returns a mutable reference to the active level 4 table.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// Creates an example mapping for the given page to frame `0xb8000`.
pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = Flags::PRESENT | Flags::WRITABLE;

    let map_to_result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    map_to_result.expect("map_to failed").flush();
}

/// Translates the given virtual address to the mapped physical address, or
/// `None` if the address is not mapped.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`.
pub unsafe fn translate_addr(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    translate_addr_inner(addr, physical_memory_offset)
}

/// Reinitialize the page table with identity mapping
///
/// Returns the physical memory offset used for the mapping
pub fn reinit_page_table(
    kernel_phys_start: PhysAddr,
    _fb_addr: Option<VirtAddr>,
    _fb_size: Option<u64>,
) -> VirtAddr {
    // Create identity mapping offset for kernel
    let phys_offset = VirtAddr::new(kernel_phys_start.as_u64());

    // Set up identity mapping for the kernel region
    // This would typically involve creating page table entries that map
    // virtual addresses to the same physical addresses

    phys_offset
}

/// Allocate heap memory from EFI memory map
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> VirtAddr {
    const FRAME_SIZE: u64 = 4096;
    let _heap_frames = (heap_size + FRAME_SIZE as usize - 1) / FRAME_SIZE as usize;

    let heap_start = if start_addr.as_u64() % FRAME_SIZE == 0 {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    VirtAddr::new(heap_start.as_u64())
}

use x86_64::structures::paging::PageTableFlags as PageFlags;

// Global heap allocator
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

// Consolidated Page Table Helper Trait
pub trait PageTableHelper {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<()>;
    fn unmap_page(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>>;
    fn translate_address(&self, virtual_addr: usize) -> crate::common::logging::SystemResult<usize>;
    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> crate::common::logging::SystemResult<()>;
    fn get_page_flags(&self, virtual_addr: usize) -> crate::common::logging::SystemResult<PageFlags>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()>;
    fn create_page_table(&mut self) -> crate::common::logging::SystemResult<usize>;
    fn destroy_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn clone_page_table(&mut self, source_table: usize) -> crate::common::logging::SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

/// A dummy frame allocator for when we need to allocate pages for page tables
pub struct DummyFrameAllocator {}

impl DummyFrameAllocator {
    pub fn new() -> Self {
        Self {}
    }
}

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for DummyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<x86_64::structures::paging::PhysFrame<Size4KiB>> {
        None // For now, we don't support allocating new frames for page tables
    }
}

/// Process page table type alias for PageTableManager
pub type ProcessPageTable = PageTableManager;

/// Page table manager implementation
pub struct PageTableManager {
    current_page_table: usize,
    initialized: bool,
    pub pml4_frame: Option<x86_64::structures::paging::PhysFrame>,
    mapper: Option<OffsetPageTable<'static>>,
}

impl PageTableManager {
    /// Create a new page table manager (deferred initialization)
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            initialized: false,
            pml4_frame: None,
            mapper: None,
        }
    }

    /// Create a new page table manager with a specific frame
    pub fn new_with_frame(pml4_frame: x86_64::structures::paging::PhysFrame) -> Self {
        Self {
            current_page_table: pml4_frame.start_address().as_u64() as usize,
            initialized: false,
            pml4_frame: Some(pml4_frame),
            mapper: None,
        }
    }

    /// Set the pml4 frame for this page table manager
    pub fn set_pml4_frame(&mut self, pml4_frame: x86_64::structures::paging::PhysFrame) {
        self.pml4_frame = Some(pml4_frame);
        self.current_page_table = pml4_frame.start_address().as_u64() as usize;
    }

    /// Initialize paging
    pub fn init_paging(&mut self, physical_memory_offset: VirtAddr) -> crate::common::logging::SystemResult<()> {
        let frame = if let Some(frame) = self.pml4_frame {
            frame
        } else {
            let (current_frame, _) = x86_64::registers::control::Cr3::read();
            current_frame
        };
        self.current_page_table = frame.start_address().as_u64() as usize;

        // Create mapper using the appropriate page table
        unsafe {
            let table_virt = physical_memory_offset + self.current_page_table as u64;
            let table_ptr = table_virt.as_mut_ptr() as *mut PageTable;
            let mapper = OffsetPageTable::new(
                &mut *table_ptr,
                physical_memory_offset,
            );
            self.mapper = Some(mapper);
        }

        self.initialized = true;
        Ok(())
    }
}

impl PageTableHelper for PageTableManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
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

        // Map the page using the stored mapper instance
        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }

        Ok(())
    }

        fn unmap_page(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let mapper = self.mapper.as_mut().unwrap();
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(virtual_addr as u64));

        let (frame, flush) = mapper.unmap(page).map_err(|_| crate::common::logging::SystemError::UnmappingFailed)?;
        flush.flush();

        Ok(frame)
    }

    fn translate_address(&self, virtual_addr: usize) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let mapper = self.mapper.as_ref().unwrap();
        let virt_addr = VirtAddr::new(virtual_addr as u64);

        match mapper.translate_addr(virt_addr) {
            Some(phys_addr) => Ok(phys_addr.as_u64() as usize),
            None => Err(crate::common::logging::SystemError::InvalidArgument),
        }
    }

    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let mapper = self.mapper.as_mut().unwrap();
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(virtual_addr as u64));

        unsafe {
            mapper.update_flags(page, flags).map_err(|_| crate::common::logging::SystemError::MappingFailed)?.flush();
        }

        Ok(())
    }

    fn get_page_flags(&self, virtual_addr: usize) -> crate::common::logging::SystemResult<PageFlags> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Traverse the page table to find the entry for this page
        let phys_mem_offset = self.mapper.as_ref().unwrap().phys_offset();
        let addr = x86_64::VirtAddr::new(virtual_addr as u64);

        // Use CR3 to get L4
        let (level_4_table_frame, _) = x86_64::registers::control::Cr3::read();

        let table_indexes = [
            addr.p4_index(),
            addr.p3_index(),
            addr.p2_index(),
            addr.p1_index(),
        ];
        let mut frame = level_4_table_frame;
        let mut flags = None;

        // Traverse the multi-level page table
        for (level, &index) in table_indexes.iter().enumerate() {
            // Convert the frame into a page table reference
            let virt = phys_mem_offset + frame.start_address().as_u64();
            let table_ptr: *const PageTable = virt.as_ptr();
            let table = unsafe { &*table_ptr };

            // Read the page table entry
            let entry = &table[index];
            if level == 3 {
                // At L1, get flags
                if entry.flags().contains(PageFlags::PRESENT) {
                    flags = Some(entry.flags());
                } else {
                    return Ok(PageFlags::empty()); // Not present
                }
            } else {
                // Continue traversing
                frame = match entry.frame() {
                    Ok(frame) => frame,
                    Err(_) => return Ok(PageFlags::empty()), // Not present
                };
            }
        }

        Ok(flags.unwrap_or(PageFlags::empty()))
    }

    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        tlb::flush(VirtAddr::new(virtual_addr as u64));
        Ok(())
    }

    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let (current, flags) = Cr3::read();
        unsafe { Cr3::write(current, flags) };
        Ok(())
    }

    fn create_page_table(&mut self) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Return a dummy address
        Ok(0x1000)
    }

    fn destroy_page_table(&mut self, _table_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        Ok(())
    }

    fn clone_page_table(&mut self, _source_table: usize) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        Ok(_source_table + 0x1000) // Dummy offset
    }

    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        self.current_page_table = table_addr;
        Ok(())
    }

    fn current_page_table(&self) -> usize {
        self.current_page_table
    }
}

/// Private function that is called by `translate_addr`.
///
/// This function is safe to limit the scope of `unsafe` because Rust is
/// conservative around generic types.
fn translate_addr_inner(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::page_table::FrameError;

    // read the active level 4 frame from the CR3 register
    let (level_4_table_frame, _) = Cr3::read();

    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let mut frame = level_4_table_frame;

    // traverse the multi-level page table
    for &index in &table_indexes {
        // convert the frame into a page table reference
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *const PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr };

        // read the page table entry and update `frame`
        let entry = &table[index];
        frame = match entry.frame() {
            Ok(frame) => frame,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("huge pages not supported"),
        };
    }

    // calculate the physical address by adding the page offset
    Some(frame.start_address() + u64::from(addr.page_offset()))
}
