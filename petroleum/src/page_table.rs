use crate::{debug_log, debug_log_no_alloc, flush_tlb_and_verify, map_pages_loop};

// Macros are automatically available from common module

use spin::Once;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PhysFrame, Size4KiB, Translate,
    },
};

// Track heap initialization to prevent double init
pub static HEAP_INITIALIZED: Once<bool> = Once::new();

// Initialize global heap allocator if not already initialized
pub fn init_global_heap(ptr: *mut u8, size: usize) {
    if HEAP_INITIALIZED.get().is_none() {
        unsafe {
            ALLOCATOR.lock().init(ptr, size);
        }
        HEAP_INITIALIZED.call_once(|| true);
    }
}

/// EFI Memory Descriptor as defined in UEFI spec
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub type_: crate::common::EfiMemoryType,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

/// ELF definitions for parsing kernel permissions
#[repr(C)]
pub struct Elf64Ehdr {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

#[repr(C)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// Static buffer for bitmap - sized for up to 32GiB of RAM (8M frames)
/// Each bit represents one 4KB frame, so size is (8M / 64) = 128K u64s = 1MB
static mut BITMAP_STATIC: [u64; 131072] = [u64::MAX; 131072];

/// Bitmap-based frame allocator implementation
pub struct BitmapFrameAllocator {
    bitmap: Option<&'static mut [u64]>,
    frame_count: usize,
    next_free_frame: usize,
    initialized: bool,
}

impl BitmapFrameAllocator {
    /// Create a new bitmap frame allocator
    pub fn new() -> Self {
        Self {
            bitmap: None,
            frame_count: 0,
            next_free_frame: 0,
            initialized: false,
        }
    }

    /// Create a FrameAllocator from the passed memory map.
    /// (for compatibility)
    pub unsafe fn init(memory_map: &[EfiMemoryDescriptor]) -> Self {
        let mut allocator = BitmapFrameAllocator::new();
        allocator.init_with_memory_map(memory_map).expect("Failed to init bitmap allocator");
        allocator
    }

