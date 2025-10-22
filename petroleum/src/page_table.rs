use crate::{
    calc_offset_addr, create_page_and_frame, debug_log_no_alloc, ensure_initialized,
    flush_tlb_and_verify, log_memory_descriptor, map_and_flush, map_identity_range_checked,
    map_with_offset,
};

// Macros are automatically available from common module
use spin::Once;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB, Translate,
    },
};

// Macros and constants
// Helper macros and functions to reduce repetitive code
macro_rules! read_unaligned {
    ($ptr:expr, $offset:expr, $ty:ty) => {{ core::ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty) }};
}

// Consolidated logging macro for page table operations
macro_rules! log_page_table_op {
    ($operation:expr) => {
        debug_log_no_alloc!($operation);
    };
    ($operation:expr, $msg:expr, $addr:expr) => {
        debug_log_no_alloc!($operation, $msg, " addr=", $addr);
    };
    ($stage:expr, $phys:expr, $virt:expr, $pages:expr) => {
        debug_log_no_alloc!(
            "Memory mapping stage=",
            $stage,
            " phys=0x",
            $phys,
            " virt=0x",
            $virt,
            " pages=",
            $pages
        );
    };
    ($operation:expr, $msg:expr) => {
        debug_log_no_alloc!($operation, $msg);
    };
}

// Bit manipulation macros to reduce repeated code in BitmapFrameAllocator
macro_rules! bit_set {
    ($bitmap:expr, $index:expr) => {
        if let Some(ref mut bmp) = $bitmap {
            let chunk_index = $index / 64;
            let bit_index = $index % 64;
            if chunk_index < bmp.len() {
                bmp[chunk_index] |= 1 << bit_index;
            }
        }
    };
}

macro_rules! bit_clear {
    ($bitmap:expr, $index:expr) => {
        if let Some(ref mut bmp) = $bitmap {
            let chunk_index = $index / 64;
            let bit_index = $index % 64;
            if chunk_index < bmp.len() {
                bmp[chunk_index] &= !(1 << bit_index);
            }
        }
    };
}

macro_rules! bit_test {
    ($bitmap:expr, $index:expr) => {
        if let Some(ref bmp) = $bitmap {
            let chunk_index = $index / 64;
            let bit_index = $index % 64;
            if chunk_index < bmp.len() {
                (bmp[chunk_index] & (1 << bit_index)) == 0
            } else {
                false
            }
        } else {
            false
        }
    };
}

// Generic validation trait for different descriptor types
trait MemoryDescriptorValidator {
    fn is_valid(&self) -> bool;
    fn get_physical_start(&self) -> u64;
    fn get_page_count(&self) -> u64;
    fn is_memory_available(&self) -> bool;
}

// Generic flag derivation trait for memory types
trait FlagDeriver {
    fn derive_flags(&self) -> PageTableFlags;
}

// Implement for EfiMemoryType
impl FlagDeriver for crate::common::EfiMemoryType {
    fn derive_flags(&self) -> PageTableFlags {
        use x86_64::structures::paging::PageTableFlags as Flags;
        match self {
            crate::common::EfiMemoryType::EfiRuntimeServicesCode => Flags::PRESENT,
            _ => Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
        }
    }
}

// Implementation for EFI memory descriptors
impl MemoryDescriptorValidator for EfiMemoryDescriptor {
    fn is_valid(&self) -> bool {
        is_valid_memory_descriptor(self)
    }

    fn get_physical_start(&self) -> u64 {
        self.physical_start
    }

    fn get_page_count(&self) -> u64 {
        self.number_of_pages
    }

    fn is_memory_available(&self) -> bool {
        use crate::common::EfiMemoryType;
        const EFI_ACPI_RECLAIM_MEMORY: u32 = 9; // Memory that holds ACPI tables that can be reclaimed after ACPI initialization
        const EFI_PERSISTENT_MEMORY: u32 = 14; // Memory that persists across reboot, typically NVDIMM-backed

        let mem_type = self.type_;
        matches!(
            mem_type,
            EfiMemoryType::EfiBootServicesData |     // 4
            EfiMemoryType::EfiConventionalMemory // 7
        ) || matches!(
            mem_type as u32,
            EFI_ACPI_RECLAIM_MEMORY | EFI_PERSISTENT_MEMORY
        )
    }
}

pub static HEAP_INITIALIZED: Once<bool> = Once::new();

pub fn init_global_heap(ptr: *mut u8, size: usize) {
    if HEAP_INITIALIZED.get().is_none() {
        unsafe {
            ALLOCATOR.lock().init(ptr, size);
        }
        HEAP_INITIALIZED.call_once(|| true);
    }
}

// Generic PeParser to reduce lines from multiple PE functions
pub struct PeParser {
    pe_base: *const u8,
    pe_offset: usize,
}

impl PeParser {
    pub unsafe fn new(kernel_ptr: *const u8) -> Option<Self> {
        find_pe_base(kernel_ptr).map(|base| {
            let pe_offset = unsafe { read_unaligned!(base, 0x3c, u32) } as usize;
            Self {
                pe_base: base,
                pe_offset,
            }
        })
    }

