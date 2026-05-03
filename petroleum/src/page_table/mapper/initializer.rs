use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{
    PhysFrame, OffsetPageTable, PageTableFlags, PageTable, Size4KiB, Mapper, FrameAllocator,
};
use crate::page_table::constants::BootInfoFrameAllocator;
use crate::page_table::mapper::mapper::{MemoryMapper, KernelMapper};
use crate::page_table::mapper::helpers::{
    map_available_memory_to_higher_half,
    map_stack_to_higher_half,
};

pub struct PageTableInitializer<'a, T: crate::page_table::efi_memory::MemoryDescriptorValidator> {
    pub mapper: &'a mut OffsetPageTable<'static>,
    pub frame_allocator: &'a mut BootInfoFrameAllocator,
    pub phys_offset: VirtAddr,
    pub current_phys_offset: VirtAddr,
    pub memory_map: &'a [T],
    pub uefi_map_phys: u64,
    pub uefi_map_size: u64,
}

impl<'a, T: crate::page_table::efi_memory::MemoryDescriptorValidator> PageTableInitializer<'a, T> {
    pub fn new(
        mapper: &'a mut OffsetPageTable<'static>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
        current_phys_offset: VirtAddr,
        memory_map: &'a [T],
        uefi_map_phys: u64,
        uefi_map_size: u64,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
            current_phys_offset,
            memory_map,
            uefi_map_phys,
            uefi_map_size,
        }
    }

    pub fn setup_transition_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        level_4_table_frame: PhysFrame,
    ) -> u64 {
        crate::debug_log_no_alloc!("Setting up transition mappings for CR3 switch");
        
        let kernel_size = self.map_essential_regions(kernel_phys_start, level_4_table_frame);
        crate::debug_log_no_alloc!("Essential regions mapped");
        
        unsafe {
            // 1. Map current stack (RSP)
            let rsp: u64;
            core::arch::asm!("mov {}, rsp", out(reg) rsp);
            let rsp_phys = rsp.wrapping_sub(self.current_phys_offset.as_u64());
            let stack_phys_start = rsp_phys.wrapping_sub(2 * 1024 * 1024) & !0xFFF;
            let stack_pages = (4 * 1024 * 1024) / 4096;
            
            self.map_identity_config_4kiB(stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.current_phys_offset, stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.phys_offset, stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            crate::debug_log_no_alloc!("Current stack region identity, current-offset, AND high-half mapped: 0x{:x}", stack_phys_start);

            // 2. Map current instruction pointer (RIP)
            let rip: u64;
            core::arch::asm!("lea {}, [rip]", out(reg) rip);
            let rip_phys = rip.wrapping_sub(self.current_phys_offset.as_u64());
            let code_phys_start = rip_phys.wrapping_sub(2 * 1024 * 1024) & !0xFFF;
            let code_pages = (4 * 1024 * 1024) / 4096;

            self.map_identity_config_4kiB(code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE_EXEC));
            self.map_at_offset_config_4kiB(self.current_phys_offset, code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE_EXEC));
            self.map_at_offset_config_4kiB(self.phys_offset, code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE_EXEC));
            crate::debug_log_no_alloc!("Current code region identity, current-offset, AND high-half mapped: 0x{:x}", code_phys_start);
        }
        
        unsafe {
            let low_mem_start = 0u64;
            let low_mem_size = 4 * 1024 * 1024 * 1024; // 4GiB
            let region_pages = low_mem_size / 4096;
            // Force WRITABLE and EXECUTABLE for the entire low memory region during transition
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            
            self.map_identity_config_4kiB(
                low_mem_start,
                region_pages,
                flags,
            );
            
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                low_mem_start,
                region_pages,
                flags,
            );
            
            crate::debug_log_no_alloc!("Low physical memory (4GiB) identity AND high-half mapped as RW for transition");
        }
        
        unsafe {
            let gdt_virt_addr = core::ptr::addr_of!(crate::page_table::mapper::transition::TRANSITION_GDT) as *const _ as u64;
            let gdt_phys_addr = (gdt_virt_addr.wrapping_sub(self.current_phys_offset.as_u64())) & !0xFFF;
            
            self.map_identity_config_4kiB(gdt_phys_addr, 1, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                gdt_phys_addr,
                1,
                crate::page_flags_const!(READ_WRITE),
            );
            
            crate::debug_log_no_alloc!("Transition GDT identity AND high-half mapped at phys: 0x{:x}", gdt_phys_addr);
        }
        
        crate::debug_log_no_alloc!("Transition mappings completed");
        kernel_size
    }

    fn map_essential_regions(
        &mut self,
        kernel_phys_start: PhysAddr,
        level_4_table_frame: PhysFrame,
    ) -> u64 {
        unsafe {
            for i in 0..(512 * 1024 * 1024 / (2 * 1024 * 1024)) {
                let start = i * 2 * 1024 * 1024;
                self.map_identity_config_4kiB(
                    start,
                    (2 * 1024 * 1024) / 4096,
                    crate::page_flags_const!(READ_WRITE),
                );
            }

            let bitmap_virt_start =
                (&raw const crate::page_table::bitmap_allocator::BITMAP_STATIC) as *const _ as usize as u64;
            let bitmap_phys_start = bitmap_virt_start.wrapping_sub(self.current_phys_offset.as_u64());
            let bitmap_pages = ((131072 * 8) + 4095) / 4096;
            self.map_identity_config_4kiB(bitmap_phys_start, bitmap_pages, crate::page_flags_const!(READ_WRITE_NO_EXEC));
            self.map_identity_config_4kiB(
                level_4_table_frame.start_address().as_u64(),
                1,
                crate::page_flags_const!(READ_WRITE_NO_EXEC),
            );
            self.map_identity_config_4kiB(4096, crate::page_table::constants::UEFI_COMPAT_PAGES, crate::page_flags_const!(READ_WRITE_NO_EXEC));

            let uefi_map_pages = (self.uefi_map_size + 4095) / 4096;
            self.map_identity_config_4kiB(
                self.uefi_map_phys,
                uefi_map_pages,
                crate::page_flags_const!(READ_WRITE_NO_EXEC),
            );
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                self.uefi_map_phys,
                uefi_map_pages,
                crate::page_flags_const!(READ_WRITE_NO_EXEC),
            );
            
            let (pe_base, kernel_size) = if let Some(parser) = unsafe { crate::page_table::pe::PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
                let base = parser.pe_base as u64;
                let size = parser.size_of_image().unwrap_or(crate::page_table::pe::FALLBACK_KERNEL_SIZE);
                (base, size)
            } else {
                (kernel_phys_start.as_u64(), crate::page_table::pe::FALLBACK_KERNEL_SIZE)
            };
            let kernel_pages = kernel_size.div_ceil(4096);
            
            self.map_identity_config_4kiB(pe_base, kernel_pages, crate::page_flags_const!(READ_WRITE));
            
            self.map_at_offset_config_4kiB(
                self.current_phys_offset,
                pe_base,
                kernel_pages,
                crate::page_flags_const!(READ_WRITE),
            );

            self.map_at_offset_config_4kiB(
                self.phys_offset,
                pe_base,
                kernel_pages,
                crate::page_flags_const!(READ_WRITE_EXEC),
            );
            
            self.map_identity_config_4kiB(crate::page_table::constants::BOOT_CODE_START, crate::page_table::constants::BOOT_CODE_PAGES, crate::page_flags_const!(READ_WRITE));
            kernel_size
        }
    }

    unsafe fn map_identity_config(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) {
        crate::identity_map_range_with_log_macro!(
            self.mapper,
            self.frame_allocator,
            phys_start,
            num_pages,
            flags
        );
    }

    unsafe fn map_identity_config_4kiB(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) { unsafe {
        let _ = crate::page_table::utils::map_range_4kiB(
            self.mapper,
            self.frame_allocator,
            phys_start,
            phys_start,
            num_pages,
            flags,
            "panic",
        );
    }}

    unsafe fn map_at_offset_config_4kiB(
        &mut self,
        offset: VirtAddr,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) { unsafe {
        let virt_start = offset.as_u64() + phys_start;
        let _ = crate::page_table::utils::map_range_4kiB(
            self.mapper,
            self.frame_allocator,
            phys_start,
            virt_start,
            num_pages,
            flags,
            "panic",
        );
    }}

    fn map_current_stack_identity(&mut self) {
        crate::map_current_stack!(
            self.mapper,
            self.frame_allocator,
            self.memory_map,
            crate::page_flags_const!(READ_WRITE_NO_EXEC)
        );
    }

    pub fn setup_higher_half_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
    ) {
        crate::debug_log_no_alloc!("Setting up higher-half mappings");
        let mut kernel_mapper =
            KernelMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        if !unsafe { kernel_mapper.map_pe_sections(kernel_phys_start) } {
            unsafe {
                kernel_mapper.map_fallback_kernel_region(kernel_phys_start);
            }
        }

        // FORCE: Map a large kernel region as WRITABLE to prevent Page Faults on static variables.
        // This overrides any restrictive PE section flags for the first 128MB of the kernel.
        unsafe {
            let kernel_virt_start = self.phys_offset + kernel_phys_start.as_u64();
            let kernel_phys_start_raw = kernel_phys_start.as_u64();
            let region_pages = (128 * 1024 * 1024) / 4096;
            
            for i in 0..region_pages {
                let v_page = kernel_virt_start + (i * 4096);
                let p_page = kernel_phys_start_raw + (i * 4096);
                let _ = self.mapper.map_to(
                    x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_page),
                    x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(p_page)),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE, // NO_EXECUTEを付けない = 実行可能
                    self.frame_allocator,
                );
            }
            crate::debug_log_no_alloc!("Forced 128MB kernel region to be WRITABLE and EXECUTABLE");
        }

        crate::debug_log_no_alloc!("Kernel segments mapped to higher half");
        unsafe {
            crate::debug_log_no_alloc!("Mapping available memory to higher half...");
            self.map_available_memory_to_higher_half();
            crate::debug_log_no_alloc!("Mapping UEFI runtime to higher half...");
            self.map_uefi_runtime_to_higher_half();
            crate::debug_log_no_alloc!("Mapping stack to higher half...");
            self.map_stack_to_higher_half();
        }
        crate::debug_log_no_alloc!("Special regions mapped");

        let mut memory_mapper =
            MemoryMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        memory_mapper.map_framebuffer(fb_addr, fb_size);
        memory_mapper.map_vga();
        memory_mapper.map_boot_code();
        crate::debug_log_no_alloc!("Additional regions mapped");
        crate::debug_log_no_alloc!("Higher-half mappings completed");
    }

    unsafe fn map_uefi_runtime_to_higher_half(&mut self) { unsafe {
        map_available_memory_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.memory_map,
        );
    }}

    unsafe fn map_available_memory_to_higher_half(&mut self) { unsafe {
        map_available_memory_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.memory_map,
        );
    }}

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
                let should_identity_map = desc.is_memory_available()
                    || (desc.get_type()
                        == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32
                        || desc.get_type()
                            == crate::common::EfiMemoryType::EfiRuntimeServicesData as u32)
                    || desc.get_type() == crate::common::EfiMemoryType::EfiBootServicesCode as u32
                    || desc.get_type() == crate::common::EfiMemoryType::EfiBootServicesData as u32;
                if should_identity_map {
                    let phys_start = desc.get_physical_start();
                    let pages = desc.get_page_count();
                    let flags = if desc.get_type()
                        == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32
                    {
                        PageTableFlags::PRESENT
                    } else {
                        PageTableFlags::PRESENT
                            | PageTableFlags::WRITABLE
                            | PageTableFlags::NO_EXECUTE
                    };
                    let _: core::result::Result<
                        (),
                        x86_64::structures::paging::mapper::MapToError<Size4KiB>,
                    > = crate::identity_map_range_with_log_macro!(
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

pub struct PageTableReinitializer {
    pub phys_offset: VirtAddr,
}

impl PageTableReinitializer {
    pub fn new() -> Self {
        Self {
            phys_offset: crate::page_table::constants::HIGHER_HALF_OFFSET,
        }
    }

    pub fn reinitialize<T, F>(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
        frame_allocator: &mut BootInfoFrameAllocator,
        memory_map: &[T],
        uefi_map_phys: u64,
        uefi_map_size: u64,
        current_physical_memory_offset: VirtAddr,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        extra_mappings: Option<F>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) -> VirtAddr 
    where 
        T: crate::page_table::efi_memory::MemoryDescriptorValidator,
        F: FnOnce(&mut OffsetPageTable, &mut BootInfoFrameAllocator, VirtAddr),
    {
        crate::debug_log_no_alloc!("Page table reinitialization starting");
        let level_4_table_frame =
            self.create_page_table(frame_allocator, current_physical_memory_offset);
        let mut mapper = self.setup_new_mapper(
            level_4_table_frame,
            current_physical_memory_offset,
            frame_allocator,
        );
        let mut initializer =
            PageTableInitializer::new(
                &mut mapper,
                frame_allocator,
                self.phys_offset,
                current_physical_memory_offset,
                memory_map,
                uefi_map_phys,
                uefi_map_size,
            );
        
        let _kernel_size =
            unsafe { initializer.setup_transition_mappings(kernel_phys_start, level_4_table_frame) };
        
        initializer.setup_higher_half_mappings(kernel_phys_start, fb_addr, fb_size);

        if let Some(mapping_fn) = extra_mappings {
            unsafe {
                mapping_fn(&mut mapper, frame_allocator, self.phys_offset);
            }
        }
        
        self.setup_recursive_mapping(&mut mapper, level_4_table_frame);
        
        unsafe {
            let l4_phys = level_4_table_frame.start_address().as_u64();
            let l4_virt = self.phys_offset.as_u64() + l4_phys;
            crate::map_range_with_log_macro!(
                &mut mapper,
                frame_allocator,
                l4_phys,
                l4_virt,
                1,
                crate::page_flags_const!(READ_WRITE_NO_EXEC)
            );
            crate::debug_log_no_alloc!("Pre-mapped L4 table to new phys_offset: 0x", l4_virt as usize);
        }

        self.perform_page_table_switch(
            &mut mapper,
            level_4_table_frame,
            frame_allocator,
            current_physical_memory_offset,
            load_gdt,
            load_idt,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        );
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Page table switch: returned to reinitialize\n");
        
        self.phys_offset
    }

    fn create_page_table(
        &self,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
    ) -> PhysFrame {
        crate::debug_log_no_alloc!("Allocating new L4 page table frame");
        let level_4_table_frame = match frame_allocator.allocate_frame() {
            Some(frame) => frame,
            None => panic!("Failed to allocate L4 page table frame"),
        };
        unsafe {
            let table_phys = level_4_table_frame.start_address().as_u64();
            let table_virt = current_physical_memory_offset + table_phys;
            let table_ptr = table_virt.as_mut_ptr() as *mut PageTable;
            *table_ptr = PageTable::new();
        }
        crate::debug_log_no_alloc!("New L4 page table created and zeroed");
        level_4_table_frame
    }

    fn setup_new_mapper(
        &self,
        level_4_table_frame: PhysFrame,
        current_physical_memory_offset: VirtAddr,
        _frame_allocator: &mut BootInfoFrameAllocator,
    ) -> OffsetPageTable<'static> {
        crate::debug_log_no_alloc!("Setting up new page table mapper");
        let temp_phys_addr = level_4_table_frame.start_address().as_u64();
        
        let temp_virt_addr = current_physical_memory_offset + temp_phys_addr;
        
        crate::debug_log_no_alloc!(
            "L4 Table Phys: 0x{:x}, Virt: 0x{:x}",
            temp_phys_addr,
            temp_virt_addr.as_u64()
        );

        unsafe {
            OffsetPageTable::new(
                &mut *(temp_virt_addr.as_mut_ptr() as *mut PageTable),
                current_physical_memory_offset,
            )
        }
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
                    crate::page_flags_const!(READ_WRITE),
                );
        }
    }

    fn perform_page_table_switch(
        &self,
        mapper: &mut OffsetPageTable,
        level_4_table_frame: PhysFrame,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) {
        x86_64::instructions::interrupts::disable();
        crate::debug_log_no_alloc!("About to switch CR3 to new table: 0x", level_4_table_frame.start_address().as_u64() as usize);
        
        let ctx = crate::page_table::mapper::transition::TransitionContext::prepare(
            self.phys_offset,
            current_physical_memory_offset,
            level_4_table_frame,
            frame_allocator,
            load_gdt,
            load_idt,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        );

        unsafe {
            // Explicitly map the KernelArgs region to avoid Page Faults when accessing it in init_common
            if let Some(args_phys) = kernel_args_phys {
                let args_virt = VirtAddr::new(args_phys + self.phys_offset.as_u64());
                let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(args_virt);
                let _ = mapper.map_to(
                    page,
                    x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(args_phys) }),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    frame_allocator,
                );
                crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: KernelArgs region mapped\n");
            }

            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: inside unsafe block, getting RIP\n");
            let rip: u64;
            core::arch::asm!("lea {}, [rip]", out(reg) rip);
            
            let rip_phys = rip.wrapping_sub(current_physical_memory_offset.as_u64());
            let rip_region_start = (rip_phys.wrapping_sub(2 * 1024 * 1024)) & !0xFFF;
            let rip_region_pages = (4 * 1024 * 1024) / 4096;
            
            for i in 0..rip_region_pages {
                let p_phys = rip_region_start + (i * 4096);
                let v_addr = VirtAddr::new(p_phys.wrapping_add(current_physical_memory_offset.as_u64()));
                let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_addr);
                let _ = mapper.unmap(page);
                    let _ = mapper.map_to(
                        page,
                        x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(p_phys) }),
                        crate::page_flags_const!(READ_WRITE_EXEC),
                        frame_allocator,
                    );
            }
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: RIP region mapped\n");
            crate::debug_log_no_alloc!("Current RIP region (4MB) explicitly mapped to current virtual address in new page table");
            
            let kernel_base_virt = crate::assembly::landing_zone as *const () as usize as u64;
            let kernel_base_phys = kernel_base_virt.wrapping_sub(current_physical_memory_offset.as_u64());
            let region_start_phys = (kernel_base_phys.wrapping_sub(1024 * 1024)) & !0xFFF;
            let region_pages = (64 * 1024 * 1024) / 4096;
            
            for i in 0..region_pages {
                let p_phys = region_start_phys + (i * 4096);
                let v_low = VirtAddr::new(p_phys.wrapping_add(current_physical_memory_offset.as_u64()));
                let page_low = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_low);
                let _ = mapper.unmap(page_low);
                    let _ = mapper.map_to(
                        page_low,
                        x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(p_phys) }),
                        crate::page_flags_const!(READ_WRITE_EXEC),
                        frame_allocator,
                    );
                let v_high = VirtAddr::new(p_phys.wrapping_add(self.phys_offset.as_u64()));
                let page_high = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_high);
                let _ = mapper.unmap(page_high);
                    let _ = mapper.map_to(
                        page_high,
                        x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(p_phys) }),
                        crate::page_flags_const!(READ_WRITE_EXEC),
                        frame_allocator,
                    );
            }
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: Landing zone region mapped\n");
            crate::mem_debug!("landing_zone region (2MB) mapped at low and high", "\n");

            // Explicitly map the landing_zone_logic page to ensure the jump succeeds
            let logic_fn_addr_low = crate::page_table::mapper::transition::landing_zone_logic as *const () as u64;
            // We are currently in the low half, so subtract current_physical_memory_offset to get physical address
            let logic_fn_phys = logic_fn_addr_low.wrapping_sub(current_physical_memory_offset.as_u64());
            let logic_fn_page = logic_fn_phys & !0xFFF;
            
            let logic_frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(logic_fn_page));
            
            // Map to identity address
            let logic_page_identity = x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(logic_fn_page));
            let _ = mapper.map_to(
                logic_page_identity,
                logic_frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE, 
                frame_allocator,
            );
            
            // Map to high-half address: logic_fn_phys + self.phys_offset
            let logic_page_high = x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(logic_fn_page + self.phys_offset.as_u64()));
            let _ = mapper.map_to(
                logic_page_high,
                logic_frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE, 
                frame_allocator,
            );
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: landing_zone_logic page explicitly mapped (fixed phys calculation)\n");

                if let Some(gdt_fn) = load_gdt {
                    let gdt_addr = gdt_fn as *const () as u64;
                    let gdt_phys = gdt_addr.wrapping_sub(current_physical_memory_offset.as_u64());
                    let gdt_page_start = gdt_phys & !0xFFF;
                        let _ = mapper.map_to(
                            x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(gdt_page_start.wrapping_add(current_physical_memory_offset.as_u64()))),
                            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(gdt_page_start) }),
                            crate::page_flags_const!(READ_WRITE_EXEC),
                            frame_allocator,
                        );
                        let _ = mapper.map_to(
                            x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(gdt_page_start.wrapping_add(self.phys_offset.as_u64()))),
                            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(gdt_page_start) }),
                            crate::page_flags_const!(READ_WRITE_EXEC),
                            frame_allocator,
                        );
                }
                if let Some(idt_fn) = load_idt {
                    let idt_addr = idt_fn as *const () as u64;
                    let idt_phys = idt_addr.wrapping_sub(current_physical_memory_offset.as_u64());
                    let idt_page_start = idt_phys & !0xFFF;
                        let _ = mapper.map_to(
                            x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(idt_page_start.wrapping_add(current_physical_memory_offset.as_u64()))),
                            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(idt_page_start) }),
                            crate::page_flags_const!(READ_WRITE_EXEC),
                            frame_allocator,
                        );
                        let _ = mapper.map_to(
                            x86_64::structures::paging::Page::<Size4KiB>::containing_address(VirtAddr::new(idt_page_start.wrapping_add(self.phys_offset.as_u64()))),
                            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(unsafe { PhysAddr::new(idt_page_start) }),
                            crate::page_flags_const!(READ_WRITE_EXEC),
                            frame_allocator,
                        );
                }
         }

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"CR3 switch: about to enter asm! block\n");
        
        // DEBUG: Print the address of the KERNEL_ARGS variable itself
        let ka_addr = unsafe { &raw const crate::page_table::mapper::transition::KERNEL_ARGS } as *const _ as u64;
        let mut buf = [0u8; 16];
        let len = crate::serial::format_hex_to_buffer(ka_addr, &mut buf, 16);
        unsafe { crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: KERNEL_ARGS var addr: 0x") };
        unsafe { crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]) };
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling perform_world_switch now\n");
        
        let mut buf = [0u8; 16];
        let len = crate::serial::format_hex_to_buffer(ctx.kernel_entry as u64, &mut buf, 16);
        unsafe { crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: ctx.kernel_entry: 0x") };
        unsafe { crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]) };
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        crate::page_table::mapper::transition::perform_world_switch(ctx);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"CR3 switch: returned from asm! block\n");
    }
}

pub fn reinit_page_table_with_allocator<T, F>(
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[T],
    uefi_map_phys: u64,
    uefi_map_size: u64,
    current_physical_memory_offset: VirtAddr,
    load_gdt: Option<fn()>,
    load_idt: Option<fn()>,
    extra_mappings: Option<F>,
    gdt_ptr: Option<*const u8>,
    kernel_entry: Option<usize>,
    kernel_args_phys: Option<u64>,
) -> VirtAddr 
where 
    T: crate::page_table::efi_memory::MemoryDescriptorValidator,
    F: FnOnce(&mut OffsetPageTable, &mut BootInfoFrameAllocator, VirtAddr),
{
    let mut reinitializer = PageTableReinitializer::new();
        reinitializer.reinitialize(
            kernel_phys_start,
            fb_addr,
            fb_size,
            frame_allocator,
            memory_map,
            uefi_map_phys,
            uefi_map_size,
            current_physical_memory_offset,
            load_gdt,
            load_idt,
            extra_mappings,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        )
}