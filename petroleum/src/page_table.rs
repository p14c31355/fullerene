// Internal submodules for modularization
pub mod bitmap_allocator;
pub mod constants;
pub mod efi_memory;

pub use bitmap_allocator::BitmapFrameAllocator;



// Import for heap range setting
use crate::common::memory::set_heap_range;
// BTreeMap will be available through std when compiled as std crate
use spin::Once;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, Translate,
    },
};

// Import constants
use constants::{
    BOOT_CODE_PAGES, BOOT_CODE_START, READ_WRITE, READ_WRITE_NO_EXEC, TEMP_LOW_VA, VGA_MEMORY_END,
    VGA_MEMORY_START,
};

pub static HEAP_INITIALIZED: Once<bool> = Once::new();

pub fn init_global_heap(ptr: *mut u8, size: usize) {
    if HEAP_INITIALIZED.get().is_none() {
        unsafe {
            ALLOCATOR.lock().init(ptr, size);
        }
        set_heap_range(ptr as usize, size);
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
    unsafe { find_pe_base(kernel_ptr) }.map(|base| {
        let pe_offset = read_unaligned!(base, 0x3c, u32) as usize;
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
        let magic = read_unaligned!(self.pe_base, self.pe_offset + 24, u16);
        if magic != 0x10B && magic != 0x20B {
            return None;
        }
        Some(read_unaligned!(self.pe_base, self.pe_offset + 24 + 0x38, u32) as u64)
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

// Re-export EfiMemoryDescriptor and MemoryDescriptorValidator from efi_memory module
pub use efi_memory::{EfiMemoryDescriptor, MemoryDescriptorValidator};

/// Named constant for UEFI firmware specific memory type (replace magic number)
const EFI_MEMORY_TYPE_FIRMWARE_SPECIFIC: u32 = 15;

/// Maximum reasonable number of pages in a descriptor (1M pages = 4GB)
const MAX_DESCRIPTOR_PAGES: u64 = 1_048_576;

/// Maximum reasonable system memory limit (512GB)
const MAX_SYSTEM_MEMORY: u64 = 512 * 1024 * 1024 * 1024u64;

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



//// Generic mapping interface
pub trait MemoryMappable {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;
}

// Consolidated MemoryMapper to reduce lines in mapping functions
pub struct MemoryMapper<'a> {
    mapper: &'a mut OffsetPageTable<'static>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
}

// Generic mapping interface
impl<'a> MemoryMappable for MemoryMapper<'a> {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                virt_start,
                num_pages,
                flags
            )
        }
    }

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        self.map_region_with_flags(phys_start, phys_start, num_pages, flags)
    }

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
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

    // Combine framebuffer mapping into a single helper
    pub fn map_framebuffer(
        &mut self,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        if let (Some(fb_addr), Some(fb_size)) = (fb_addr, fb_size) {
            let fb_pages = fb_size.div_ceil(4096);
            let fb_phys = fb_addr.as_u64();
            let flags = READ_WRITE_NO_EXEC;
            self.map_region_dual(fb_phys, fb_pages, flags)?;
        }
        Ok(())
    }

    pub fn map_vga(&mut self) {
        const VGA_PAGES: u64 = (VGA_MEMORY_END - VGA_MEMORY_START) / 4096;
        let flags = READ_WRITE_NO_EXEC;
        unsafe {
            let _ = self.map_to_higher_half(VGA_MEMORY_START, VGA_PAGES, flags);
        }
    }

    pub fn map_boot_code(&mut self) {
        let flags = READ_WRITE;
        unsafe {
            let _ = self.map_to_higher_half(BOOT_CODE_START, BOOT_CODE_PAGES, flags);
        }
    }

    // Helper to map both identity and higher-half for regions like framebuffer
    fn map_region_dual(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            self.map_to_higher_half(phys_start, num_pages, flags)?;
            self.identity_map_range(phys_start, num_pages, flags)?;
        }
        Ok(())
    }

    unsafe fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        let virt_start = self.phys_offset.as_u64() + phys_start;
        map_range_with_log_macro!(
            self.mapper,
            self.frame_allocator,
            phys_start,
            virt_start,
            num_pages,
            flags
        );
        Ok(())
    }

    unsafe fn identity_map_range(
        &mut self,
        start_addr: u64,
        num_pages: u64,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        identity_map_range_with_log_macro!(
            self.mapper,
            self.frame_allocator,
            start_addr,
            num_pages,
            flags
        )
    }
}

// Import process_memory_descriptors from efi_memory submodule
pub use efi_memory::process_memory_descriptors;

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
    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

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

/// Temporary virtual address for page table destruction operations.
/// WARNING: Assumes this address range is not already mapped or in use.
/// A dedicated temporary VA allocation pool would be safer but is not implemented here.
const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);

/// Temporary virtual address for page table cloning operations.
/// WARNING: Assumes this address range is not already mapped or in use.
/// A dedicated temporary VA allocation pool would be safer but is not implemented here.
/// This is distinct from TEMP_VA_FOR_DESTROY to avoid conflicts during recursive operations.
const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);

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

