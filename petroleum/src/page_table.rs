use crate::{
    bitmap_operation, calc_offset_addr, create_page_and_frame, debug_log_no_alloc,
    ensure_initialized, flush_tlb_and_verify, log_memory_descriptor, map_and_flush,
    map_identity_range_checked, map_pages_loop, map_with_offset,
};

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

// Macros and constants
macro_rules! read_unaligned {
    ($ptr:expr, $offset:expr, $ty:ty) => {{
        core::ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty)
    }};
}

pub static HEAP_INITIALIZED: Once<bool> = Once::new();

pub fn init_global_heap(ptr: *mut u8, size: usize) {
    if HEAP_INITIALIZED.get().is_none() {
        unsafe { ALLOCATOR.lock().init(ptr, size); }
        HEAP_INITIALIZED.call_once(|| true);
    }
}

// Generic PeParser to reduce lines from multiple PE functions
pub struct PeParser {
    pe_base: *const u8,
}

impl PeParser {
    pub unsafe fn new(kernel_ptr: *const u8) -> Option<Self> {
        find_pe_base(kernel_ptr).map(|base| Self { pe_base: base })
    }

    pub unsafe fn size_of_image(&self) -> Option<u64> {
        let pe_offset = read_unaligned!(self.pe_base, 0x3c, u32) as usize;
        if pe_offset == 0 || pe_offset >= 1024 * 1024 { return None; }
        let magic = read_unaligned!(self.pe_base, pe_offset + 24, u16);
        if magic != 0x10B && magic != 0x20B { return None; }
        Some(read_unaligned!(self.pe_base, pe_offset + 24 + 0x38, u32) as u64)
    }

