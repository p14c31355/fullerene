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

        // WIDE MAPPING: Map the kernel region to avoid #PF during early boot
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Mapping wide higher-half kernel region\n");
        let kernel_virt_start = crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let kernel_phys_start = kernel_phys_start.as_u64();
        let mut wide_mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
        {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let allocator = fa_guard.as_mut().expect("Frame allocator should be ready");
            let map_size_pages = (512 * 1024 * 1024) / 4096; // 512MB
            for i in 0..map_size_pages {
                let v_page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(
                    VirtAddr::new(kernel_virt_start + i * 4096)
                );
                let p_frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
                    PhysAddr::new(kernel_phys_start + i * 4096)
                );
                unsafe {
                    if let Ok(flush) = wide_mapper.map_to(
                        v_page,
                        p_frame,
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                        allocator,
                    ) {
                        flush.flush();
                    }
                }
            }
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Wide higher-half kernel region mapped\n");

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
            let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: petroleum::page_table::init (1) done\n");
            {
                let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
                let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator should be ready now");

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
        
        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");
        debug_log_no_alloc!("DEBUG: Frame allocator lock acquired for TSS");
        
        debug_log_no_alloc!("DEBUG: Attempting to allocate contiguous frames: ", tss_stack_pages);
        let tss_phys_addr = match frame_allocator.allocate_contiguous_frames(tss_stack_pages) {
            Ok(phys_addr) => {
                debug_log_no_alloc!("DEBUG: TSS frames allocated at 0x", phys_addr);
                PhysAddr::new(phys_addr as u64)
            },
            Err(_) => {
                panic!("Critical failure: Failed to allocate contiguous physical frames for TSS stacks.");
            }
        };

        let tss_stacks = crate::gdt::TssStacks {
            double_fault: VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64() + crate::gdt::GDT_TSS_STACK_SIZE as u64),
            timer: VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64() + (crate::gdt::GDT_TSS_STACK_SIZE * 2) as u64),
        };
        crate::gdt::init_with_stacks(tss_stacks);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: GDT initialized with TSS stacks\n");
        
        debug_log_no_alloc!("Entering memory_management_initialization");
        let framebuffer_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|mutex| *mutex.lock());

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

        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");
        debug_log_no_alloc!("DEBUG: Frame allocator lock acquired for page table setup");
        debug_log_no_alloc!("DEBUG: About to map TSS stacks");

        let tss_flags = x86_64::structures::paging::PageTableFlags::PRESENT 
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling petroleum::page_table::init (2)...\n");
        let mut tss_mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: petroleum::page_table::init (2) done\n");
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
        if let Err(e) = petroleum::page_table::test_page_table_copy_switch(
            VirtAddr::zero(),
            &mut *frame_allocator,
            memory_map_ref,
        ) {
            debug_log_no_alloc!("Page table copy test failed: ", e as usize);
        } else {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Page table copy test passed\n");
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Setting kernel CR3...\n");
        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Kernel CR3 set\n");

        crate::memory_management::set_physical_memory_offset(
            crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE,
        );

        let heap_phys_start = find_heap_start(memory_map_ref);
        let heap_phys_start_addr = if heap_phys_start.as_u64() < 0x1000
            || heap_phys_start.as_u64() >= 0x0000_8000_0000_0000
        {
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            heap_phys_start
        };
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Allocating heap physical memory...\n");
        let heap_phys_addr =
            petroleum::allocate_heap_from_map(heap_phys_start_addr, heap::HEAP_SIZE);
        let heap_pages = (heap::HEAP_SIZE + 4095) / 4096;

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Reserving heap frames...\n");
        frame_allocator
            .allocate_frames_at(heap_phys_addr.as_u64() as usize, heap_pages)
            .expect("Failed to reserve heap frames");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling petroleum::page_table::init (3)...\n");
        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
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
        let heap_start_for_allocator =
            self.virtual_heap_start + crate::gdt::GDT_INIT_OVERHEAD as u64;
        let heap_size_for_allocator = heap::HEAP_SIZE - crate::gdt::GDT_INIT_OVERHEAD;
        unsafe {
            ALLOCATOR.lock().init(
                heap_start_for_allocator.as_mut_ptr::<u8>(),
                heap_size_for_allocator,
            );
        }
        HEAP_INITIALIZED.call_once(|| true);
        petroleum::common::memory::set_heap_range(
            heap_start_for_allocator.as_u64() as usize,
            heap_size_for_allocator,
        );

        (
            self.physical_memory_offset,
            heap_phys_addr,
            self.virtual_heap_start,
        )
    }

    pub fn prepare_kernel_stack(
        &mut self,
        virtual_heap_start: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) -> u64 {
        log::info!("Setting up kernel stack");
        self.heap_start_after_gdt = virtual_heap_start;

        let stack_phys_start = self.heap_start_after_gdt.as_u64() - physical_memory_offset.as_u64();
        let stack_pages = (crate::heap::KERNEL_STACK_SIZE + 4095) / 4096;

        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");

        let mut mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
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

        write_serial_bytes!(0x3F8, 0x3FD, b"Kernel stack allocated and mapped\n");

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

        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
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