// Helper struct for kernel mapping operations
struct KernelMapper<'a, 'b> {
    mapper: &'a mut OffsetPageTable<'b>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
}

impl<'a, 'b> KernelMapper<'a, 'b> {
    fn new(
        mapper: &'a mut OffsetPageTable<'b>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
        }
    }

    unsafe fn map_pe_sections(&mut self, kernel_phys_start: PhysAddr) -> bool {
        if let Some(sections) = unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) }
            .and_then(|p| unsafe { p.sections() })
        {
            for section in sections.into_iter().filter(|s| s.virtual_size > 0) {
                unsafe {
                    self.map_single_pe_section(section, kernel_phys_start);
                }
            }
            true
        } else {
            false
        }
    }

    unsafe fn map_fallback_kernel_region(&mut self, kernel_phys_start: PhysAddr) {
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        let flags = READ_WRITE;
        unsafe {
            map_identity_range(
                self.mapper,
                self.frame_allocator,
                kernel_phys_start.as_u64(),
                kernel_pages,
                flags,
            )
            .expect("Failed to map fallback kernel range");
        }
    }

    unsafe fn map_single_pe_section(&mut self, section: PeSection, kernel_phys_start: PhysAddr) {
        map_pe_section(
            self.mapper,
            section,
            kernel_phys_start,
            self.phys_offset,
            self.frame_allocator,
        );
    }
}

// Unified mapping configuration structure to reduce parameters and lines
#[derive(Clone, Copy)]
struct MappingConfig {
    phys_start: u64,
    virt_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
}

// Macro to create mapping configurations for common patterns
macro_rules! create_mapping_config {
    ($phys_start:expr, $virt_start:expr, $num_pages:expr, $flags:expr) => {
        MappingConfig {
            phys_start: $phys_start,
            virt_start: $virt_start,
            num_pages: $num_pages,
            flags: $flags,
        }
    };
}

macro_rules! higher_half_config {
    ($phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {
        MappingConfig {
            phys_start: $phys_start,
            virt_start: $phys_offset.as_u64() + $phys_start,
            num_pages: $num_pages,
            flags: $flags,
        }
    };
}

// Generic function to map descriptors with custom configuration using MappingConfig macros
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
                map_range_with_log_macro!(
                    mapper,
                    frame_allocator,
                    config.phys_start,
                    config.virt_start,
                    config.num_pages,
                    config.flags
                );
            }
        }
    }
}

// Unified function to map UEFI runtime to higher half using macros
unsafe fn map_uefi_runtime_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    unsafe {
        map_memory_descriptors_with_config(mapper, frame_allocator, memory_map, move |desc| {
            if desc.is_valid()
                && matches!(
                    desc.type_,
                    crate::common::EfiMemoryType::EfiRuntimeServicesCode
                        | crate::common::EfiMemoryType::EfiRuntimeServicesData
                )
            {
                let mut flags = PageTableFlags::PRESENT;
                if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesData {
                    flags |= PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
                }
                Some(higher_half_config!(
                    phys_offset,
                    desc.physical_start,
                    desc.number_of_pages,
                    flags
                ))
            } else {
                None
            }
        });
    }
}

// Consolidated mapping for available memory to higher half with reduced duplication
unsafe fn map_available_memory_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) {
    process_memory_descriptors(memory_map, |desc, start_frame, end_frame| {
        let phys_start = desc.get_physical_start();
        let pages = (end_frame - start_frame) as u64;
        // Always give writable access to available memory regions for compatibility, but executable code regions should not be writable
        let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
            PageTableFlags::PRESENT // Executable, read-only
        } else {
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE // Writable, not executable
        };
        unsafe {
            let _ = map_to_higher_half_with_log(
                mapper,
                frame_allocator,
                phys_offset,
                phys_start,
                pages,
                flags,
            );
        }
    });
}

// Simplified stack mapping using rsp detection macro
macro_rules! get_current_stack_pointer {
    () => {{
        let rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }
        rsp
    }};
}

unsafe fn map_stack_to_higher_half(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[EfiMemoryDescriptor],
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let rsp = get_current_stack_pointer!();

    for desc in memory_map.iter() {
        if desc.is_valid() {
            let start = desc.physical_start;
            let end = start + desc.number_of_pages * 4096;
            if rsp >= start && rsp < end {
                map_to_higher_half_with_log(
                    mapper,
                    frame_allocator,
                    phys_offset,
                    desc.physical_start,
                    desc.number_of_pages,
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::NO_EXECUTE,
                )?;
                break;
            }
        }
    }
    Ok(())
}