    pub unsafe fn sections(&self) -> Option<[PeSection; 16]> {
        let pe_offset = read_unaligned!(self.pe_base, 0x3c, u32) as usize;
        if pe_offset == 0 || pe_offset >= 1024 * 1024 { return None; }
        let num_sections = read_unaligned!(self.pe_base, pe_offset + 6, u16) as usize;
        let optional_header_size = read_unaligned!(self.pe_base, pe_offset + 20, u16) as usize;
        let section_table_offset = pe_offset + 24 + optional_header_size;
        let mut sections = [PeSection {
            name: [0; 8], virtual_size: 0, virtual_address: 0, size_of_raw_data: 0,
            pointer_to_raw_data: 0, characteristics: 0
        }; 16];
        for i in 0..num_sections.min(16) {
            let offset = section_table_offset + i * 40;
            for j in 0..8 { sections[i].name[j] = *self.pe_base.add(offset + j); }
            sections[i].virtual_size = read_unaligned!(self.pe_base, offset + 8, u32);
            sections[i].virtual_address = read_unaligned!(self.pe_base, offset + 12, u32);
            sections[i].size_of_raw_data = read_unaligned!(self.pe_base, offset + 16, u32);
            sections[i].pointer_to_raw_data = read_unaligned!(self.pe_base, offset + 20, u32);
            sections[i].characteristics = read_unaligned!(self.pe_base, offset + 36, u32);
        }
        Some(sections)
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

/// Named constant for UEFI firmware specific memory type (replace magic number)
const EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC: u32 = 15;

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

// PE parsing constants
const MAX_PE_SEARCH_DISTANCE: usize = 10 * 1024 * 1024;
const MAX_PE_OFFSET: usize = 16 * 1024 * 1024;
const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024;
const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;

// Helper function to find the PE base address by searching backwards for MZ signature
unsafe fn find_pe_base(start_ptr: *const u8) -> Option<*const u8> {
    for i in 0..MAX_PE_SEARCH_DISTANCE {
        if (start_ptr as u64) < i as u64 {
            break;
        }
        let candidate_ptr = start_ptr.sub(i);
        if candidate_ptr.read() == b'M' && candidate_ptr.add(1).read() == b'Z' {
            let pe_offset = read_unaligned!(candidate_ptr, 0x3c, u32) as usize;
            if pe_offset > 0 && pe_offset < MAX_PE_OFFSET {
                let pe_sig = read_unaligned!(candidate_ptr, pe_offset, u32);
                if pe_sig == 0x00004550 { // "PE\0\0"
                    return Some(candidate_ptr);
                }
            }
        }
    }
    None
}

// Derive page table flags from PE section characteristics
fn derive_pe_flags(characteristics: u32) -> x86_64::structures::paging::PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    let mut flags = Flags::PRESENT;
    if (characteristics & 0x8000_0000) != 0 { // IMAGE_SCN_MEM_WRITE
        flags |= Flags::WRITABLE;
    }
    if (characteristics & 0x2000_0000) == 0 { // NOT IMAGE_SCN_MEM_EXECUTE
        flags |= Flags::NO_EXECUTE;
    }
    flags
}

// Map a single PE section to virtual memory
unsafe fn map_pe_section(
    mapper: &mut OffsetPageTable,
    section: PeSection,
    kernel_phys_start: PhysAddr,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let flags = derive_pe_flags(section.characteristics);
    let section_start_phys = kernel_phys_start.as_u64() + section.pointer_to_raw_data as u64;
    let section_start_virt = phys_offset.as_u64() + section.virtual_address as u64;
    let section_size = section.virtual_size as u64;
    let pages = section_size.div_ceil(4096);
    for p in 0..pages {
        let phys_addr = calc_offset_addr!(section_start_phys, p);
        let virt_addr = calc_offset_addr!(section_start_virt, p);
        map_with_offset!(mapper, frame_allocator, phys_addr, virt_addr, flags);
    }
}

// Calculate frame allocation parameters from memory map
fn calculate_frame_allocation_params(memory_map: &[EfiMemoryDescriptor]) -> (u64, usize, usize) {
    let max_addr = memory_map
        .iter()
        .map(|d| d.physical_start.saturating_add(d.number_of_pages.saturating_mul(4096)))
        .max()
        .unwrap_or(0);
    let capped_max_addr = max_addr.min(32 * 1024 * 1024 * 1024u64);
    let total_frames = (capped_max_addr.div_ceil(4096)) as usize;
    let bitmap_size = (total_frames + 63) / 64;
    (max_addr, total_frames, bitmap_size)
}

// Mark available frames as free based on memory map
fn mark_available_frames(
    frame_allocator: &mut BitmapFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
) {
    for descriptor in memory_map {
        if descriptor.type_ == crate::common::EfiMemoryType::EfiConventionalMemory
            || descriptor.type_ as u32 == EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC
        {
            let start_frame = (descriptor.physical_start / 4096) as usize;
            let end_frame = start_frame + descriptor.number_of_pages as usize;
            for frame_index in start_frame..end_frame {
                if frame_index < frame_allocator.frame_count {
                    frame_allocator.set_frame_free(frame_index);
                }
            }
        }
    }
    // Mark frame 0 as used to avoid allocating the null page
    frame_allocator.set_frame_used(0);
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
    ///
    /// # Safety
    ///
    /// This function is unsafe because calling it multiple times will cause
    /// mutable aliasing of the global static `BITMAP_STATIC` buffer, leading
    /// to undefined behavior. It must only be called once during system initialization.
    /// (for compatibility)
    pub unsafe fn init(memory_map: &[EfiMemoryDescriptor]) -> Self {
        let mut allocator = BitmapFrameAllocator::new();
        unsafe {
            allocator
                .init_with_memory_map(memory_map)
                .expect("Failed to init bitmap allocator");
        }
        allocator
    }

    /// Initialize with EFI memory map
    pub unsafe fn init_with_memory_map(
        &mut self,
        memory_map: &[EfiMemoryDescriptor],
    ) -> crate::common::logging::SystemResult<()> {
        // Debug: Log memory map information
        debug_log_no_alloc!("Memory map contains ", memory_map.len(), " descriptors");

        // Validate memory map is not empty
        if memory_map.is_empty() {
            debug_log_no_alloc!("ERROR: Empty memory map received");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Debug: Log each descriptor
        for (i, desc) in memory_map.iter().enumerate() {
            log_memory_descriptor!(desc, i);
        }

        let (max_addr, total_frames, bitmap_size) = calculate_frame_allocation_params(memory_map);

        debug_log_no_alloc!("Max address: 0x", max_addr as usize);
        debug_log_no_alloc!("Calculated total frames: ", total_frames);

        if total_frames == 0 {
            debug_log_no_alloc!("ERROR: No valid frames found in memory map");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        debug_log_no_alloc!("Required bitmap size: ", bitmap_size);

        // Ensure bitmap size doesn't exceed our static buffer
        if bitmap_size > 131072 {
            debug_log_no_alloc!("ERROR: Bitmap size ", bitmap_size, " exceeds limit 131072");
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

        // Mark available frames as free based on memory map
        mark_available_frames(self, memory_map);

        debug_log_no_alloc!(
            "BitmapFrameAllocator initialized successfully with ",
            total_frames,
            " frames"
        );

        Ok(())
    }

    /// Set a frame as free in the bitmap
    fn set_frame_free(&mut self, frame_index: usize) {
        bitmap_operation!(self.bitmap, frame_index, set_free);
    }

    /// Set a frame as used in the bitmap
    fn set_frame_used(&mut self, frame_index: usize) {
        bitmap_operation!(self.bitmap, frame_index, set_used);
    }

    /// Check if a frame is free
    fn is_frame_free(&self, frame_index: usize) -> bool {
        bitmap_operation!(self.bitmap, frame_index, is_free)
    }

    /// Find the next free frame starting from a given index
    fn find_next_free_frame(&self, start_index: usize) -> Option<usize> {
        if !self.initialized {
            return None;
        }

        self.bitmap.as_ref().and_then(|bitmap| Self::find_frame_in_bitmap(bitmap, start_index, self.frame_count))
    }

    /// Helper method for bitmap operations
    fn find_frame_in_bitmap(bitmap: &[u64], start_index: usize, frame_count: usize) -> Option<usize> {
        let mut chunk_index = start_index / 64;
        let bit_in_chunk = start_index % 64;

        if chunk_index < bitmap.len() {
            let mut chunk = bitmap[chunk_index];
            chunk |= (1u64.wrapping_shl(bit_in_chunk as u32)).wrapping_sub(1);
            if chunk != u64::MAX {
                let first_free_bit = (!chunk).trailing_zeros() as usize;
                if chunk_index * 64 + first_free_bit < frame_count {
                    return Some(chunk_index * 64 + first_free_bit);
                }
            }
            chunk_index += 1;
        }

        for i in chunk_index..bitmap.len() {
            if bitmap[i] != u64::MAX {
                let first_free_bit = (!bitmap[i]).trailing_zeros() as usize;
                if i * 64 + first_free_bit < frame_count {
                    return Some(i * 64 + first_free_bit);
                }
            }
        }
        None
    }

    /// Allocate a specific frame range (for reserving used regions)
    pub fn allocate_frames_at(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        ensure_initialized!(self);

        let start_frame = start_addr / 4096;
        if start_frame + count > self.frame_count {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        for i in 0..count {
            if !self.is_frame_free(start_frame + i) {
                debug_log_no_alloc!(
                    "Frame allocation failed: frame already in use at index ",
                    start_frame + i
                );
                return Err(crate::common::logging::SystemError::FrameAllocationFailed);
            }
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
            Some(PhysFrame::containing_address(PhysAddr::new(
                frame_addr as u64,
            )))
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
    map_identity_range_checked!(mapper, frame_allocator, phys_start, num_pages, flags)
}

// Helper to map kernel segments with proper permissions
unsafe fn map_kernel_segments_inner(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    kernel_phys_start: PhysAddr,
    phys_offset: VirtAddr
) {
    if let Some(sections) = unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8).and_then(|p| p.sections()) } {
        for section in sections.into_iter().filter(|s| s.virtual_size > 0) {
            unsafe { map_pe_section(mapper, section, kernel_phys_start, phys_offset, frame_allocator); }
        }
    }
}

// Helper to map additional regions like framebuffer and VGA
fn map_additional_regions(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    phys_offset: VirtAddr
) {
    unsafe {
        use x86_64::structures::paging::PageTableFlags as Flags;

        // Map framebuffer if provided
        if let (Some(fb_addr), Some(fb_size)) = (fb_addr, fb_size) {
            map_pages_loop!(mapper, frame_allocator, fb_addr.as_u64(), phys_offset.as_u64() + fb_addr.as_u64(), fb_size.div_ceil(4096), Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE);
        }

        // Always map VGA memory
        map_pages_loop!(mapper, frame_allocator, 0xA0000, phys_offset.as_u64() + 0xA0000, (0xC0000 - 0xA0000)/4096, Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE);
    }
}

// Helper to adjust return address after page table switch
fn adjust_return_address(phys_offset: VirtAddr) {
    unsafe {
        let mut base_pointer: u64;
        core::arch::asm!("mov {}, rbp", out(reg) base_pointer);
        let return_address_ptr = (base_pointer as *mut u64).add(1);
        *return_address_ptr = phys_offset.as_u64() + *return_address_ptr;
    }
}

pub fn reinit_page_table_with_allocator(
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> VirtAddr {
    use x86_64::structures::paging::PageTableFlags as Flags;
    debug_log_no_alloc!("Reinit start");
    let phys_offset = HIGHER_HALF_OFFSET;
    let level_4_table_frame = frame_allocator.allocate_frame().expect("L4 alloc");
    unsafe {
        core::ptr::write_bytes(level_4_table_frame.start_address().as_u64() as *mut PageTable, 0, 1);
        let mut mapper = OffsetPageTable::new(&mut *(level_4_table_frame.start_address().as_u64() as *mut PageTable), VirtAddr::new(0));
        map_identity_range(&mut mapper, frame_allocator, 4096, 16383, Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE).unwrap();
        let kernel_size = calculate_kernel_memory_size(kernel_phys_start);
        map_identity_range(&mut mapper, frame_allocator, kernel_phys_start.as_u64(), kernel_size.div_ceil(4096), Flags::PRESENT | Flags::WRITABLE).unwrap();
        map_kernel_segments_inner(&mut mapper, frame_allocator, kernel_phys_start, phys_offset);
        map_additional_regions(&mut mapper, frame_allocator, fb_addr, fb_size, phys_offset);
        mapper.map_to(Page::containing_address(phys_offset + level_4_table_frame.start_address().as_u64()), level_4_table_frame, Flags::PRESENT | Flags::WRITABLE, frame_allocator).unwrap().flush();
        Cr3::write(level_4_table_frame, x86_64::registers::control::Cr3Flags::empty());
    }
    flush_tlb_and_verify!();
    adjust_return_address(phys_offset);
    debug_log_no_alloc!("Reinit done");
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

// Generic Memory Mapper structure to reduce repetitive mapping code
pub struct MemoryMapper<'a> {
    mapper: &'a mut OffsetPageTable<'a>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
}

impl<'a> MemoryMapper<'a> {
    pub fn new(
        mapper: &'a mut OffsetPageTable<'a>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
        }
    }

    // Generic method to map a range with given flags
    pub unsafe fn map_range(&mut self, phys_start: u64, virt_start: u64, num_pages: u64, flags: x86_64::structures::paging::PageTableFlags) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        map_identity_range_checked!(self.mapper, self.frame_allocator, phys_start, num_pages, flags)
    }

    // Map kernel segments using PE parsing
    pub unsafe fn map_kernel_segments(&mut self, kernel_phys_start: PhysAddr) {
        map_kernel_segments_inner(self.mapper, self.frame_allocator, kernel_phys_start, self.phys_offset);
    }
}

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
        ensure_initialized!(self);

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
        ensure_initialized!(self);

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
        ensure_initialized!(self);

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
        ensure_initialized!(self);

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
        ensure_initialized!(self);

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
        ensure_initialized!(self);

        tlb::flush(VirtAddr::new(virtual_addr as u64));
        Ok(())
    }

    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()> {
        ensure_initialized!(self);

        let (current, flags) = Cr3::read();
        unsafe { Cr3::write(current, flags) };
        Ok(())
    }

    fn create_page_table(&mut self) -> crate::common::logging::SystemResult<usize> {
        ensure_initialized!(self);

        // Return a dummy address
        Ok(0x1000)
    }

    fn destroy_page_table(
        &mut self,
        _table_addr: usize,
    ) -> crate::common::logging::SystemResult<()> {
        ensure_initialized!(self);

        Ok(())
    }

    fn clone_page_table(
        &mut self,
        _source_table: usize,
    ) -> crate::common::logging::SystemResult<usize> {
        ensure_initialized!(self);

        Ok(_source_table + 0x1000) // Dummy offset
    }

    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()> {
        ensure_initialized!(self);

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

/// Simple PE section structure for manual parsing
#[derive(Debug, Clone, Copy)]
pub struct PeSection {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub characteristics: u32,
}

pub unsafe fn calculate_kernel_memory_size(kernel_phys_start: PhysAddr) -> u64 {
    debug_log_no_alloc!("calculate_kernel_memory_size: starting");
    if kernel_phys_start.as_u64() == 0 { return FALLBACK_KERNEL_SIZE; }
    let parser = match unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
        Some(p) => p,
        None => return FALLBACK_KERNEL_SIZE,
    };
    match parser.size_of_image() {
        Some(size) => ((size + 4095) & !4095) + KERNEL_MEMORY_PADDING,
        None => FALLBACK_KERNEL_SIZE,
    }
}