    /// Initialize with EFI memory map
        pub fn init_with_memory_map(
        &mut self,
        memory_map: &[EfiMemoryDescriptor],
    ) -> crate::common::logging::SystemResult<()> {
        // 1. Find the highest physical address to determine the total number of frames to manage.
        let max_phys_addr = memory_map
            .iter()
            .map(|d| d.physical_start + d.number_of_pages * 4096)
            .max()
            .unwrap_or(0);
        let total_frames = (max_phys_addr.div_ceil(4096)) as usize;

        if total_frames == 0 {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Calculate bitmap size needed
        let bitmap_size = (total_frames + 63) / 64; // Round up for 64-bit chunks

        // Ensure bitmap size doesn't exceed our static buffer
        if bitmap_size > 131072 {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Get a mutable slice from the static buffer
        unsafe {
            self.bitmap = Some(&mut BITMAP_STATIC[..bitmap_size]);

            // Initialize bitmap - mark all as used initially
            for chunk in self.bitmap.as_mut().unwrap().iter_mut() {
                *chunk = u64::MAX;
            }
        }

        self.frame_count = total_frames;
        self.next_free_frame = 0;
        self.initialized = true;

        // Mark available frames as free based on their physical address
        for descriptor in memory_map {
            if descriptor.type_ == crate::common::EfiMemoryType::EfiConventionalMemory {
                let start_frame = (descriptor.physical_start / 4096) as usize;
                let end_frame = start_frame + descriptor.number_of_pages as usize;

                for frame_index in start_frame..end_frame {
                    if frame_index < self.frame_count {
                        self.set_frame_free(frame_index);
                    }
                }
            }
        }
        // Mark frame 0 as used to avoid allocating the null page.
        self.set_frame_used(0);
        Ok(())
    }

    /// Set a frame as free in the bitmap
    fn set_frame_free(&mut self, frame_index: usize) {
        if let Some(ref mut bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;

            if chunk_index < bitmap.len() {
                bitmap[chunk_index] &= !(1 << bit_index);
            }
        }
    }

    /// Set a frame as used in the bitmap
    fn set_frame_used(&mut self, frame_index: usize) {
        if let Some(ref mut bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;

            if chunk_index < bitmap.len() {
                bitmap[chunk_index] |= 1 << bit_index;
            }
        }
    }

    /// Check if a frame is free
    fn is_frame_free(&self, frame_index: usize) -> bool {
        if let Some(ref bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;

            if chunk_index < bitmap.len() {
                (bitmap[chunk_index] & (1 << bit_index)) == 0
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Find the next free frame starting from a given index
        fn find_next_free_frame(&self, start_index: usize) -> Option<usize> {
        if !self.initialized {
            return None;
        }

        if let Some(ref bitmap) = self.bitmap {
            let mut chunk_index = start_index / 64;
            let bit_in_chunk = start_index % 64;

            if chunk_index < bitmap.len() {
                let mut chunk = bitmap[chunk_index];
                // Mask off bits before start_index in the first chunk to ignore them
                chunk |= (1u64.wrapping_shl(bit_in_chunk as u32)).wrapping_sub(1);
                if chunk != u64::MAX {
                    let first_free_bit = (!chunk).trailing_zeros() as usize;
                    let frame_index = chunk_index * 64 + first_free_bit;
                    if frame_index < self.frame_count {
                        return Some(frame_index);
                    }
                }
                chunk_index += 1;
            }

            for i in chunk_index..bitmap.len() {
                let chunk = bitmap[i];
                if chunk != u64::MAX {
                    let first_free_bit = (!chunk).trailing_zeros() as usize;
                    let frame_index = i * 64 + first_free_bit;
                    if frame_index < self.frame_count {
                        return Some(frame_index);
                    }
                }
            }
        }

        None
    }

    /// Allocate a specific frame range (for reserving used regions)
    pub fn allocate_frames_at(&mut self, start_addr: usize, count: usize) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let start_frame = start_addr / 4096;
        if start_frame + count > self.frame_count {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        for i in 0..count {
            self.set_frame_used(start_frame + i);
        }

        Ok(())
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        if !self.initialized {
            return None;
        }

        if let Some(frame_index) = self.find_next_free_frame(self.next_free_frame) {
            self.set_frame_used(frame_index);
            self.next_free_frame = frame_index + 1;

            let frame_addr = frame_index * 4096;
            Some(PhysFrame::containing_address(PhysAddr::new(frame_addr as u64)))
        } else {
            None
        }
    }
}

// Type alias for backward compatibility
pub type BootInfoFrameAllocator = BitmapFrameAllocator;

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

/// Returns the higher-half kernel mapping offset.
pub const HIGHER_HALF_OFFSET: VirtAddr = VirtAddr::new(0xFFFF_8000_0000_0000);

/// Helper function to map a range of physical addresses to the same virtual addresses (identity mapping)
unsafe fn map_identity_range(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    phys_start: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    for i in 0..num_pages {
        let phys_addr = PhysAddr::new(phys_start + i * 4096);
        let virt_addr = VirtAddr::new(phys_start + i * 4096);
        let page = Page::containing_address(virt_addr);
        let frame = PhysFrame::containing_address(phys_addr);
        match unsafe { mapper.map_to(page, frame, flags, frame_allocator) } {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Reinitialize the page table with identity mapping and higher-half kernel mapping
/// This version takes a frame allocator to perform the actual mapping
///
/// Returns the physical memory offset used for the mapping
pub fn reinit_page_table_with_allocator(
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> VirtAddr {
    use x86_64::structures::paging::PageTableFlags as Flags;

    debug_log_no_alloc!("reinit_page_table_with_allocator: starting");

    // Use the higher-half kernel offset
    let phys_offset = HIGHER_HALF_OFFSET;

    // Allocate a new L4 table frame for the kernel
    let level_4_table_frame = frame_allocator
        .allocate_frame()
        .expect("Failed to allocate frame for new L4 table");

    // Zero the new L4 table
    unsafe {
        let l4_table_ptr = level_4_table_frame.start_address().as_u64() as *mut PageTable;
        core::ptr::write_bytes(l4_table_ptr, 0, 1);
    }

    // Create mapper for the new L4 table with identity mapping (offset 0)
    let mut mapper = unsafe {
        let l4_table_ptr = level_4_table_frame.start_address().as_u64() as *mut PageTable;
        OffsetPageTable::new(&mut *l4_table_ptr, VirtAddr::new(0))
    };

    // Set up identity mapping for the first 64MB of physical memory for UEFI compatibility
    // Skip the first page (physical address 0) to avoid null pointer issues
    unsafe {
        map_identity_range(
            &mut mapper,
            frame_allocator,
            4096,
            16383, // 64MB - 4KB = 16383 pages
            Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
        )
        .expect("Failed to map identity range")
    }

    // Identity map kernel code for CR3 switch
    let kernel_size = unsafe { calculate_kernel_memory_size(kernel_phys_start) };
    let kernel_pages = kernel_size.div_ceil(4096);
    unsafe {
        map_identity_range(
            &mut mapper,
            frame_allocator,
            kernel_phys_start.as_u64(),
            kernel_pages,
            Flags::PRESENT | Flags::WRITABLE,
        )
        .expect("Failed to identity map kernel")
    }

    // Map kernel at higher half by parsing the ELF file for permissions
    unsafe {
        map_kernel_segments(&mut mapper, kernel_phys_start, phys_offset, frame_allocator);
    }

    // Map framebuffer if provided
    if let (Some(fb_addr), Some(fb_size)) = (fb_addr, fb_size) {
        let fb_pages = fb_size.div_ceil(4096); // Round up to page count
        let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE;
        map_pages_loop!(
            mapper,
            frame_allocator,
            fb_addr.as_u64(),
            phys_offset.as_u64() + fb_addr.as_u64(),
            fb_pages,
            flags
        );
    }

    // Always map VGA memory regions (0xA0000 - 0xC0000) for compatibility with VGA text/graphics modes
    const VGA_MEMORY_START: u64 = 0xA0000;
    const VGA_MEMORY_SIZE: u64 = 0xC0000 - 0xA0000; // 128KB VGA memory aperture
    let vga_pages = VGA_MEMORY_SIZE / 4096;
    let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE;
    map_pages_loop!(
        mapper,
        frame_allocator,
        VGA_MEMORY_START,
        phys_offset.as_u64() + VGA_MEMORY_START,
        vga_pages,
        flags
    );

    // Map framebuffer to identity for bootloader compatibility
    if let (Some(fb_addr), Some(fb_size)) = (fb_addr, fb_size) {
        let fb_pages = fb_size.div_ceil(4096);
        unsafe {
            map_identity_range(
                &mut mapper,
                frame_allocator,
                fb_addr.as_u64(),
                fb_pages,
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
            )
            .expect("Failed to map framebuffer identity pages");
        }
    }

    // Map the L4 page table to higher-half so that OffsetPageTable with high offset can access it
    let l4_virt = phys_offset + level_4_table_frame.start_address().as_u64();
    let page = Page::containing_address(l4_virt);
    unsafe {
        mapper
            .map_to(
                page,
                level_4_table_frame,
                Flags::PRESENT | Flags::WRITABLE,
                frame_allocator,
            )
            .expect("Failed to map L4 to higher half")
            .flush();
    }

    // Switch to the new page table and flush TLB
    unsafe {
        Cr3::write(
            level_4_table_frame,
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }
    flush_tlb_and_verify!();
    debug_log_no_alloc!("reinit_page_table_with_allocator: CR3 switched, phys_offset=", phys_offset.as_u64());

    phys_offset
}

/// Allocate heap memory from EFI memory map
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> PhysAddr {
    const FRAME_SIZE: u64 = 4096;
    let _heap_frames = (heap_size + FRAME_SIZE as usize - 1) / FRAME_SIZE as usize;

    let aligned_start = if start_addr.as_u64() % FRAME_SIZE == 0 {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    aligned_start
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
    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>>;
    fn translate_address(&self, virtual_addr: usize)
    -> crate::common::logging::SystemResult<usize>;
    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: PageFlags,
    ) -> crate::common::logging::SystemResult<()>;
    fn get_page_flags(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PageFlags>;
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
    pub fn init_paging(
        &mut self,
        physical_memory_offset: VirtAddr,
    ) -> crate::common::logging::SystemResult<()> {
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
            let mapper = OffsetPageTable::new(&mut *table_ptr, physical_memory_offset);
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

    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<x86_64::structures::paging::PhysFrame<Size4KiB>> {
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

        match mapper.translate_addr(virt_addr) {
            Some(phys_addr) => Ok(phys_addr.as_u64() as usize),
            None => Err(crate::common::logging::SystemError::InvalidArgument),
        }
    }

    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: PageFlags,
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
    ) -> crate::common::logging::SystemResult<PageFlags> {
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

    fn destroy_page_table(
        &mut self,
        _table_addr: usize,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        Ok(())
    }

    fn clone_page_table(
        &mut self,
        _source_table: usize,
    ) -> crate::common::logging::SystemResult<usize> {
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

/// Compute the total memory size required for the kernel by parsing ELF headers
pub unsafe fn calculate_kernel_memory_size(kernel_phys_start: PhysAddr) -> u64 {
    let ehdr_ptr = kernel_phys_start.as_u64() as *const Elf64Ehdr;
    let ehdr = unsafe { &*ehdr_ptr };

    // Check ELF magic
    if ehdr.e_ident[0..4] != [0x7f, b'E', b'L', b'F'] {
        // If not ELF, fall back to hardcoded size
        const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;
        return FALLBACK_KERNEL_SIZE;
    }

    // Parse program headers to find total memory size
        let mut min_vaddr = u64::MAX;
    let mut max_vaddr = 0u64;
    let phdr_base = kernel_phys_start.as_u64() + ehdr.e_phoff;
    for i in 0..ehdr.e_phnum {
        let phdr_ptr = (phdr_base + i as u64 * ehdr.e_phentsize as u64) as *const Elf64Phdr;
        let phdr = unsafe { &*phdr_ptr };

        if phdr.p_type == 1 && phdr.p_memsz > 0 { // PT_LOAD
            min_vaddr = min_vaddr.min(phdr.p_vaddr);
            max_vaddr = max_vaddr.max(phdr.p_vaddr + phdr.p_memsz);
        }
    }

    let kernel_size = if min_vaddr <= max_vaddr {
        max_vaddr - min_vaddr
    } else {
        0
    };

    // Round up to page size and add some padding for safety
    const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024; // 1MB padding
    ((kernel_size + 4095) & !4095) + KERNEL_MEMORY_PADDING
}

/// Map kernel segments with appropriate permissions parsed from the ELF file
unsafe fn map_kernel_segments(
    mapper: &mut OffsetPageTable,
    kernel_phys_start: PhysAddr,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    let ehdr_ptr = kernel_phys_start.as_u64() as *const Elf64Ehdr;
    let ehdr = unsafe { &*ehdr_ptr };

    // Check ELF magic
    if ehdr.e_ident[0..4] != [0x7f, b'E', b'L', b'F'] {
        // If not ELF, fall back to mapping as writable (old behavior)
        const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024; // As defined in calculate_kernel_memory_size
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        for i in 0..kernel_pages {
            let phys_addr = kernel_phys_start + (i * 4096);
            let virt_addr = phys_offset + phys_addr.as_u64();
            let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt_addr);
            let frame =
                x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(phys_addr);
            let flags = Flags::PRESENT | Flags::WRITABLE;
            unsafe {
                mapper
                    .map_to(page, frame, flags, frame_allocator)
                    .expect("Failed to map kernel page")
                    .flush();
            }
        }
        return;
    }

    // Parse program headers
    let phdr_base = kernel_phys_start.as_u64() + ehdr.e_phoff;
    for i in 0..ehdr.e_phnum {
        let phdr_ptr = (phdr_base + i as u64 * ehdr.e_phentsize as u64) as *const Elf64Phdr;
        let phdr = unsafe { &*phdr_ptr };

        if phdr.p_type == 1 {
            // PT_LOAD
            // Map the segment
            let segment_start_phys = kernel_phys_start.as_u64() + phdr.p_offset;
            let segment_start_virt = phdr.p_vaddr;
            let segment_size = phdr.p_memsz;

            // Derive flags from p_flags
            let mut flags = Flags::PRESENT;
            if (phdr.p_flags & 2) != 0 {
                flags |= Flags::WRITABLE;
            }
            if (phdr.p_flags & 1) == 0 {
                flags |= Flags::NO_EXECUTE;
            }
            // Read bit is always present for loadable segments

            let pages = segment_size.div_ceil(4096);
            for p in 0..pages {
                let phys_addr = PhysAddr::new(segment_start_phys + p * 4096);
                let virt_addr = VirtAddr::new(segment_start_virt + p * 4096);
                let page =
                    x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt_addr);
                let frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
                    phys_addr,
                );
                unsafe {
                    mapper
                        .map_to(page, frame, flags, frame_allocator)
                        .expect("Failed to map kernel segment")
                        .flush();
                }
            }
        }
    }
}