// Generic page table utilities to reduce duplication between different mappers
trait PageTableUtils {
    fn map_multiple_ranges<F>(
        &mut self,
        frame_allocator: &mut BootInfoFrameAllocator,
        ranges: &[MappingConfig],
        log_fn: F,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>
    where
        F: Fn(&MappingConfig);
}

impl<T: MemoryMappable + ?Sized> PageTableUtils for T {
    fn map_multiple_ranges<F>(
        &mut self,
        frame_allocator: &mut BootInfoFrameAllocator,
        ranges: &[MappingConfig],
        log_fn: F,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>
    where
        F: Fn(&MappingConfig),
    {
        for config in ranges {
            log_fn(config);
            self.map_region_with_flags(
                config.phys_start,
                config.virt_start,
                config.num_pages,
                config.flags,
            )?;
        }
        Ok(())
    }
}

// Helper to adjust return address after page table switch
fn adjust_return_address_and_stack(phys_offset: VirtAddr) {
    // WARNING: This code assumes frame pointers (rbp) are available and enabled, and relies on
    // the standard stack layout where the return address is at [rbp + 8]. This may not hold for
    // all compiler versions or optimization levels, especially in debug builds where
    // force-frame-pointers is not set by default. Violation could lead to stack corruption or crash.
    // This is acknowledged as fragile but is necessary for the higher-half kernel transition.
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
unsafe fn setup_recursive_mapping(mapper: &mut OffsetPageTable, level_4_table_frame: PhysFrame) {
    unsafe {
        let table = mapper.level_4_table() as *const PageTable as *mut PageTable;
        (&mut *table
            .cast::<x86_64::structures::paging::page_table::PageTableEntry>()
            .add(511))
            .set_addr(
                level_4_table_frame.start_address(),
                page_flags_const!(READ_WRITE),
            );
    }
}

// Create and initialize a new page table
fn create_new_page_table(
    frame_allocator: &mut BootInfoFrameAllocator,
) -> crate::common::logging::SystemResult<PhysFrame> {
    debug_log_no_alloc!("Allocating new L4 page table frame");

    let level_4_table_frame: PhysFrame = match frame_allocator.allocate_frame() {
        Some(frame) => frame,
        None => return Err(crate::common::logging::SystemError::MemOutOfMemory),
    };

    // Temporarily create an identity mapper for this context to zero the allocated frame
    let mut temp_mapper = unsafe { init(VirtAddr::new(0)) };
    let temp_page = unsafe { Page::<Size4KiB>::containing_address(TEMP_LOW_VA) };
    unsafe {
        temp_mapper
            .map_to(
                temp_page,
                level_4_table_frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                frame_allocator,
            )
            .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
            .flush();
    }

    // Zero the new L4 table through the temporary mapping
    unsafe {
        let table_addr = TEMP_LOW_VA.as_mut_ptr() as *mut PageTable;
        *table_addr = PageTable::new();
    }

    // Unmap the temporary page
    if let Ok((_frame, flush)) = temp_mapper.unmap(temp_page) {
        flush.flush();
    }

    debug_log_no_alloc!("New L4 page table created and zeroed");
    Ok(level_4_table_frame)
}

// Consolidated identity mapping functions using macro for uniformity
// These were replaced with inline macro calls to reduce function count

// Unified helper function for stack mapping using RSP detection macro
macro_rules! map_current_stack {
    ($mapper:expr, $frame_allocator:expr, $memory_map:expr, $flags:expr) => {{
        let rsp = get_current_stack_pointer!();
        let stack_pages = 256; // 1MB stack
        let stack_start = rsp & !4095; // page align

        // Map current stack area
        unsafe {
            map_identity_range($mapper, $frame_allocator, stack_start, stack_pages, $flags)
        }
        .expect("Failed to map current stack region");

        // Map actual stack descriptor if found
        for desc in $memory_map.iter() {
            if desc.is_valid() {
                let start = desc.physical_start;
                let end = start + desc.number_of_pages * 4096;
                if rsp >= start && rsp < end && desc.number_of_pages <= MAX_DESCRIPTOR_PAGES {
                    unsafe {
                        map_identity_range(
                            $mapper,
                            $frame_allocator,
                            desc.physical_start,
                            desc.number_of_pages,
                            $flags,
                        )
                    }
                    .expect("Failed to map stack region");
                    break;
                }
            }
        }
    }};
}

struct PageTableReinitializer {
    phys_offset: VirtAddr,
}

impl PageTableReinitializer {
    fn new() -> Self {
        Self {
            phys_offset: HIGHER_HALF_OFFSET,
        }
    }

    fn reinitialize(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
        frame_allocator: &mut BootInfoFrameAllocator,
        memory_map: &[EfiMemoryDescriptor],
        current_physical_memory_offset: VirtAddr,
    ) -> VirtAddr {
        debug_log_no_alloc!("Page table reinitialization starting");

        let level_4_table_frame =
            self.create_page_table(frame_allocator, current_physical_memory_offset);
        let mut mapper = self.setup_new_mapper(
            level_4_table_frame,
            current_physical_memory_offset,
            frame_allocator,
        );
        let mut initializer =
            PageTableInitializer::new(&mut mapper, frame_allocator, self.phys_offset, memory_map);
        let _kernel_size =
            unsafe { initializer.setup_identity_mappings(kernel_phys_start, level_4_table_frame) };
        initializer.setup_higher_half_mappings(kernel_phys_start, fb_addr, fb_size);
        self.setup_recursive_mapping(&mut mapper, level_4_table_frame);
        self.perform_page_table_switch(
            level_4_table_frame,
            frame_allocator,
            current_physical_memory_offset,
        );
        adjust_return_address_and_stack(self.phys_offset);
        self.phys_offset
    }

    fn create_page_table(
        &self,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
    ) -> PhysFrame {
        debug_log_no_alloc!("Allocating new L4 page table frame");
        let level_4_table_frame = match frame_allocator.allocate_frame() {
            Some(frame) => frame,
            None => panic!("Failed to allocate L4 page table frame"),
        };

        // Zero the new L4 table directly through identity mapping (UEFI identity maps all memory)
        unsafe {
            let table_phys = level_4_table_frame.start_address().as_u64();
            let table_virt = current_physical_memory_offset + table_phys;
            let table_ptr = table_virt.as_mut_ptr() as *mut PageTable;
            *table_ptr = PageTable::new();
        }

        debug_log_no_alloc!("New L4 page table created and zeroed");
        level_4_table_frame
    }

    fn setup_new_mapper(
        &self,
        level_4_table_frame: PhysFrame,
        current_physical_memory_offset: VirtAddr,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> OffsetPageTable<'static> {
        debug_log_no_alloc!("Setting up new page table mapper");

        // Since all virtual addresses seem to be obstructed by huge pages in UEFI,
        // we need to create the OffsetPageTable after the page table switch.
        // For now, return a minimal placeholder that can be used to build the mappings
        // using the fact that we can access physical memory through existing mappings.

        // We'll map the L4 table at its own physical address as a last resort
        let temp_phys_addr = level_4_table_frame.start_address().as_u64();
        let temp_virt_addr = current_physical_memory_offset + temp_phys_addr;
        let temp_page = Page::<Size4KiB>::containing_address(temp_virt_addr);

        debug_log_no_alloc!(
            "Using existing phys offset mapping at: 0x",
            temp_virt_addr.as_u64() as usize
        );

        // Check if this page is already mapped (it should be in UEFI)
        if temp_virt_addr.as_u64() < 0x800000000000 {
            // Reasonable sanity check for low memory
            unsafe {
                return OffsetPageTable::new(
                    &mut *(temp_virt_addr.as_mut_ptr() as *mut PageTable),
                    current_physical_memory_offset,
                );
            }
        }

        // If even that fails, we have a fundamental problem with UEFI memory layout
        panic!(
            "Cannot create any mapping for L4 table frame - UEFI huge page coverage is complete"
        );
    }

    fn setup_recursive_mapping(
        &self,
        mapper: &mut OffsetPageTable,
        level_4_table_frame: PhysFrame,
    ) {
        unsafe {
            let table = mapper.level_4_table() as *const PageTable as *mut PageTable;
            (&mut *table
                .cast::<x86_64::structures::paging::page_table::PageTableEntry>()
                .add(511))
                .set_addr(
                    level_4_table_frame.start_address(),
                    page_flags_const!(READ_WRITE),
                );
        }
    }

    fn perform_page_table_switch(
        &self,
        level_4_table_frame: PhysFrame,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
    ) {
        debug_log_no_alloc!("Page table switch: setting recursive in new table");

        // Access the new L4 table at its physical address (since UEFI identity maps)
        let new_l4_phys = level_4_table_frame.start_address().as_u64();
        let new_l4_virt = if current_physical_memory_offset.as_u64() == 0 {
            new_l4_phys // identity mapped in UEFI
        } else {
            current_physical_memory_offset.as_u64() + new_l4_phys
        };

        unsafe {
            let new_l4_table = &mut *(new_l4_virt as *mut PageTable);
            new_l4_table[511].set_addr(
                level_4_table_frame.start_address(),
                page_flags_const!(READ_WRITE),
            );
        }

        debug_log_no_alloc!("Switching CR3 to new table");

        // Switch to new page table
        unsafe {
            Cr3::write(
                level_4_table_frame,
                x86_64::registers::control::Cr3Flags::empty(),
            );
        }

        flush_tlb_and_verify!();

        debug_log_no_alloc!("CR3 switched, now mapping L4 to higher half");

        // Now map the L4 to higher half using the new page table with recursive mapping
        let mut mapper = unsafe { init(current_physical_memory_offset) };

        unsafe {
            map_to_higher_half_with_log(
                &mut mapper,
                frame_allocator,
                self.phys_offset,
                new_l4_phys,
                1,
                page_flags_const!(READ_WRITE_NO_EXEC),
            )
            .expect("Failed to map L4 to higher half");
        }

        debug_page_table_info(level_4_table_frame, self.phys_offset);

        debug_log_no_alloc!("Page table switch complete");
    }
}

// Consolidated page table initializer to reduce lines and improve organization
struct PageTableInitializer<'a> {
    mapper: &'a mut OffsetPageTable<'static>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &'a [EfiMemoryDescriptor],
}

impl<'a> PageTableInitializer<'a> {
    fn new(
        mapper: &'a mut OffsetPageTable<'static>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
        memory_map: &'a [EfiMemoryDescriptor],
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
            memory_map,
        }
    }