    pub unsafe fn size_of_image(&self) -> Option<u64> {
        if self.pe_offset == 0
            || self.pe_offset >= PeParser::MAX_PE_HEADER_OFFSET
            || self.pe_base.is_null()
        {
            return None;
        }
        let magic = unsafe { read_unaligned!(self.pe_base, self.pe_offset + 24, u16) };
        if magic != 0x10B && magic != 0x20B {
            return None;
        }
        Some(unsafe { read_unaligned!(self.pe_base, self.pe_offset + 24 + 0x38, u32) } as u64)
    }

    pub unsafe fn sections(&self) -> Option<[PeSection; PeParser::MAX_PE_SECTIONS]> {
        if self.pe_offset == 0
            || self.pe_offset >= PeParser::MAX_PE_HEADER_OFFSET
            || self.pe_base.is_null()
        {
            return None;
        }
        let num_sections =
            unsafe { read_unaligned!(self.pe_base, self.pe_offset + 6, u16) } as usize;
        let optional_header_size =
            unsafe { read_unaligned!(self.pe_base, self.pe_offset + 20, u16) } as usize;
        let section_table_offset = self.pe_offset + 24 + optional_header_size;

        let mut sections = [PeSection {
            name: [0; 8],
            virtual_size: 0,
            virtual_address: 0,
            size_of_raw_data: 0,
            pointer_to_raw_data: 0,
            characteristics: 0,
        }; PeParser::MAX_PE_SECTIONS];
        for i in 0..num_sections.min(PeParser::MAX_PE_SECTIONS) {
            let offset = section_table_offset + i * 40;
            let header = unsafe { read_unaligned!(self.pe_base, offset, PeSectionHeader) };
            sections[i] = PeSection {
                name: header.name,
                virtual_size: header.virtual_size,
                virtual_address: header.virtual_address,
                size_of_raw_data: header.size_of_raw_data,
                pointer_to_raw_data: header.pointer_to_raw_data,
                characteristics: header.characteristics,
            };
        }
        Some(sections)
    }
}

/// EFI Memory Descriptor as defined in UEFI spec
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EfiMemoryDescriptor {
    pub type_: crate::common::EfiMemoryType,
    pub padding: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

impl core::fmt::Debug for EfiMemoryDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EfiMemoryDescriptor")
            .field("type_", &self.type_)
            .field("padding", &self.padding)
            .field("physical_start", &self.physical_start)
            .field("virtual_start", &self.virtual_start)
            .field("number_of_pages", &self.number_of_pages)
            .field("attribute", &self.attribute)
            .finish()
    }
}

/// Named constant for UEFI firmware specific memory type (replace magic number)
const EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC: u32 = 15;

/// Maximum reasonable number of pages in a descriptor (1M pages = 4GB)
const MAX_DESCRIPTOR_PAGES: u64 = 1_048_576;

/// Maximum reasonable system memory limit (512GB)
const MAX_SYSTEM_MEMORY: u64 = 512 * 1024 * 1024 * 1024u64;

/// Boot code physical start address
const BOOT_CODE_START: u64 = 0x100000;

/// Boot code size in pages (0x8000 pages = 128MB)
const BOOT_CODE_PAGES: u64 = 0x8000;

/// Validate an EFI memory descriptor for safety
fn is_valid_memory_descriptor(descriptor: &EfiMemoryDescriptor) -> bool {
    // Check memory type is within valid UEFI range (0x0-0x7FFFFFFF)
    // Allow OEM-specific memory types up to the UEFI maximum
    // But still be conservative about obviously garbage values
    let mem_type = descriptor.type_ as u32;
    if mem_type >= 0x80000000 {
        debug_log_no_alloc!("Invalid memory type (too high): ", mem_type);
        return false;
    }
    debug_log_no_alloc!("Memory type validated: ", mem_type);

    // Check physical start is page-aligned
    if descriptor.physical_start % 4096 != 0 {
        debug_log_no_alloc!(
            "Unaligned physical_start: 0x",
            descriptor.physical_start as usize
        );
        return false;
    }
    debug_log_no_alloc!(
        "Physical start page-aligned: 0x",
        descriptor.physical_start as usize
    );

    // Check number of pages is reasonable
    if descriptor.number_of_pages == 0 || descriptor.number_of_pages > MAX_DESCRIPTOR_PAGES {
        debug_log_no_alloc!("Invalid page count: ", descriptor.number_of_pages as usize);
        return false;
    }
    debug_log_no_alloc!(
        "Page count validated: ",
        descriptor.number_of_pages as usize
    );

    // Check for potential overflow when calculating end address
    let page_size = 4096u64;
    if let Some(end_addr) = descriptor.physical_start.checked_add(
        descriptor
            .number_of_pages
            .checked_mul(page_size)
            .unwrap_or(u64::MAX),
    ) {
        // Ensure end address doesn't exceed reasonable system limits (512GB)
        if end_addr > MAX_SYSTEM_MEMORY {
            debug_log_no_alloc!("Memory region too large: end_addr=0x", end_addr as usize);
            return false;
        }
        debug_log_no_alloc!("End address validated: 0x", end_addr as usize);
    } else {
        debug_log_no_alloc!("Overflow in address calculation");
        return false;
    }

    true
}

