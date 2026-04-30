use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        Mapper, OffsetPageTable, PageTableFlags, Size4KiB, PhysFrame, Page, PageTable, FrameAllocator,
    },
};
use crate::page_table::constants::{BootInfoFrameAllocator};

pub trait MemoryMappable {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;
}

pub struct MemoryMapper<'a> {
    pub mapper: &'a mut OffsetPageTable<'static>,
    pub frame_allocator: &'a mut BootInfoFrameAllocator,
    pub phys_offset: VirtAddr,
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
}

impl<'a> MemoryMappable for MemoryMapper<'a> {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                virt_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            crate::identity_map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        let virt_start = self.phys_offset.as_u64() + phys_start;
        unsafe {
            crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                virt_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }
}

impl<'a> MemoryMapper<'a> {
    pub fn map_vga(&mut self) {
        use crate::page_table::constants::{VGA_MEMORY_END, VGA_MEMORY_START};
        const VGA_PAGES: u64 = (VGA_MEMORY_END - VGA_MEMORY_START + 4095) / 4096;
        let flags = crate::page_flags_const!(READ_WRITE_NO_EXEC);
        let _ = self.map_region_dual(VGA_MEMORY_START, VGA_PAGES, flags);
    }

    pub fn map_boot_code(&mut self) {
        use crate::page_table::constants::{BOOT_CODE_PAGES, BOOT_CODE_START};
        let flags = crate::page_flags_const!(READ_WRITE);
        unsafe {
            let _ = crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                BOOT_CODE_START,
                self.phys_offset.as_u64() + BOOT_CODE_START,
                BOOT_CODE_PAGES,
                flags
            );

            for i in 0..BOOT_CODE_PAGES {
                let virt_addr = self.phys_offset.as_u64() + BOOT_CODE_START + (i * 4096);
                crate::page_table::utils::force_update_page_flags_no_flush(
                    self.mapper,
                    x86_64::VirtAddr::new(virt_addr),
                    flags,
                );
            }
            x86_64::instructions::tlb::flush_all();
            crate::debug_log_no_alloc!("Boot code flags forcefully updated to READ_WRITE (global TLB flush)");
        }
    }

    fn map_region_dual(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        self.map_to_higher_half(phys_start, num_pages, flags)?;
        self.map_to_identity(phys_start, num_pages, flags)?;
        Ok(())
    }

    pub fn map_framebuffer(&mut self, addr: Option<VirtAddr>, size: Option<u64>) {
        if let (Some(addr), Some(size)) = (addr, size) {
            // Sanity check for size to prevent overflow and excessive mapping
            if size == 0 || size > 1024 * 1024 * 1024 * 10 { // 10 GiB limit
                return;
            }
            let pages = size.wrapping_add(4095) / 4096;
            let flags = crate::page_flags_const!(READ_WRITE);
            // addr is already the physical address from UEFI config
            let phys_start = addr.as_u64();
            let _ = self.map_region_dual(phys_start, pages, flags);
        }
    }
}

pub struct KernelMapper<'a, 'b> {
    pub mapper: &'a mut OffsetPageTable<'b>,
    pub frame_allocator: &'a mut BootInfoFrameAllocator,
    pub phys_offset: VirtAddr,
}

impl<'a, 'b> KernelMapper<'a, 'b> {
    pub fn new(
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

    pub unsafe fn map_pe_sections(&mut self, kernel_phys_start: PhysAddr) -> bool {
        if let Some(parser) = unsafe { crate::page_table::pe::PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
            let pe_base_phys = parser.pe_base as u64;
            if let Some(sections) = unsafe { parser.sections() } {
                for section in sections.into_iter().filter(|s| s.virtual_size > 0) {
                    unsafe {
                        self.map_single_pe_section(section, pe_base_phys);
                    }
                }
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    pub unsafe fn map_fallback_kernel_region(&mut self, kernel_phys_start: PhysAddr) {
        use crate::page_table::pe::FALLBACK_KERNEL_SIZE;
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        let flags = crate::page_flags_const!(READ_WRITE);
        unsafe {
            crate::page_table::utils::map_identity_range(
                self.mapper,
                self.frame_allocator,
                kernel_phys_start.as_u64(),
                kernel_pages,
                flags,
            )
            .expect("Failed to map fallback kernel range");
        }
    }

    unsafe fn map_single_pe_section(&mut self, section: crate::page_table::pe::PeSection, pe_base_phys: u64) {
        unsafe { crate::page_table::mapper::helpers::map_pe_section(
            self.mapper,
            section,
            pe_base_phys,
            self.phys_offset,
            self.frame_allocator,
        ); }
    }
}