    // Setup identity mappings needed for CR3 switch using helper functions
    fn setup_identity_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        level_4_table_frame: PhysFrame,
    ) -> u64 {
        debug_log_no_alloc!("Setting up identity mappings for CR3 switch");

        // Use helper functions for essential mappings
        let kernel_size = self.map_essential_regions(kernel_phys_start, level_4_table_frame);

        // Map current stack area
        self.map_current_stack_identity();

        // Helper for memory descriptor mappings
        unsafe { self.map_available_memory_identity(); }

        debug_log_no_alloc!("Identity mappings completed");
        kernel_size
    }

    // Helper to map essential fixed regions and kernel
    fn map_essential_regions(&mut self, kernel_phys_start: PhysAddr, level_4_table_frame: PhysFrame) -> u64 {
        unsafe {
            // Bitmap area - must be first
            let bitmap_start = (&raw const bitmap_allocator::BITMAP_STATIC) as *const _ as usize as u64;
            let bitmap_pages = ((131072 * 8) + 4095) / 4096;
            self.map_identity_config(bitmap_start, bitmap_pages, READ_WRITE_NO_EXEC);

            // L4 table, UEFI compat, kernel
            self.map_identity_config(level_4_table_frame.start_address().as_u64(), 1, READ_WRITE_NO_EXEC);
            self.map_identity_config(4096, UEFI_COMPAT_PAGES, READ_WRITE_NO_EXEC);

            let kernel_size = calculate_kernel_memory_size(kernel_phys_start);
            let kernel_pages = kernel_size.div_ceil(4096);
            self.map_identity_config(kernel_phys_start.as_u64(), kernel_pages, READ_WRITE);

            // Boot code
            self.map_identity_config(BOOT_CODE_START, BOOT_CODE_PAGES, READ_WRITE);

            kernel_size
        }
    }