/// Constant for UEFI compatibility pages (disabled - first page)
const UEFI_COMPAT_PAGES: u64 = 16383;

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

// Consolidated PE parsing constants into associated constants
impl PeParser {
    const MAX_PE_SEARCH_DISTANCE: usize = 10 * 1024 * 1024;
    const MAX_PE_OFFSET: usize = 16 * 1024 * 1024;
    const MAX_PE_HEADER_OFFSET: usize = 1024 * 1024;
    const MAX_PE_SECTIONS: usize = 16;
}
const KERNEL_MEMORY_PADDING: u64 = 1024 * 1024;
const FALLBACK_KERNEL_SIZE: u64 = 64 * 1024 * 1024;

/// PE section header as defined in PE file format
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PeSectionHeader {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub _pointer_to_relocations: u32,
    pub _pointer_to_linenumbers: u32,
    pub _number_of_relocations: u16,
    pub _number_of_linenumbers: u16,
    pub characteristics: u32,
}

// Helper function to find the PE base address by searching backwards for MZ signature
unsafe fn find_pe_base(start_ptr: *const u8) -> Option<*const u8> {
    log_page_table_op!("PE base", "starting search", start_ptr as usize);

    for i in 0..PeParser::MAX_PE_SEARCH_DISTANCE {
        let candidate_addr = unsafe {
            match (start_ptr as usize).checked_sub(i) {
                Some(addr) => addr as *const u8,
                None => break,
            }
        };

        unsafe {
            if candidate_addr.read() == b'M' && candidate_addr.add(1).read() == b'Z' {
                log_page_table_op!("PE base", "found MZ candidate", candidate_addr as usize);
                let pe_offset = read_unaligned!(candidate_addr, 0x3c, u32) as usize;

                if pe_offset > 0 && pe_offset < PeParser::MAX_PE_OFFSET {
                    let pe_sig = read_unaligned!(candidate_addr, pe_offset, u32);
                    if pe_sig == 0x00004550 {
                        log_page_table_op!("PE base", "found valid PE", candidate_addr as usize);
                        return Some(candidate_addr);
                    }
                }
            }
        }

        // Progress logging
        if i % 100000 == 0 && i != 0 {
            log_page_table_op!("PE base", "progress", i);
        }

        // Early exit check
        if i >= PeParser::MAX_PE_SEARCH_DISTANCE / 4 {
            log_page_table_op!("PE base", "long search warning", i);
        }
    }

    log_page_table_op!("PE base", "search complete - no PE found");
    None
}

// Derive page table flags from PE section characteristics
fn derive_pe_flags(characteristics: u32) -> x86_64::structures::paging::PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    let mut flags = Flags::PRESENT;
    if (characteristics & 0x8000_0000) != 0 {
        // IMAGE_SCN_MEM_WRITE
        flags |= Flags::WRITABLE;
    }
    if (characteristics & 0x2000_0000) == 0 {
        // NOT IMAGE_SCN_MEM_EXECUTE
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
    // Only consider valid descriptors to prevent corrupted data from causing excessive bitmap allocation
    let mut max_addr: u64 = 0;

    for descriptor in memory_map {
        if is_valid_memory_descriptor(descriptor) {
            let end_addr = descriptor
                .physical_start
                .saturating_add(descriptor.number_of_pages.saturating_mul(4096));
            if end_addr > max_addr {
                max_addr = end_addr;
            }
        }
    }

    if max_addr == 0 {
        debug_log_no_alloc!("No valid descriptors found in memory map");
        return (0, 0, 0);
    }

    let capped_max_addr = max_addr.min(32 * 1024 * 1024 * 1024u64);
    let total_frames = (capped_max_addr.div_ceil(4096)) as usize;
    let bitmap_size = (total_frames + 63) / 64;
    (max_addr, total_frames, bitmap_size)
}

// Helper trait for memory region mapping operations
trait MemoryRegionMapper {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_identity(&mut self, phys_start: u64, num_pages: u64, flags: x86_64::structures::paging::PageTableFlags) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_higher_half(&mut self, phys_start: u64, num_pages: u64, flags: x86_64::structures::paging::PageTableFlags) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;
}

// Consolidated MemoryMapper to reduce lines in mapping functions
pub struct MemoryMapper<'a> {
    mapper: &'a mut OffsetPageTable<'static>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
}

impl<'a> MemoryRegionMapper for MemoryMapper<'a> {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
        ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe { map_range_with_log(self.mapper, self.frame_allocator, phys_start, virt_start, num_pages, flags) }
    }

    fn map_to_identity(&mut self, phys_start: u64, num_pages: u64, flags: x86_64::structures::paging::PageTableFlags) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        self.map_region_with_flags(phys_start, phys_start, num_pages, flags)
    }

    fn map_to_higher_half(&mut self, phys_start: u64, num_pages: u64, flags: x86_64::structures::paging::PageTableFlags) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        let virt_start = self.phys_offset.as_u64() + phys_start;
        self.map_region_with_flags(phys_start, virt_start, num_pages, flags)
    }
}

