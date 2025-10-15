use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PhysFrame, Size4KiB,
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
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
) -> VirtAddr {
    // For now, just return identity mapping offset
    // Full implementation would involve creating a new page table structure
    VirtAddr::new(0)
}

/// Allocate heap memory from EFI memory map
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> VirtAddr {
    const FRAME_SIZE: u64 = 4096;
    let heap_frames = (heap_size + FRAME_SIZE as usize - 1) / FRAME_SIZE as usize;

    let heap_start = if start_addr.as_u64() % FRAME_SIZE == 0 {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    VirtAddr::new(heap_start.as_u64())
}

// Global heap allocator
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap = linked_list_allocator::LockedHeap::empty();

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