    // Consolidated identity mapping helper using unified stack macro
    unsafe fn map_identity_config(&mut self, phys_start: u64, num_pages: u64, flags: PageTableFlags) {
        identity_map_range_with_log_macro!(self.mapper, self.frame_allocator, phys_start, num_pages, flags);
    }

    // Streamlined stack region mapping with macro integration
    fn map_current_stack_identity(&mut self) {
        map_current_stack!(
            self.mapper,
            self.frame_allocator,
            self.memory_map,
            READ_WRITE_NO_EXEC
        );
    }

    // Setup higher-half mappings using helper methods
    fn setup_higher_half_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
    ) {
        debug_log_no_alloc!("Setting up higher-half mappings");

        self.map_kernel_segments(kernel_phys_start);
        self.map_additional_regions(fb_addr, fb_size);
        self.map_special_regions();

        debug_log_no_alloc!("Higher-half mappings completed");
    }

    // Helper to map kernel segments with fallback
    fn map_kernel_segments(&mut self, kernel_phys_start: PhysAddr) {
        let mut kernel_mapper = KernelMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        if !unsafe { kernel_mapper.map_pe_sections(kernel_phys_start) } {
            unsafe { kernel_mapper.map_fallback_kernel_region(kernel_phys_start); }
        }
        debug_log_no_alloc!("Kernel segments mapped to higher half");
    }

    // Helper to map additional standard regions
    fn map_additional_regions(&mut self, fb_addr: Option<VirtAddr>, fb_size: Option<u64>) {
        let mut memory_mapper = MemoryMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        memory_mapper.map_framebuffer(fb_addr, fb_size);
        memory_mapper.map_vga();
        memory_mapper.map_boot_code();
        debug_log_no_alloc!("Additional regions mapped");
    }

    // Helper to map special UEFI regions
    fn map_special_regions(&mut self) {
        unsafe {
            self.map_uefi_runtime_to_higher_half();
            self.map_available_memory_to_higher_half();
            self.map_stack_to_higher_half();
        }
        debug_log_no_alloc!("Special regions mapped");
    }

    // Removed redundant map_page_aligned_descriptors_safely function

    unsafe fn map_uefi_runtime_to_higher_half(&mut self) {
        let phys_offset = self.phys_offset; // Copy since VirtAddr is Copy
        map_memory_descriptors_with_config(
            self.mapper,
            self.frame_allocator,
            self.memory_map,
            move |desc| {
                if desc.is_valid()
                    && matches!(
                        desc.type_,
                        crate::common::EfiMemoryType::EfiRuntimeServicesCode
                            | crate::common::EfiMemoryType::EfiRuntimeServicesData
                    )
                {
                    let flags =
                        if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                            PageTableFlags::PRESENT
                        } else {
                            PageTableFlags::PRESENT
                                | PageTableFlags::WRITABLE
                                | PageTableFlags::NO_EXECUTE
                        };
                    Some(higher_half_config!(
                        phys_offset,
                        desc.physical_start,
                        desc.number_of_pages,
                        flags
                    ))
                } else {
                    None
                }
            },
        );
    }

    unsafe fn map_available_memory_to_higher_half(&mut self) {
        process_memory_descriptors(self.memory_map, |desc, start_frame, end_frame| {
            let phys_start = desc.get_physical_start();
            let pages = (end_frame - start_frame) as u64;
            // Always give writable access to available memory regions for compatibility, but executable code regions should not be writable
            let flags = if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                PageTableFlags::PRESENT
            } else {
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
            };
            let _ = map_to_higher_half_with_log(
                self.mapper,
                self.frame_allocator,
                self.phys_offset,
                phys_start,
                pages,
                flags,
            );
        });
    }

    unsafe fn map_stack_to_higher_half(&mut self) {
        map_stack_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.memory_map,
        )
        .expect("Failed to map stack region to higher half");
    }

    unsafe fn map_available_memory_identity(&mut self) {
        for desc in self.memory_map.iter() {
            if desc.is_valid() {
                // Include available memory and UEFI runtime services regions
                let should_identity_map = desc.is_memory_available()
                    || matches!(
                        desc.type_,
                        crate::common::EfiMemoryType::EfiRuntimeServicesCode
                            | crate::common::EfiMemoryType::EfiRuntimeServicesData
                    );

                if should_identity_map {
                    let phys_start = desc.get_physical_start();
                    let pages = desc.get_page_count();
                    // Always give writable access to available memory regions for compatibility, but executable code regions should not be writable
                    let flags =
                        if desc.type_ == crate::common::EfiMemoryType::EfiRuntimeServicesCode {
                            PageTableFlags::PRESENT // Executable, read-only
                        } else {
                            PageTableFlags::PRESENT
                                | PageTableFlags::WRITABLE
                                | PageTableFlags::NO_EXECUTE // Writable, not executable
                        };
                    let _: core::result::Result<
                        (),
                        x86_64::structures::paging::mapper::MapToError<Size4KiB>,
                    > = identity_map_range_with_log_macro!(
                        self.mapper,
                        self.frame_allocator,
                        phys_start,
                        pages,
                        flags
                    );
                }
            }
        }
    }
}