impl<'a> MemoryMapper<'a> {
    pub fn new(
        mapper: &'a mut OffsetPageTable<'static>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
        }
    }

    pub fn map_framebuffer(
        &mut self,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        use x86_64::structures::paging::PageTableFlags as Flags;
        if let (Some(fb_addr), Some(fb_size)) = (fb_addr, fb_size) {
            let fb_pages = fb_size.div_ceil(4096);
            let fb_phys = fb_addr.as_u64();
            unsafe {
                self.map_to_higher_half(
                    fb_phys,
                    fb_pages,
                    Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
                )?;
                self.identity_map_range(
                    fb_phys,
                    fb_pages,
                    Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
                )?;
            }
        }
        Ok(())
    }

    pub fn map_vga(&mut self) {
        use x86_64::structures::paging::PageTableFlags as Flags;
        const VGA_MEMORY_START: u64 = 0xA0000;
        const VGA_MEMORY_END: u64 = 0xC0000;
        const VGA_PAGES: u64 = (VGA_MEMORY_END - VGA_MEMORY_START) / 4096;
        unsafe {
            let _ = self.map_to_higher_half(
                VGA_MEMORY_START,
                VGA_PAGES,
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
            );
        }
    }

    pub fn map_boot_code(&mut self) {
        use x86_64::structures::paging::PageTableFlags as Flags;
        unsafe {
            let _ = self.map_to_higher_half(
                BOOT_CODE_START,
                BOOT_CODE_PAGES,
                Flags::PRESENT | Flags::WRITABLE,
            );
        }
    }

    unsafe fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        let virt_start = self.phys_offset.as_u64() + phys_start;
        map_range_with_log(
            self.mapper,
            self.frame_allocator,
            phys_start,
            virt_start,
            num_pages,
            flags,
        )
    }

    unsafe fn identity_map_range(
        &mut self,
        start_addr: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        map_range_with_log(
            self.mapper,
            self.frame_allocator,
            start_addr,
            start_addr,
            num_pages,
            flags,
        )
    }
}

// Generic function to process memory descriptors using traits
fn process_memory_descriptors<T, F>(descriptors: &[T], mut processor: F)
where
    T: MemoryDescriptorValidator,
    F: FnMut(&T, usize, usize), // (descriptor, start_frame, end_frame)
{
    for descriptor in descriptors {
        if descriptor.is_valid() && descriptor.is_memory_available() {
            let start_frame = (descriptor.get_physical_start() / 4096) as usize;
            let end_frame = start_frame.saturating_add(descriptor.get_page_count() as usize);

            if start_frame < end_frame {
                processor(descriptor, start_frame, end_frame);
            }
        }
    }
}

// Mark available frames as free based on memory map
fn mark_available_frames(
    frame_allocator: &mut BitmapFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
) {
    process_memory_descriptors(memory_map, |descriptor, start_frame, end_frame| {
        let actual_end = end_frame.min(frame_allocator.frame_count);
        frame_allocator.set_frame_range(start_frame, actual_end, false);
    });

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
        bit_clear!(self.bitmap, frame_index);
    }

    /// Set a frame as used in the bitmap
    fn set_frame_used(&mut self, frame_index: usize) {
        bit_set!(self.bitmap, frame_index);
    }

    /// Check if a frame is free
    fn is_frame_free(&self, frame_index: usize) -> bool {
        bit_test!(self.bitmap, frame_index)
    }

    /// Find the next free frame starting from a given index
    fn find_next_free_frame(&self, start_index: usize) -> Option<usize> {
        if !self.initialized {
            return None;
        }

        self.bitmap
            .as_ref()
            .and_then(|bitmap| Self::find_frame_in_bitmap(bitmap, start_index, self.frame_count))
    }

    /// Helper method for bitmap operations
    fn find_frame_in_bitmap(
        bitmap: &[u64],
        start_index: usize,
        frame_count: usize,
    ) -> Option<usize> {
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
        let end_frame = start_frame + count;
        if end_frame > self.frame_count {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        // Check if frames are free before allocating to prevent double-allocation
        for frame_index in start_frame..end_frame {
            if !self.is_frame_free(frame_index) {
                debug_log_no_alloc!(
                    "Frame allocation failed: frame already in use at index ",
                    frame_index
                );
                return Err(crate::common::logging::SystemError::FrameAllocationFailed);
            }
        }

        // Mark frames as used
        self.set_frame_range(start_frame, end_frame, true);

        Ok(())
    }

    /// Set a range of frames as used or free
    fn set_frame_range(&mut self, start_frame: usize, end_frame: usize, used: bool) {
        for i in start_frame..end_frame {
            if used {
                self.set_frame_used(i);
            } else {
                self.set_frame_free(i);
            }
        }
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
    phys_offset: VirtAddr,
) {
    if let Some(sections) =
        unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8).and_then(|p| p.sections()) }
    {
        for section in sections.into_iter().filter(|s| s.virtual_size > 0) {
            unsafe {
                map_pe_section(
                    mapper,
                    section,
                    kernel_phys_start,
                    phys_offset,
                    frame_allocator,
                );
            }
        }
    } else {
        // Fallback: map 64MB region for the kernel if PE parsing fails
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        let flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE;
        unsafe {
            map_identity_range(
                mapper,
                frame_allocator,
                kernel_phys_start.as_u64(),
                kernel_pages,
                flags,
            )
            .expect("Failed to map fallback kernel range");
        }
    }
}

