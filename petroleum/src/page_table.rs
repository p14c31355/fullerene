use crate::{bitmap_operation, debug_log_no_alloc, ensure_initialized, flush_tlb_and_verify, map_pages_loop, calc_offset_addr, create_page_and_frame, map_and_flush, map_with_offset, log_memory_descriptor, map_identity_range_checked};

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

        // 1. Find the highest physical address in any memory descriptor to determine the total number of frames to manage.
        // We need to track all physical memory, even non-conventional, to be able to reserve regions like kernel code.
        let max_addr = memory_map
            .iter()
            .map(|d| {
                let pages_size = d.number_of_pages.saturating_mul(4096);
                d.physical_start.saturating_add(pages_size)
            })
            .max()
            .unwrap_or(0);
        // Cap at 32GB to avoid excessive bitmap size
        const MAX_SUPPORTED_BYTES: u64 = 32 * 1024 * 1024 * 1024; // 32GB
        let capped_max_addr = max_addr.min(MAX_SUPPORTED_BYTES);
        let total_frames = (capped_max_addr.div_ceil(4096)) as usize;

        debug_log_no_alloc!("Max address: 0x", max_addr as usize);
        debug_log_no_alloc!("Calculated total frames: ", total_frames);

        if total_frames == 0 {
            debug_log_no_alloc!("ERROR: No valid frames found in memory map");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Calculate bitmap size needed
        let bitmap_size = (total_frames + 63) / 64; // Round up for 64-bit chunks

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

        // Mark available frames as free based on their physical address
        for descriptor in memory_map {
            if descriptor.type_ == crate::common::EfiMemoryType::EfiConventionalMemory
                || descriptor.type_ as u32 == EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC {
                let start_frame = (descriptor.physical_start / 4096) as usize;
                let end_frame = start_frame + descriptor.number_of_pages as usize;

                for frame_index in start_frame..end_frame {
                    if frame_index < self.frame_count {
                        self.set_frame_free(frame_index);
                    }
                }
            }
        }

        // Mark frame 0 as used to avoid allocating the null page
        self.set_frame_used(0);

        debug_log_no_alloc!("BitmapFrameAllocator initialized successfully with ", total_frames, " frames");

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

        if let Some(ref bitmap) = self.bitmap {
            let mut chunk_index = start_index / 64;
            let bit_in_chunk = start_index % 64;

            if chunk_index < bitmap.len() {
                let mut chunk = bitmap[chunk_index];
                // Create a mask with all bits set before the start_index bit position
                // This effectively ignores any free bits before start_index in the bitmap chunk
                // For example, if bit_in_chunk is 3, this creates a mask: 0b000...0111 (bits 0-2 set)
                // which marks the lower bits as used so they won't be considered free
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

use core::arch::naked_asm;

/// Switch to higher-half virtual address space after CR3 switch.
///
/// After switching page tables with CR3, the return address on the stack is still
/// an identity-mapped address. This naked function adjusts it to point to the
/// new higher-half virtual address space by adding the physical offset.
///
/// This approach ensures robustness across different compiler versions and
/// optimization levels, as it has full control over the stack manipulation.
///
/// # Safety
/// This function must be called immediately after CR3 switch and before returning
/// from the calling function. The higher-half mapping must be properly set up.
#[unsafe(naked)]
pub extern "C" fn switch_to_higher_half(phys_offset: u64) {
    // phys_offset in rdi
    naked_asm!(
        // Calculate return address location: rbp + 8
        "mov %rbp, %rax",
        "lea 0x8(%rax), %rax",

        // Load current return address
        "mov (%rax), %rcx",

        // Adjust return address: rcx = rcx + phys_offset (rdi)
        "add %rdi, %rcx",

        // Store adjusted return address
        "mov %rcx, (%rax)",

        // Return
        "ret",
    );
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

    debug_log_no_alloc!("About to map kernel segments");
    // Map kernel at higher half by parsing the ELF file for permissions
    unsafe {
        map_kernel_segments(&mut mapper, kernel_phys_start, phys_offset, frame_allocator);
    }
    debug_log_no_alloc!("Kernel segments mapped");

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

    // After CR3 switch, we must adjust the return address on the stack to point to the new
    // higher-half virtual address space. The current return address is an identity-mapped address.
    // WARNING: This code assumes frame pointers (rbp) are available and enabled, and relies on
    // the standard stack layout where the return address is at [rbp + 8]. This may not hold for
    // all compiler versions or optimization levels, especially in debug builds where
    // force-frame-pointers is not set by default. Violation could lead to stack corruption or crash.
    // This is acknowledged as fragile but necessary for the higher-half kernel transition.
    unsafe {
        let mut base_pointer: u64;
        core::arch::asm!("mov {}, rbp", out(reg) base_pointer);
        let return_address_ptr = (base_pointer as *mut u64).add(1); // Return address is at [rbp + 8]
        let current_return_addr = *return_address_ptr;
        let adjusted_return_addr = phys_offset.as_u64() + current_return_addr;
        *return_address_ptr = adjusted_return_addr;
    }

    debug_log_no_alloc!(
        "reinit_page_table_with_allocator: CR3 switched, return address adjusted, phys_offset=",
        phys_offset.as_u64()
    );

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

/// Manually parse PE SizeOfImage from the optional header
pub unsafe fn calculate_kernel_memory_size(kernel_phys_start: PhysAddr) -> u64 {
    debug_log_no_alloc!("calculate_kernel_memory_size: starting manual PE parsing");

    let mut kernel_ptr = kernel_phys_start.as_u64() as *const u8;

    // Check if we have at least enough data for the PE signature
    if kernel_phys_start.as_u64() == 0 {
        debug_log_no_alloc!("calculate_kernel_memory_size: null kernel_phys_start");
        return 64 * 1024 * 1024; // fallback
    }

    // Search backwards from kernel_phys_start to find a valid DOS header (MZ signature)
    // and valid PE structure, since kernel_phys_start points to entry point inside the PE image
    const MAX_SEARCH_DISTANCE: usize = 10 * 1024 * 1024; // 10MB max search distance
    const MAX_PE_OFFSET: usize = 16 * 1024 * 1024; // 16MB max PE offset
    let _search_offset = 0usize;
    let _dos_ptr = kernel_ptr as *const u16;
    let mut found_valid_pe = false;

    // Search backwards, checking each 2-byte aligned address for valid PE structure
        // Search backwards byte-by-byte for robustness against alignment issues.
    for i in 0..MAX_SEARCH_DISTANCE {
        if (kernel_ptr as u64) < i as u64 {
            // We've searched back to address 0 or beyond, abort
            debug_log_no_alloc!("calculate_kernel_memory_size: Search exceeded address range, using fallback");
            return 64 * 1024 * 1024;
        }
        let candidate_ptr = unsafe { kernel_ptr.sub(i) };

        // Check for 'M' and 'Z'
        if unsafe { candidate_ptr.read() == b'M' && candidate_ptr.add(1).read() == b'Z' } {
            // Found "MZ" signature, now validate it's a real PE file.
            let pe_offset_ptr = unsafe { candidate_ptr.add(0x3c) as *const u32 };
            let pe_offset = unsafe { pe_offset_ptr.read_unaligned() } as usize;

            // A simple sanity check for the offset
            if pe_offset > 0 && pe_offset < MAX_PE_OFFSET {
                let pe_sig_ptr = unsafe { candidate_ptr.add(pe_offset) as *const u32 };
                if unsafe { pe_sig_ptr.read_unaligned() } == 0x00004550 { // "PE\0\0"
                    kernel_ptr = candidate_ptr;
                    debug_log_no_alloc!("calculate_kernel_memory_size: Found valid PE at offset -", i);
                    debug_log_no_alloc!("calculate_kernel_memory_size: PE base address = 0x", kernel_ptr as usize);
                    found_valid_pe = true;
                    break;
                }
            }
        }
    }


    if !found_valid_pe {
        debug_log_no_alloc!("calculate_kernel_memory_size: Valid PE header not found within search limit, using fallback");
        return 64 * 1024 * 1024;
    }

    // Valid PE header found, kernel_ptr now points to the PE base

    // Get PE header offset from DOS header (offset 0x3c)
    let pe_offset = unsafe { core::ptr::read_unaligned(kernel_ptr.add(0x3c) as *const u32) } as usize;
    if pe_offset == 0 || pe_offset >= 1024 * 1024 {
        debug_log_no_alloc!("calculate_kernel_memory_size: Invalid PE offset, using fallback");
        return 64 * 1024 * 1024;
    }

    // Check PE signature
    let pe_signature_ptr = unsafe { kernel_ptr.add(pe_offset) as *const u32 };
    let pe_signature = unsafe { core::ptr::read_unaligned(pe_signature_ptr) };
    if pe_signature != 0x00004550 { // "PE\0\0"
        debug_log_no_alloc!("calculate_kernel_memory_size: Invalid PE signature, using fallback");
        return 64 * 1024 * 1024;
    }

    // Get to optional header - after COFF header (24 bytes: signature + 20 bytes COFF)
    let optional_header_ptr = unsafe { kernel_ptr.add(pe_offset + 24) as *const u16 };

    // First 2 bytes of optional header is magic - should be 0x10B (PE32) or 0x20B (PE32+)
    let optional_magic = unsafe { core::ptr::read_unaligned(optional_header_ptr) };
    if optional_magic != 0x10B && optional_magic != 0x20B {
        debug_log_no_alloc!("calculate_kernel_memory_size: Invalid optional header magic, using fallback");
        return 64 * 1024 * 1024;
    }

// SizeOfImage is at offset 0x38 in the optional header for both PE32 and PE32+.
let size_of_image_offset = 0x38;
    let size_of_image_ptr = unsafe { kernel_ptr.add(pe_offset + 24 + size_of_image_offset as usize) as *const u32 };
    let size_of_image = unsafe { core::ptr::read_unaligned(size_of_image_ptr) } as u64;

    debug_log_no_alloc!("calculate_kernel_memory_size: PE parsed successfully, SizeOfImage=", size_of_image);

    // Round up to page size and add some padding for safety
    const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024; // 1MB padding
    let result = ((size_of_image + 4095) & !4095) + KERNEL_MEMORY_PADDING;
    debug_log_no_alloc!("calculate_kernel_memory_size: final result=", result);
    result
}

pub unsafe fn parse_pe_sections(kernel_phys_start: PhysAddr) -> Option<[PeSection; 16]> {
    let mut kernel_ptr = kernel_phys_start.as_u64() as *const u8;

    // Search backwards from kernel_phys_start to find a valid DOS header (MZ signature)
    // and valid PE structure, since kernel_phys_start points to entry point inside the PE image
    const MAX_SEARCH_DISTANCE: usize = 10 * 1024 * 1024; // 10MB max search distance
    const MAX_PE_OFFSET: usize = 16 * 1024 * 1024; // 16MB max PE offset
    let mut found_valid_pe = false;

    // Search backwards byte-by-byte for robustness against alignment issues.
    for i in 0..MAX_SEARCH_DISTANCE {
        if (kernel_ptr as u64) < i as u64 {
            // We've searched back to address 0 or beyond, abort
            return None;
        }
        let candidate_ptr = unsafe { kernel_ptr.sub(i) };

        // Check for 'M' and 'Z'
        if unsafe { candidate_ptr.read() == b'M' && candidate_ptr.add(1).read() == b'Z' } {
            // Found "MZ" signature, now validate it's a real PE file.
            let pe_offset_ptr = unsafe { candidate_ptr.add(0x3c) as *const u32 };
            let pe_offset = unsafe { pe_offset_ptr.read_unaligned() } as usize;

            // A simple sanity check for the offset
            if pe_offset > 0 && pe_offset < MAX_PE_OFFSET {
                let pe_sig_ptr = unsafe { candidate_ptr.add(pe_offset) as *const u32 };
                if unsafe { pe_sig_ptr.read_unaligned() } == 0x00004550 { // "PE\0\0"
                    // Valid PE structure found
                    kernel_ptr = candidate_ptr;
                    found_valid_pe = true;
                    break;
                }
            }
        }
    }

    if !found_valid_pe {
        return None;
    }

    // Get PE header offset from DOS header (offset 0x3c)
    let pe_offset = unsafe { core::ptr::read_unaligned(kernel_ptr.add(0x3c) as *const u32) } as usize;
    if pe_offset == 0 || pe_offset >= 1024 * 1024 {
        return None;
    }

    // Check PE signature
    let pe_signature_ptr = unsafe { kernel_ptr.add(pe_offset) as *const u32 };
    let pe_signature = unsafe { core::ptr::read_unaligned(pe_signature_ptr) };
    if pe_signature != 0x4550 { // "PE\0\0"
        return None;
    }

    // Get number of sections from COFF header (offset 6 from PE header)
    let num_sections_ptr = unsafe { kernel_ptr.add(pe_offset + 6) as *const u16 };
    let num_sections = unsafe { core::ptr::read_unaligned(num_sections_ptr) } as usize;
    let num_sections = num_sections.min(16); // Cap at 16 sections max

    // Get optional header size from COFF header (offset 20 from PE header)
    let optional_header_size_ptr = unsafe { kernel_ptr.add(pe_offset + 20) as *const u16 };
    let optional_header_size = unsafe { core::ptr::read_unaligned(optional_header_size_ptr) } as usize;

    // Section table starts after optional header
    let section_table_offset = pe_offset + 24 + optional_header_size;
    let mut sections = [PeSection {
        name: [0; 8],
        virtual_size: 0,
        virtual_address: 0,
        size_of_raw_data: 0,
        pointer_to_raw_data: 0,
        characteristics: 0,
    }; 16];

    let mut _section_count = 0;
    for i in 0..num_sections {
        let section_offset = section_table_offset + i * 40; // Each section header is 40 bytes

        let mut name = [0u8; 8];
        for j in 0..8 {
            name[j] = unsafe { *kernel_ptr.add(section_offset + j) };
        }

        let virtual_size = unsafe { core::ptr::read_unaligned(kernel_ptr.add(section_offset + 8) as *const u32) };
        let virtual_address = unsafe { core::ptr::read_unaligned(kernel_ptr.add(section_offset + 12) as *const u32) };
        let size_of_raw_data = unsafe { core::ptr::read_unaligned(kernel_ptr.add(section_offset + 16) as *const u32) };
        let pointer_to_raw_data = unsafe { core::ptr::read_unaligned(kernel_ptr.add(section_offset + 20) as *const u32) };
        let characteristics = unsafe { core::ptr::read_unaligned(kernel_ptr.add(section_offset + 36) as *const u32) };

        sections[_section_count] = PeSection {
            name,
            virtual_size,
            virtual_address,
            size_of_raw_data,
            pointer_to_raw_data,
            characteristics,
        };
        _section_count += 1;
    }

    Some(sections)
}

/// Map kernel segments with appropriate permissions parsed from the PE file
unsafe fn map_kernel_segments(
    mapper: &mut OffsetPageTable,
    kernel_phys_start: PhysAddr,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    debug_log_no_alloc!("map_kernel_segments: starting PE mapping, kernel_phys_start=", kernel_phys_start.as_u64());

    if let Some(sections) = unsafe { parse_pe_sections(kernel_phys_start) } {
        debug_log_no_alloc!("map_kernel_segments: PE parsed successfully, mapping sections");

        // Count non-zero sections (since we have a fixed-size array with padding)
        let mut _section_count = 0;
        for i in 0..sections.len() {
            if sections[i].virtual_size > 0 {
                _section_count += 1;
            }
        }

        for section in sections {
            let section_name = core::str::from_utf8(&section.name).unwrap_or("<invalid>");
            debug_log_no_alloc!("map_kernel_segments: mapping section ", section_name,
                               ", vaddr=", section.virtual_address as u64,
                               ", vsize=", section.virtual_size as u64,
                               ", raw_size=", section.size_of_raw_data as u64);

            // Skip empty sections
            if section.virtual_size == 0 {
                continue;
            }

            // Map the section
            let section_start_phys = kernel_phys_start.as_u64() + section.pointer_to_raw_data as u64;
            let section_start_virt = section.virtual_address as u64;
            let section_size = section.virtual_size as u64;

            // Derive flags from section characteristics
            let mut flags = Flags::PRESENT;
            let characteristics = section.characteristics;
            // PE characteristics are bitflags
            if (characteristics & 0x8000_0000) != 0 { // IMAGE_SCN_MEM_WRITE
                flags |= Flags::WRITABLE;
            }
            if (characteristics & 0x2000_0000) == 0 { // NOT IMAGE_SCN_MEM_EXECUTE
                flags |= Flags::NO_EXECUTE;
            }
            // Read permission is always present for loaded sections

            let pages = section_size.div_ceil(4096);
            for p in 0..pages {
                let phys_addr = calc_offset_addr!(section_start_phys, p);
                // Map to the virtual address specified in the PE section header
                let virt_addr = calc_offset_addr!(phys_offset.as_u64() + section_start_virt, p);
                map_with_offset!(mapper, frame_allocator, phys_addr, virt_addr, flags);
            }
        }
    } else {
        debug_log_no_alloc!("map_kernel_segments: PE parsing failed, using fallback");
        // If PE parsing fails, fall back to mapping as executable and writable
        const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        for i in 0..kernel_pages {
let phys_addr = calc_offset_addr!(kernel_phys_start.as_u64(), i);
let virt_addr = calc_offset_addr!(phys_offset.as_u64(), i);
map_with_offset!(mapper, frame_allocator, phys_addr, virt_addr, Flags::PRESENT | Flags::WRITABLE);
        }
    }
}
