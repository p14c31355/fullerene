use crate::MEMORY_MAP;
use crate::heap;
use petroleum::FramebufferLike;
use crate::memory::find_heap_start;
use crate::{gdt, graphics, interrupts, memory};
use core::ffi::c_void;
use petroleum::common::uefi::{write_vga_string};
use petroleum::common::{
    ConfigWithMetadata, EfiSystemTable, FRAMEBUFFER_CONFIG_MAGIC,
};
use petroleum::page_table::efi_memory::MemoryMapDescriptor;
use petroleum::page_table::MemoryMappable;
use petroleum::{
    allocate_heap_from_map, debug_log, debug_log_no_alloc, mem_debug, write_serial_bytes,
};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTableFlags, Mapper, mapper::MapToError},
};

/// Helper struct for UEFI initialization context
#[repr(C)]
pub struct UefiInitContext {
    /// Reference to EFI system table
    pub system_table: &'static EfiSystemTable,
    /// EFI memory map data
    pub memory_map: *mut c_void,
    /// Memory map size
    pub memory_map_size: usize,
    /// Descriptor size for memory map entries
    pub descriptor_size: usize,
    /// Physical memory offset after page table reconfiguration
    pub physical_memory_offset: VirtAddr,
    /// Virtual heap start address
    pub virtual_heap_start: VirtAddr,
    /// Heap start after GDT allocation
    pub heap_start_after_gdt: VirtAddr,
    /// Heap start after stack allocation
    pub heap_start_after_stack: VirtAddr,
}