// Unified mapping configuration structure to reduce parameters
#[derive(Clone, Copy)]
struct MappingConfig {
    phys_start: u64,
    virt_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
}

// Generic function to map descriptors with custom configuration
unsafe fn map_memory_descriptors_with_config<F>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
    config_fn: F,
) where
    F: Fn(&EfiMemoryDescriptor) -> Option<MappingConfig>,
{
    for desc in memory_map.iter() {
        if let Some(config) = config_fn(desc) {
            unsafe {
                map_range_with_log(
                    mapper,
                    frame_allocator,
                    config.phys_start,
                    config.virt_start,
                    config.num_pages,
                    config.flags,
                ).ok(); // Ignore errors to continue mapping other regions
            }
        }
    }
}

// Helper functions for region mapping using generic config
unsafe fn map_uefi_runtime_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    unsafe {
        map_memory_descriptors_with_config(
            mapper,
            frame_allocator,
            memory_map,
            move |desc| {
                if is_valid_memory_descriptor(desc)
                    && matches!(
                        desc.type_,
                        crate::common::EfiMemoryType::EfiRuntimeServicesCode
                            | crate::common::EfiMemoryType::EfiRuntimeServicesData
                    ) {
                    let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                        PageTableFlags::PRESENT
                    } else {
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
                    };
                    Some(MappingConfig {
                        phys_start: desc.physical_start,
                        virt_start: desc.physical_start + phys_offset.as_u64(),
                        num_pages: desc.number_of_pages,
                        flags,
                    })
                } else {
                    None
                }
            },
        );
    }
}

unsafe fn map_available_memory_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    process_memory_descriptors(memory_map, |desc, start_frame, end_frame| {
        let phys_start = desc.get_physical_start();
        let virt_start = phys_start + phys_offset.as_u64();
        let pages = (end_frame - start_frame) as u64;
        let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
            PageTableFlags::PRESENT
        } else {
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
        };
        unsafe {
            let _ = map_range_with_log(
                mapper,
                frame_allocator,
                phys_start,
                virt_start,
                pages,
                flags,
            );
        }
    });
}

unsafe fn map_stack_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }

    for desc in memory_map.iter() {
        if is_valid_memory_descriptor(desc) {
            let start = desc.physical_start;
            let end = start + desc.number_of_pages * 4096;
            if rsp >= start && rsp < end {
                unsafe {
                    map_range_with_log(
                        mapper,
                        frame_allocator,
                        desc.physical_start,
                        desc.physical_start + phys_offset.as_u64(),
                        desc.number_of_pages,
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                    )
                    .expect("Failed to map stack region to higher half");
                }
                break;
            }
        }
    }
}

// Helper to adjust return address after page table switch
fn adjust_return_address_and_stack(phys_offset: VirtAddr) {
    // WARNING: This code assumes frame pointers (rbp) are available and enabled, and relies on
    // the standard stack layout where the return address is at [rbp + 8]. This may not hold for
    // all compiler versions or optimization levels, especially in debug builds where
    // force-frame-pointers is not set by default. Violation could lead to stack corruption or crash.
    // This is acknowledged as fragile but necessary for the higher-half kernel transition.
    debug_log_no_alloc!("Adjusting return address and stack for higher half");

    unsafe {
        let mut base_pointer: u64;
        core::arch::asm!("mov {}, rbp", out(reg) base_pointer);

        let return_address_ptr = (base_pointer as *mut u64).add(1);
        let old_return_address = *return_address_ptr;
        *return_address_ptr = old_return_address + phys_offset.as_u64();

        let new_base_pointer = base_pointer + phys_offset.as_u64();
        core::arch::asm!("mov rbp, {}", in(reg) new_base_pointer);

        let old_rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) old_rsp);

        let new_rsp = old_rsp + phys_offset.as_u64();
        core::arch::asm!("mov rsp, {}", in(reg) new_rsp);
    }

    debug_log_no_alloc!("Return address and stack adjusted successfully");
}

// Helper function to setup recursive mapping
unsafe fn setup_recursive_mapping(
    mapper: &mut OffsetPageTable,
    level_4_table_frame: PhysFrame,
) {
    unsafe {
        let table = mapper.level_4_table() as *const PageTable as *mut PageTable;
        (&mut *table.cast::<x86_64::structures::paging::page_table::PageTableEntry>().add(511)).set_addr(
            level_4_table_frame.start_address(),
            x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
        );
    }
}