// Perform the page table switch
// Function to assist with page table debugging
fn debug_page_table_info(level_4_table_frame: PhysFrame, phys_offset: VirtAddr) {
    debug_log_no_alloc!(
        "New L4 table phys addr: ",
        level_4_table_frame.start_address().as_u64() as usize
    );
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
                panic!("Failed to map L4 table to higher half: {:?}", e);
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
    let mut reinitializer = PageTableReinitializer::new();
    reinitializer.reinitialize(
        kernel_phys_start,
        fb_addr,
        fb_size,
        frame_allocator,
        memory_map,
        current_physical_memory_offset,
    )
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
    map_range_with_log_macro!(
        mapper,
        frame_allocator,
        phys_start,
        virt_start,
        num_pages,
        flags
    );
    Ok(())
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

impl PageTableManager {
    /// Get the current pml4 frame (for backward compatibility)
    pub fn pml4_frame(&self) -> Option<x86_64::structures::paging::PhysFrame> {
        self.pml4_frame
    }
}

pub type ProcessPageTable = PageTableManager;

fn destroy_page_table_recursive(
    mapper: &mut OffsetPageTable<'static>,
    frame_alloc: &mut BootInfoFrameAllocator,
    table_phys: PhysAddr,
    level: usize,
    temp_va: VirtAddr,
) -> crate::common::logging::SystemResult<()> {
    if level == 0 || level > 4 {
        return Ok(());
    }

    // Temporarily map the table to read its entries
    let page = Page::<Size4KiB>::containing_address(temp_va);
    let frame = PhysFrame::<Size4KiB>::containing_address(table_phys);
    let flush = unsafe {
        mapper
            .map_to(
                page,
                frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                frame_alloc,
            )
            .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
    };
    flush.flush();

    let table = unsafe { &*(temp_va.as_ptr() as *const PageTable) };

    let mut child_frames_to_free = alloc::vec::Vec::new();
    if level > 1 {
        for entry in table.iter() {
            if entry.flags().contains(PageTableFlags::PRESENT)
                && !entry.flags().contains(PageTableFlags::HUGE_PAGE)
            {
                if let Ok(child_frame) = entry.frame() {
                    child_frames_to_free.push(child_frame);
                }
            }
        }
    }

    // Unmap the temporary page before recursing
    if let Ok((_frame, flush)) = mapper.unmap(page) {
        flush.flush();
    }

    // Now recurse on children
    for child_frame in child_frames_to_free {
        destroy_page_table_recursive(
            mapper,
            frame_alloc,
            child_frame.start_address(),
            level - 1,
            TEMP_VA_FOR_DESTROY,
        )?;
        frame_alloc.deallocate_frame(child_frame);
    }

    Ok(())
}

pub struct PageTableManager {
    current_page_table: usize,
    initialized: bool,
    pml4_frame: Option<PhysFrame>,
    mapper: Option<OffsetPageTable<'static>>,
    allocated_tables: alloc::collections::BTreeMap<usize, PhysFrame>,
    frame_allocator: Option<&'static mut BootInfoFrameAllocator>,
}

impl PageTableManager {
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            initialized: false,
            pml4_frame: None,
            mapper: None,
            allocated_tables: alloc::collections::BTreeMap::new(),
            frame_allocator: None,
        }
    }

    pub fn new_with_frame(pml4_frame: x86_64::structures::paging::PhysFrame) -> Self {
        Self {
            current_page_table: pml4_frame.start_address().as_u64() as usize,
            initialized: false,
            pml4_frame: Some(pml4_frame),
            mapper: None,
            allocated_tables: alloc::collections::BTreeMap::new(),
            frame_allocator: None,
        }
    }

    /// Initialize paging (for compatibility)
    pub fn init_paging(&mut self) -> crate::common::logging::SystemResult<()> {
        // No-op for now
        Ok(())
    }

    pub fn initialize_with_frame_allocator(
        &mut self,
        phys_offset: VirtAddr,
        frame_allocator: &'static mut BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()> {
        if self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Get the current active page table
        let (current_pml4, _) = Cr3::read();
        let table_phys_addr = current_pml4.start_address().as_u64();

        // Initialize the mapper with the current table
        self.mapper = Some(unsafe {
            // Temporarily map the current table to access it
            let mut temp_mapper = unsafe { init(phys_offset) };
            let virt_addr = phys_offset + table_phys_addr;
            let page = Page::containing_address(virt_addr);
            match temp_mapper.map_to(
                page,
                current_pml4,
                page_flags_const!(READ_WRITE_NO_EXEC),
                frame_allocator,
            ) {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => { /* Already mapped, which is fine. */
                }
                Err(_) => return Err(crate::common::logging::SystemError::MappingFailed),
            };
            OffsetPageTable::new(
                &mut *(virt_addr.as_mut_ptr() as *mut PageTable),
                phys_offset,
            )
        });

        self.pml4_frame = Some(current_pml4);
        self.current_page_table = table_phys_addr as usize;
        self.allocated_tables
            .insert(table_phys_addr as usize, current_pml4);
        self.frame_allocator = Some(frame_allocator);
        self.initialized = true;
        Ok(())
    }

    fn clone_page_table_recursive(
        mapper: &mut OffsetPageTable<'static>,
        frame_alloc: &mut BootInfoFrameAllocator,
        source_table_phys: PhysAddr,
        level: usize,
        temp_va: VirtAddr,
        allocated_tables: &mut alloc::collections::BTreeMap<usize, PhysFrame>,
    ) -> crate::common::logging::SystemResult<PhysAddr> {
        if level == 0 || level > 4 {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        // Allocate new frame for destination table
        let dest_frame: PhysFrame = match frame_alloc.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };

        // Zero the new table
        unsafe {
            core::ptr::write_bytes(dest_frame.start_address().as_u64() as *mut PageTable, 0, 1);
        }

        // Track the allocated frame
        allocated_tables.insert(dest_frame.start_address().as_u64() as usize, dest_frame);

        // Temporarily map source table for reading
        let source_page = Page::<Size4KiB>::containing_address(temp_va);
        let source_phys_frame = PhysFrame::<Size4KiB>::containing_address(source_table_phys);
        unsafe {
            mapper
                .map_to(
                    source_page,
                    source_phys_frame,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    frame_alloc,
                )
                .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }

        let source_table = unsafe { &mut *(temp_va.as_mut_ptr() as *mut PageTable) };

        // Temporarily map destination table for writing
        let dest_page = Page::<Size4KiB>::containing_address(temp_va + 0x1000u64);
        unsafe {
            mapper
                .map_to(
                    dest_page,
                    dest_frame,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    frame_alloc,
                )
                .map_err(|_| crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }

        let dest_table = unsafe { &mut *((temp_va.as_u64() + 0x1000) as *mut PageTable) };

        let mut child_va = temp_va + 0x2000u64;

        // Copy all entries
        for (_i, (source_entry, dest_entry)) in
            source_table.iter().zip(dest_table.iter_mut()).enumerate()
        {
            if source_entry.flags().contains(PageTableFlags::PRESENT) {
                if level > 1
                    && !((level == 2) && source_entry.flags().contains(PageTableFlags::HUGE_PAGE))
                {
                    // Entry points to a sub-table, recursively clone it
                    match source_entry.frame() {
                        Ok(child_frame) => {
                            let cloned_child_phys = Self::clone_page_table_recursive(
                                mapper,
                                frame_alloc,
                                child_frame.start_address(),
                                level - 1,
                                child_va,
                                allocated_tables,
                            )?;
                            // Update entry to point to cloned child table
                            dest_entry.set_addr(cloned_child_phys, source_entry.flags());
                            child_va += 0x1000u64;
                        }
                        Err(_) => {
                            // Invalid frame, skip
                        }
                    }
                } else {
                    // Leaf entry, copy directly
                    dest_entry.set_addr(source_entry.addr(), source_entry.flags());
                }
            }
        }

        // Unmap temp mappings
        if let Ok((_frame, flush)) = mapper.unmap(source_page) {
            flush.flush();
        }
        if let Ok((_frame, flush)) = mapper.unmap(dest_page) {
            flush.flush();
        }

        Ok(dest_frame.start_address())
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

        // Get a reference to the frame allocator
        let frame_alloc = self.frame_allocator.as_mut().unwrap();

        // Use the configured frame allocator
        let new_frame = match frame_alloc.allocate_frame() {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
        };

        // Temporarily map the page table frame before accessing it
        let mapper = self.mapper.as_mut().unwrap();
        let temp_page = unsafe {
            Page::<Size4KiB>::containing_address(VirtAddr::new(
                TEMP_VA_FOR_CLONE.as_u64() + 0x3000u64,
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

        // Zero the new page table frame
        unsafe {
            let table_va = (TEMP_VA_FOR_CLONE.as_u64() + 0x3000) as *mut u8;
            core::ptr::write_bytes(table_va, 0, 4096);
        }

        // Unmap the temporary mapping
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
        ensure_initialized!(self);

        let table_phys = PhysAddr::new(table_addr as u64);
        if let Some(frame) = self.allocated_tables.remove(&table_addr) {
            let mapper = self.mapper.as_mut().unwrap();
            let frame_alloc = self.frame_allocator.as_deref_mut().unwrap();
            // Recursively destroy lower level tables
            destroy_page_table_recursive(mapper, frame_alloc, table_phys, 4, TEMP_VA_FOR_DESTROY)?;
            // Now deallocate the L4 frame
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
        ensure_initialized!(self);

        let source_frame = match self.allocated_tables.get(&source_table) {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::InvalidArgument),
        };

        let mapper = self.mapper.as_mut().unwrap();
        let frame_alloc = self.frame_allocator.as_mut().unwrap();

        // Clone recursively starting from L4 level (level 4)
        let cloned_phys = Self::clone_page_table_recursive(
            mapper,
            frame_alloc,
            source_frame.start_address(),
            4,
            TEMP_VA_FOR_CLONE, // Use a different temp VA than destroy
            &mut self.allocated_tables,
        )?;

        let new_table_addr = cloned_phys.as_u64() as usize;
        // Note: allocated_tables tracking is done inside the recursive function

        Ok(new_table_addr)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()> {
        ensure_initialized!(self);

        let new_frame = match self.allocated_tables.get(&table_addr) {
            Some(frame) => frame,
            None => return Err(crate::common::logging::SystemError::InvalidArgument),
        };

        unsafe {
            Cr3::write(*new_frame, x86_64::registers::control::Cr3Flags::empty());
        }

        self.pml4_frame = Some(*new_frame);
        self.current_page_table = table_addr;

        Ok(())
    }

    fn current_page_table(&self) -> usize {
        self.current_page_table
    }
}

impl crate::initializer::Initializable for PageTableManager {
    fn init(&mut self) -> crate::common::logging::SystemResult<()> {
        // This is a no-op for PageTableManager, initialization is done in initialize_with_frame_allocator
        Ok(())
    }

    fn name(&self) -> &'static str {
        "PageTableManager"
    }

    fn priority(&self) -> i32 {
        // Lower priority than UnifiedMemoryManager
        900
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
    log_page_table_op!(
        "PE size calculation",
        "starting",
        kernel_phys_start.as_u64() as usize
    );

    if kernel_phys_start.as_u64() == 0 {
        debug_log_no_alloc!("Kernel phys start is 0, using fallback size");
        return FALLBACK_KERNEL_SIZE;
    }

    let parser = match unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
        Some(p) => {
            log_page_table_op!("PE size calculation", "parser created successfully", 0);
            p
        }
        None => {
            log_page_table_op!(
                "PE size calculation",
                "parser creation failed, using fallback",
                0
            );
            return FALLBACK_KERNEL_SIZE;
        }
    };

    match unsafe { parser.size_of_image() } {
        Some(size) => {
            let padded_size = (size + KERNEL_MEMORY_PADDING).div_ceil(4096) * 4096;
            log_page_table_op!(
                "PE size calculation",
                "parsing successful",
                padded_size as usize
            );
            padded_size
        }
        None => {
            log_page_table_op!(
                "PE size calculation",
                "size_of_image failed, using fallback",
                0
            );
            FALLBACK_KERNEL_SIZE
        }
    }
}