impl UefiInitContext {
    /// Early initialization: serial, VGA, memory maps
    #[cfg(target_os = "uefi")]
    pub fn early_initialization(&mut self) -> PhysAddr {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: early_initialization start\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Entering serial_init\n");
        
        // Diagnostic: Direct port write to verify I/O permissions
        unsafe {
            x86_64::instructions::port::Port::<u8>::new(0x3F8).write(b'!');
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Direct write done\n");

        petroleum::serial::serial_init();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: serial_init done\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: efi_main entered\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Printing efi_main address\n");
        let mut buf = [0u8; 16];
        // Note: efi_main is in uefi_entry.rs
        let len = petroleum::serial::format_hex_to_buffer(crate::boot::uefi_entry::efi_main as u64, &mut buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: efi_main located at 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: efi_main address printed\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Attempting VGA buffer access 1\n");
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 0, b"Kernel boot (UEFI)", 0x1F00);
            write_vga_string(vga_buffer, 1, b"Early init start", 0x1F00);
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: VGA buffer access 1 successful\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Attempting VGA buffer access 2\n");
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 2, b"Serial init done", 0x1F00);
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: VGA buffer access 2 successful\n");
        
        write_serial_bytes!(0x3F8, 0x3FD, b"Early setup completed\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling setup_kernel_location\n");
        
        let mut map_buf = [0u8; 16];
        let map_len = petroleum::serial::format_hex_to_buffer(self.memory_map as u64, &mut map_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_map ptr: 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &map_buf[..map_len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        let kernel_virt_addr = crate::boot::uefi_entry::efi_main as u64;
        let kernel_phys_addr = kernel_virt_addr.wrapping_sub(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64);
        
        let res = crate::memory::setup_kernel_location(
            self.memory_map,
            self.memory_map_size,
            kernel_phys_addr,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: setup_kernel_location returned\n");
        res
    }

    pub fn memory_management_initialization(
        &mut self,
        kernel_phys_start: PhysAddr,
    ) -> (VirtAddr, PhysAddr, VirtAddr) {
        // Set physical memory offset FIRST so it can be used by init_memory_map
        self.physical_memory_offset = x86_64::VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64);

        debug_log_no_alloc!("DEBUG: Starting memory_management_initialization");
        debug_log_no_alloc!("DEBUG: Offset value: ", self.physical_memory_offset.as_u64());

        debug_log_no_alloc!("DEBUG: Calling init_memory_map...");
        self.init_memory_map();
        debug_log_no_alloc!("DEBUG: init_memory_map returned");
        
        let memory_map_ref = MEMORY_MAP.lock().as_ref().expect("Memory map not initialized").clone();
        debug_log_no_alloc!("DEBUG: Memory map reference acquired at 0x", memory_map_ref.as_ptr() as usize);
        
        heap::init_frame_allocator(memory_map_ref);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Heap frame allocator initialized\n");


        // Now that FRAME_ALLOCATOR is ready, map the memory_map buffer to higher half for consistency
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Mapping memory_map buffer to higher half...\n");
        
        let map_addr = self.memory_map as u64;
        let offset_val = self.physical_memory_offset.as_u64();
        
        // Check if memory_map is already a virtual address in the higher half
        if map_addr >= 0xFFFF_8000_0000_0000 {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_map is already in higher half, skipping re-mapping\n");
            // Use the already existing mapping
            let map_virt = map_addr;
            let map_size = self.memory_map_size;
            let map_pages = ((map_size as u64) + 4095) / 4096;
            
            // We don't need to call mapper.map_to because it's already mapped by the bootloader
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Memory map buffer already mapped\n");
        } else {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_map is physical, mapping to higher half\n");
            let map_phys = map_addr;
            let map_virt = map_phys + offset_val;
            let map_size = self.memory_map_size;
            let map_pages = ((map_size as u64) + 4095) / 4096;

            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling petroleum::page_table::init (1)...\n");
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator should be ready now");
            let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset, frame_allocator) };
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: petroleum::page_table::init (1) done\n");
            {
                for i in 0..map_pages {
                    let v_page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(
                        VirtAddr::new(map_virt + i * 4096)
                    );
                    let p_frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
                        PhysAddr::new(map_phys + i * 4096)
                    );

                    unsafe {
                        if let Ok(flush) = mapper.map_to(
                            v_page,
                            p_frame,
                            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                            frame_allocator,
                        ) {
                            flush.flush();
                        }
                    }
                }
            }
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Memory map buffer mapped successfully\n");
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Allocating TSS stacks...\n");
        let tss_stack_pages = (crate::gdt::GDT_TSS_STACK_COUNT * crate::gdt::GDT_TSS_STACK_SIZE) / 4096;
        
        let tss_phys_addr = {
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");
            debug_log_no_alloc!("DEBUG: Frame allocator lock acquired for TSS");
            
            debug_log_no_alloc!("DEBUG: Attempting to allocate contiguous frames: ", tss_stack_pages);
            match frame_allocator.allocate_contiguous_frames(tss_stack_pages) {
                Ok(phys_addr) => {
                    debug_log_no_alloc!("DEBUG: TSS frames allocated at 0x", phys_addr);
                    PhysAddr::new(phys_addr as u64)
                },
                Err(_) => {
                    panic!("Critical failure: Failed to allocate contiguous physical frames for TSS stacks.");
                }
            }
        };

        let tss_stacks = crate::gdt::TssStacks {
            double_fault: VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64() + crate::gdt::GDT_TSS_STACK_SIZE as u64),
            timer: VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64() + (crate::gdt::GDT_TSS_STACK_SIZE * 2) as u64),
        };
        crate::gdt::init_with_stacks(tss_stacks);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: GDT initialized with TSS stacks\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_init] Start mapping 1GB kernel area\n");

        let kernel_virt_start = crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let kernel_phys_start_val = kernel_phys_start.as_u64();

        let mut val_buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(kernel_phys_start_val, &mut val_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: phys_start=0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &val_buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_init] Attempting to lock FRAME_ALLOCATOR for init\n");
        let mut wide_mapper = unsafe {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_init] Lock acquired, calling init\n");
            let allocator = fa_guard.as_mut().expect("Frame allocator should be ready");
            petroleum::page_table::init(self.physical_memory_offset, allocator)
        };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_init] petroleum::page_table::init for wide_mapper returned\n");
        {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let allocator = fa_guard.as_mut().expect("Frame allocator should be ready");
            for i in 0..(256 * 1024) {
                let vaddr = x86_64::VirtAddr::new(kernel_virt_start + i as u64 * 4096);
                let paddr = x86_64::PhysAddr::new(kernel_phys_start_val + i as u64 * 4096);

                let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(vaddr);
                let frame = x86_64::structures::paging::PhysFrame::containing_address(paddr);

                unsafe {
                    let _ = wide_mapper.map_to(
                        page,
                        frame,
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                        allocator,
                    );
                }
            }
            x86_64::instructions::tlb::flush_all();
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Large kernel mapping completed\n");

        debug_log_no_alloc!("Entering memory_management_initialization");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Post-GDT init phase start\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Accessing FULLERENE_FRAMEBUFFER_CONFIG...\n");
        let framebuffer_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|mutex| {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Locking framebuffer config mutex...\n");
                let lock = mutex.lock();
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Framebuffer config mutex locked\n");
                *lock
            });
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Framebuffer config access completed\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to lock FRAME_ALLOCATOR (line 222)\n");
        let config = framebuffer_config.as_ref();
        let (fb_addr, fb_size) = if let Some(config) = config {
            let fb_size_bytes =
                (config.width as usize * config.height as usize * config.bpp as usize) / 8;
            (
                Some(VirtAddr::new(config.address)),
                Some(fb_size_bytes as u64),
            )
        } else {
            (None, None)
        };

        debug_log_no_alloc!("DEBUG: About to lock FRAME_ALLOCATOR for page table setup");
        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: FRAME_ALLOCATOR locked\n");
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");

        let tss_flags = x86_64::structures::paging::PageTableFlags::PRESENT 
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling petroleum::page_table::init (2)...\n");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to init tss_mapper\n");
        let mut tss_mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset, frame_allocator) };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: petroleum::page_table::init (2) done\n");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to map TSS stacks\n");
        unsafe {
            petroleum::map_range_with_log_macro!(
                &mut tss_mapper,
                &mut *frame_allocator,
                tss_phys_addr.as_u64(),
                (crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64) + tss_phys_addr.as_u64(),
                tss_stack_pages as u64,
                tss_flags
            );
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: TSS stacks mapped to higher half\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Starting page table copy test...\n");
        let test_res = petroleum::page_table::test_page_table_copy_switch(
            VirtAddr::zero(),
            &mut *frame_allocator,
            memory_map_ref,
        );
        if let Err(e) = test_res {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Page table copy test FAILED\n");
            debug_log_no_alloc!("Page table copy test failed: ", e as usize);
        } else {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Page table copy test passed\n");
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Setting kernel CR3...\n");
        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Kernel CR3 set\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to set physical memory offset\n");
        crate::memory_management::set_physical_memory_offset(
            crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Physical memory offset set\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to find heap start\n");
        let heap_phys_start = find_heap_start(memory_map_ref);
        let heap_phys_start_addr = if heap_phys_start.as_u64() < 0x1000
            || heap_phys_start.as_u64() >= 0x0000_8000_0000_0000
        {
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            heap_phys_start
        };
        let heap_pages = (heap::HEAP_SIZE + 4095) / 4096;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Allocating contiguous frames for heap...\n");
        
        let heap_phys_addr_val = frame_allocator
            .allocate_contiguous_frames(heap_pages)
            .expect("Failed to allocate contiguous frames for heap");
        
        let heap_phys_addr = PhysAddr::new(heap_phys_addr_val as u64);
        
        let mut addr_buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(heap_phys_addr.as_u64(), &mut addr_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Heap frames allocated at 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &addr_buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling petroleum::page_table::init (3)...\n");
        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset, frame_allocator) };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: petroleum::page_table::init (3) done\n");

        let heap_flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        petroleum::map_pages_to_offset!(
            mapper,
            &mut *frame_allocator,
            heap_phys_addr.as_u64(),
            self.physical_memory_offset.as_u64(),
            heap_pages as u64,
            heap_flags
        );

        self.virtual_heap_start = self.physical_memory_offset + heap_phys_addr.as_u64();
        write_serial_bytes!(0x3F8, 0x3FD, b"Heap allocated and mapped\n");

        use petroleum::page_table::{ALLOCATOR, HEAP_INITIALIZED};
        
        // Ensure heap_start_for_allocator is aligned to 16 bytes to avoid alignment faults
        let raw_start_u64 = self.virtual_heap_start.as_u64() + crate::gdt::GDT_INIT_OVERHEAD as u64;
        let aligned_start_u64 = (raw_start_u64 + 15) & !15;
        let heap_start_for_allocator = VirtAddr::new(aligned_start_u64);
        let heap_size_for_allocator = heap::HEAP_SIZE - (aligned_start_u64 - self.virtual_heap_start.as_u64()) as usize;
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Initializing global allocator...\n");
        unsafe {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: ALLOCATOR.lock().init start\n");
            ALLOCATOR.lock().init(
                heap_start_for_allocator.as_mut_ptr::<u8>(),
                heap_size_for_allocator,
            );
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: ALLOCATOR.lock().init done\n");
        }
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: HEAP_INITIALIZED.call_once start\n");
        HEAP_INITIALIZED.call_once(|| true);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: HEAP_INITIALIZED.call_once done\n");
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: set_heap_range start\n");
        petroleum::common::memory::set_heap_range(
            heap_start_for_allocator.as_u64() as usize,
            heap_size_for_allocator,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: set_heap_range done\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_management_initialization about to return\n");
        
        let res_offset = self.physical_memory_offset;
        let res_phys = heap_phys_addr;
        let res_virt = self.virtual_heap_start;
        
        (res_offset, res_phys, res_virt)
    }

    pub fn prepare_kernel_stack(
        &mut self,
        virtual_heap_start: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) -> u64 {
        log::info!("Setting up kernel stack");
        self.heap_start_after_gdt = virtual_heap_start;

        let stack_phys_start = self.heap_start_after_gdt.as_u64() - physical_memory_offset.as_u64();
        // WIDER STACK MAPPING: Map 2MB instead of just KERNEL_STACK_SIZE to prevent #PF on stack growth
        let stack_pages = (2 * 1024 * 1024) / 4096;

        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");

        let mut mapper = unsafe { petroleum::page_table::init(physical_memory_offset, frame_allocator) };
        let mut mem_mapper = petroleum::page_table::mapper::MemoryMapper::new(
            &mut mapper,
            &mut *frame_allocator,
            physical_memory_offset,
        );

        let stack_flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        
        mem_mapper.map_to_higher_half(stack_phys_start, stack_pages as u64, stack_flags)
            .expect("Failed to map kernel stack to higher half");

        write_serial_bytes!(0x3F8, 0x3FD, b"Kernel stack allocated and mapped (wide)\n");

        let kernel_stack_top =
            (self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64 - 8).as_u64();
        
        self.heap_start_after_stack =
            self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64;
        
        kernel_stack_top
    }

    pub fn setup_allocator(&mut self, virtual_heap_start: VirtAddr) {
        if petroleum::page_table::HEAP_INITIALIZED.get().is_some() {
            return;
        }

        let kernel_overhead =
            (self.heap_start_after_stack.as_u64() - virtual_heap_start.as_u64()) as usize;
        let heap_size_remaining = heap::HEAP_SIZE - kernel_overhead;

        use petroleum::page_table::ALLOCATOR;
        unsafe {
            let mut allocator = ALLOCATOR.lock();
            allocator.init(
                self.heap_start_after_stack.as_mut_ptr::<u8>(),
                heap_size_remaining,
            );
        }
    }

    pub fn map_mmio(&mut self) {
        log::info!("Mapping MMIO regions for APIC and IOAPIC");

        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");

        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset, frame_allocator) };
        let mut mem_mapper = petroleum::page_table::mapper::MemoryMapper::new(
            &mut mapper,
            &mut *frame_allocator,
            self.physical_memory_offset,
        );

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

        let regions = [
            (0xfee00000, 1, "Local APIC"),
            (0xfec00000, 1, "IO APIC"),
            (0xb8000, (0xc0000 - 0xb8000) / 4096, "VGA text buffer"),
        ];

        for (phys, pages, name) in regions {
            if let Err(e) = mem_mapper.map_to_identity(phys, pages, flags) {
                if !matches!(e, MapToError::PageAlreadyMapped(_)) {
                    panic!("Failed to map {}: {:?}", name, e);
                }
            }
            log::info!("{} mapped at identity address {:#x}", name, phys);
        }
        *petroleum::LOCAL_APIC_ADDRESS.lock() =
            petroleum::LocalApicAddress(0xfee00000 as *mut u32);
    }

    fn init_memory_map(&self) {
        debug_log_no_alloc!("!!! ENTERING init_memory_map (FIXED) !!!");

        // The raw_ptr is actually pointing to the 'physical_start' field (offset 8).
        // We must move it back by 8 bytes to reach the 'type' field.
        let raw_ptr = self.memory_map as u64;
        let base_ptr = (raw_ptr.wrapping_sub(8)) as *const u8;
        
        // Force standard x86_64 UEFI descriptor size (48 bytes)
        let descriptor_size = 48;

        debug_log_no_alloc!("Corrected base_ptr: 0x", base_ptr as u64);
        debug_log_no_alloc!("Using forced DESC_SIZE: ", descriptor_size);

        unsafe {
            let mut count = 0;
            for i in 0..crate::heap::MAX_DESCRIPTORS {
                let desc_ptr = base_ptr.add(i * descriptor_size);
                let desc = MemoryMapDescriptor::new(desc_ptr, descriptor_size);
                
                if !petroleum::page_table::MemoryDescriptorValidator::is_valid(&desc) {
                    debug_log_no_alloc!("Stopped parsing at descriptor {} (invalid)", i);
                    break;
                }
                
                crate::heap::MEMORY_MAP_BUFFER[i] = desc;
                count += 1;
            }
            
            debug_log_no_alloc!("Successfully parsed {} descriptors", count);
            *crate::heap::MEMORY_MAP.lock() = Some(&crate::heap::MEMORY_MAP_BUFFER[0..count]);
        }

        debug_log_no_alloc!("!!! INIT_MMAP DONE (FIXED) !!!");
    }
}