// Create and initialize a new page table
fn create_new_page_table(
    frame_allocator: &mut BootInfoFrameAllocator,
) -> crate::common::logging::SystemResult<PhysFrame> {
    debug_log_no_alloc!("Allocating new L4 page table frame");

    let level_4_table_frame = match frame_allocator.allocate_frame() {
        Some(frame) => frame,
        None => return Err(crate::common::logging::SystemError::MemOutOfMemory),
    };

    // Zero the new L4 table
    unsafe {
        core::ptr::write_bytes(
            level_4_table_frame.start_address().as_u64() as *mut PageTable,
            0,
            1,
        );
    }

    debug_log_no_alloc!("New L4 page table created and zeroed");
    Ok(level_4_table_frame)
}

// Helper function to map L4 table frame for CR3 switch
unsafe fn map_l4_table_frame_for_switch(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    level_4_table_frame: PhysFrame,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;
    unsafe { map_identity_range(
        mapper,
        frame_allocator,
        level_4_table_frame.start_address().as_u64(),
        1,
        Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
    ).expect("Failed to map L4 table frame"); }
}

// Helper function to map UEFI compatibility range
unsafe fn map_uefi_compatibility_range(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;
    map_identity_range(
        mapper,
        frame_allocator,
        4096,
        UEFI_COMPAT_PAGES,
        Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
    ).expect("Failed to map identity range for UEFI compatibility");
}

// Helper function to map kernel for CR3 switch
unsafe fn map_kernel_for_switch(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    kernel_phys_start: PhysAddr,
) -> u64 {
    let kernel_size = unsafe { calculate_kernel_memory_size(kernel_phys_start) };
    let kernel_pages = kernel_size.div_ceil(4096);
    use x86_64::structures::paging::PageTableFlags as Flags;
    map_identity_range(
        mapper,
        frame_allocator,
        kernel_phys_start.as_u64(),
        kernel_pages,
        Flags::PRESENT | Flags::WRITABLE,
    ).expect("Failed to identity map kernel for CR3 switch");
    kernel_size
}

// Helper function to map boot code range
unsafe fn map_boot_code_range(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;
    map_identity_range(
        mapper,
        frame_allocator,
        0x190000, // Map from approximately where boot code starts
        0x10000,  // Map generous 64MB range for boot code
        Flags::PRESENT | Flags::WRITABLE,
    ).expect("Failed to identity map boot code");
}

// Generic function to map UEFI runtime regions
fn map_uefi_runtime_regions<M: MemoryRegionMapper>(
    mapper: &mut M,
    memory_map: &[EfiMemoryDescriptor],
) {
    use x86_64::structures::paging::PageTableFlags as Flags;
    for desc in memory_map.iter() {
        if is_valid_memory_descriptor(desc)
            && matches!(
                desc.type_,
                crate::common::EfiMemoryType::EfiRuntimeServicesCode
                    | crate::common::EfiMemoryType::EfiRuntimeServicesData
            )
        {
            let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                Flags::PRESENT
            } else {
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
            };
            let _ = mapper.map_to_identity(desc.physical_start, desc.number_of_pages, flags);
        }
    }
}

// Generic function to map available memory regions using descriptor validator
fn map_available_memory_regions<M: MemoryRegionMapper>(
    mapper: &mut M,
    memory_map: &[EfiMemoryDescriptor],
) {
    use x86_64::structures::paging::PageTableFlags as Flags;
    process_memory_descriptors(memory_map, |desc, start_frame, end_frame| {
        let phys_start = desc.get_physical_start();
        let pages = (end_frame - start_frame) as u64;
        let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
            Flags::PRESENT
        } else {
            Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
        };
        let _ = mapper.map_to_identity(phys_start, pages, flags);
    });
}

// Helper function to map stack region
unsafe fn map_stack_region(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
) {
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp) };
    let stack_pages = 256; // 1MB stack
    let stack_start = rsp & !4095; // page align
    use x86_64::structures::paging::PageTableFlags as Flags;
    map_identity_range(
        mapper,
        frame_allocator,
        stack_start,
        stack_pages,
        Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
    ).expect("Failed to map current stack region");

    for desc in memory_map.iter() {
        if is_valid_memory_descriptor(desc) {
            let start = desc.physical_start;
            let end = start + desc.number_of_pages * 4096;
            if rsp >= start && rsp < end && desc.number_of_pages <= MAX_DESCRIPTOR_PAGES {
                map_identity_range(
                    mapper,
                    frame_allocator,
                    desc.physical_start,
                    desc.number_of_pages,
                    Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
                ).expect("Failed to map stack region");
                break;
            }
        }
    }
}

// Helper function to map bitmap region
unsafe fn map_bitmap_region(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let bitmap_ptr = &raw const BITMAP_STATIC as *const u8;
    let bitmap_start = bitmap_ptr as usize as u64 & !(4095);
    let bitmap_end = bitmap_ptr as usize as u64 + core::mem::size_of::<[u64; 131072]>() as u64;
    let bitmap_end_page = ((bitmap_end - 1) & !(4095)) + 4096;
    let pages = ((bitmap_end_page - bitmap_start) / 4096) as u64;
    use x86_64::structures::paging::PageTableFlags as Flags;
    map_identity_range(
        mapper,
        frame_allocator,
        bitmap_start,
        pages,
        Flags::PRESENT | Flags::WRITABLE,
    ).expect("Failed to map bitmap region");
}

// Setup identity mappings needed for CR3 switch
fn setup_identity_mappings(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    kernel_phys_start: PhysAddr,
    level_4_table_frame: PhysFrame,
    memory_map: &[EfiMemoryDescriptor],
) -> u64 {
    debug_log_no_alloc!("Setting up identity mappings for CR3 switch");

    // Map essential regions for CR3 switch
    let kernel_size;
    unsafe {
        map_l4_table_frame_for_switch(mapper, frame_allocator, level_4_table_frame);
        map_uefi_compatibility_range(mapper, frame_allocator);
        kernel_size = map_kernel_for_switch(mapper, frame_allocator, kernel_phys_start);
        map_boot_code_range(mapper, frame_allocator);
        map_stack_region(mapper, frame_allocator, memory_map);
        map_bitmap_region(mapper, frame_allocator);
    }

    // Helper function to map page-aligned descriptors safely
    unsafe fn map_page_aligned_descriptors_safely(
        mapper: &mut OffsetPageTable,
        frame_allocator: &mut BootInfoFrameAllocator,
        memory_map: &[EfiMemoryDescriptor],
    ) {
        use x86_64::structures::paging::PageTableFlags as Flags;
        for desc in memory_map.iter() {
            if desc.physical_start % 4096 != 0 {
                continue; // skip unaligned
            }
            let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                Flags::PRESENT
            } else {
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
            };
            unsafe {
                let _ = map_identity_range(mapper, frame_allocator, desc.physical_start, 1, flags);
            }
        }
    }

    // Map page-aligned descriptors safely
    unsafe { map_page_aligned_descriptors_safely(mapper, frame_allocator, memory_map); }

    debug_log_no_alloc!("Identity mappings completed");
    kernel_size
}

// Setup higher-half mappings
fn setup_higher_half_mappings<'a>(
    mapper: &'a mut OffsetPageTable<'static>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    debug_log_no_alloc!("Setting up higher-half mappings");

    // Map kernel segments to higher half
    unsafe {
        map_kernel_segments_inner(mapper, frame_allocator, kernel_phys_start, phys_offset);
    }

    debug_log_no_alloc!("Kernel segments mapped to higher half");

    // Map additional regions using MemoryMapper
    let mut memory_mapper = MemoryMapper::new(mapper, frame_allocator, phys_offset);
    memory_mapper.map_framebuffer(fb_addr, fb_size);
    memory_mapper.map_vga();
    memory_mapper.map_boot_code();

    // Map UEFI runtime services regions to higher half
    unsafe { map_uefi_runtime_to_higher_half(mapper, frame_allocator, phys_offset, memory_map); }

    // Map all available memory regions to higher half for complete UEFI compatibility
    unsafe { map_available_memory_to_higher_half(mapper, frame_allocator, phys_offset, memory_map); }

    // Map current stack region to higher half
    unsafe { map_stack_to_higher_half(mapper, frame_allocator, phys_offset, memory_map); }

    debug_log_no_alloc!("Additional regions mapped to higher half");
}

// Perform the page table switch
// Function to assist with page table debugging
fn debug_page_table_info(level_4_table_frame: PhysFrame, phys_offset: VirtAddr) {
    debug_log_no_alloc!("New L4 table phys addr: ", level_4_table_frame.start_address().as_u64() as usize);
    debug_log_no_alloc!("Phys offset: ", phys_offset.as_u64() as usize);
}

fn switch_to_new_page_table(
    level_4_table_frame: PhysFrame,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
    current_physical_memory_offset: VirtAddr,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    debug_page_table_info(level_4_table_frame, phys_offset);

    // Use the current active page table to map the L4 table to higher half
    let mut current_mapper = unsafe {
        let l4_table = crate::page_table::active_level_4_table(current_physical_memory_offset);
        OffsetPageTable::new(l4_table, current_physical_memory_offset)
    };

    unsafe {
        match current_mapper.map_to(
            Page::containing_address(phys_offset + level_4_table_frame.start_address().as_u64()),
            level_4_table_frame,
            Flags::PRESENT | Flags::WRITABLE,
            frame_allocator,
        ) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                debug_log_no_alloc!("L4 table already mapped to higher half");
            }
            Err(e) => {
                debug_log_no_alloc!("Failed to map L4 table to higher half");
                // Continue anyway, may already be mapped
            }
        }
    }

    debug_log_no_alloc!("About to switch CR3 to new page table");

    // Switch to new page table
    unsafe {
        Cr3::write(
            level_4_table_frame,
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }

    debug_log_no_alloc!("CR3 switched, flushing TLB");

    // Flush TLB
    flush_tlb_and_verify!();

    debug_log_no_alloc!("TLB flushed, page table switch complete");
}

pub fn reinit_page_table_with_allocator(
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[EfiMemoryDescriptor],
    current_physical_memory_offset: VirtAddr,
) -> VirtAddr {
    debug_log_no_alloc!("Page table reinitialization starting");

    let phys_offset = HIGHER_HALF_OFFSET;

    // Create new page table
    let level_4_table_frame =
        create_new_page_table(frame_allocator).expect("Failed to create new page table");

    // Create a temporary mapper for current page table to identity map the new L4 table
    let mut current_mapper = unsafe { init(current_physical_memory_offset) };

    // Identity map the new L4 table frame so we can access it
    use x86_64::structures::paging::PageTableFlags as Flags;
    unsafe {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(level_4_table_frame.start_address().as_u64()));
        let frame = level_4_table_frame;
        let _ = current_mapper.map_to(page, frame, Flags::PRESENT | Flags::WRITABLE, frame_allocator);
    }

    // Create mapper for new page table - use current phys_offset to access the new table
    let mut mapper = unsafe {
        let table_addr = current_physical_memory_offset.as_u64() + level_4_table_frame.start_address().as_u64();
        OffsetPageTable::new(
            &mut *(table_addr as *mut PageTable),
            VirtAddr::new(0),
        )
    };

    // Setup identity mappings
    let kernel_size = setup_identity_mappings(
        &mut mapper,
        frame_allocator,
        kernel_phys_start,
        level_4_table_frame,
        memory_map,
    );

    // Setup higher-half mappings
    setup_higher_half_mappings(
        &mut mapper,
        frame_allocator,
        kernel_phys_start,
        fb_addr,
        fb_size,
        phys_offset,
        memory_map,
    );

    // Set up recursive mapping for page table management
    unsafe { setup_recursive_mapping(&mut mapper, level_4_table_frame) };

    // Perform the page table switch
    switch_to_new_page_table(level_4_table_frame, phys_offset, frame_allocator, current_physical_memory_offset);

    // Adjust return address
    adjust_return_address_and_stack(phys_offset);

    debug_log_no_alloc!("Page table reinitialization completed");
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

// Generic function to map a range with given flags (consolidated from MemoryMapper)
unsafe fn map_range_with_log(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_start: u64,
    virt_start: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    log_page_table_op!("Mapping range", phys_start, virt_start, num_pages);
    for i in 0..num_pages {
        let phys_addr = phys_start + i * 4096;
        let virt_addr = virt_start + i * 4096;
        let (page, frame) = create_page_and_frame!(virt_addr, phys_addr);
        match mapper.map_to(page, frame, flags, frame_allocator) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// Helper to identity map a memory range
unsafe fn identity_map_range_with_log(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    start_addr: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    map_range_with_log(
        mapper,
        frame_allocator,
        start_addr,
        start_addr,
        num_pages,
        flags,
    )
}

// Helper to map to higher half
unsafe fn map_to_higher_half_with_log(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    phys_start: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let virt_start = phys_offset.as_u64() + phys_start;
    map_range_with_log(
        mapper,
        frame_allocator,
        phys_start,
        virt_start,
        num_pages,
        flags,
    )
}

// Global heap allocator
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

// PageTableHelper trait with methods required by other subcrates
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
    ) -> crate::common::logging::SystemResult<PhysFrame>;
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

pub type ProcessPageTable = PageTableManager;

pub struct PageTableManager {
    current_page_table: usize,
    initialized: bool,
    pml4_frame: Option<PhysFrame>,
    mapper: Option<OffsetPageTable<'static>>,
}

impl PageTableManager {
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            initialized: false,
            pml4_frame: None,
            mapper: None,
        }
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

/// A dummy frame allocator for when we need to allocate pages for page tables
pub struct DummyFrameAllocator {}

impl DummyFrameAllocator {
    pub fn new() -> Self {
        Self {}
    }
}

unsafe impl FrameAllocator<Size4KiB> for DummyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None // For now, we don't support allocating new frames for page tables
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

    if kernel_phys_start.as_u64() == 0 {
        debug_log_no_alloc!("Kernel phys start is 0, using fallback size");
        return FALLBACK_KERNEL_SIZE;
    }

    let parser = match unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
        Some(p) => {
            debug_log_no_alloc!("PE parser created successfully");
            p
        }
        None => {
            debug_log_no_alloc!("Failed to create PE parser, using fallback");
            return FALLBACK_KERNEL_SIZE;
        }
    };

    match unsafe { parser.size_of_image() } {
        Some(size) => {
            let padded_size = (size + KERNEL_MEMORY_PADDING).div_ceil(4096) * 4096;
            debug_log_no_alloc!("PE parsing successful, size=0x", padded_size as usize);
            padded_size
        }
        None => {
            debug_log_no_alloc!("PE parsing failed for size_of_image, using fallback size");
            FALLBACK_KERNEL_SIZE
        }
    }
}
