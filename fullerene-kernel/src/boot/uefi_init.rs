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
    structures::paging::{PageTableFlags, mapper::MapToError},
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
        debug_log_no_alloc!("DEBUG: Starting memory_management_initialization");
        self.init_memory_map();
        debug_log_no_alloc!("DEBUG: init_memory_map completed");
        
        let memory_map_ref = MEMORY_MAP.lock().as_ref().expect("Memory map not initialized").clone();
        debug_log_no_alloc!("DEBUG: Memory map reference acquired at 0x", memory_map_ref.as_ptr() as usize);
        
        heap::init_frame_allocator(memory_map_ref);
        debug_log_no_alloc!("Heap frame allocator initialized");

        debug_log_no_alloc!("DEBUG: Allocating TSS stacks");
        let tss_stack_pages = (crate::gdt::GDT_TSS_STACK_COUNT * crate::gdt::GDT_TSS_STACK_SIZE) / 4096;
        
        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("Frame allocator not initialized");
        debug_log_no_alloc!("DEBUG: Frame allocator lock acquired for TSS");
            
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
        debug_log_no_alloc!("DEBUG: GDT initialized with TSS stacks");
        
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

        self.physical_memory_offset = x86_64::VirtAddr::new(crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64);

        let tss_flags = x86_64::structures::paging::PageTableFlags::PRESENT 
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        let mut tss_mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };
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
        debug_log_no_alloc!("TSS stacks mapped to higher half");

        if let Err(e) = petroleum::page_table::test_page_table_copy_switch(
            VirtAddr::zero(),
            &mut *frame_allocator,
            memory_map_ref,
        ) {
            debug_log_no_alloc!("Page table copy test failed: ", e as usize);
        } else {
            debug_log_no_alloc!("Page table copy test passed");
        }

        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());

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
        let heap_phys_addr =
            petroleum::allocate_heap_from_map(heap_phys_start_addr, heap::HEAP_SIZE);
        let heap_pages = (heap::HEAP_SIZE + 4095) / 4096;

        frame_allocator
            .allocate_frames_at(heap_phys_addr.as_u64() as usize, heap_pages)
            .expect("Failed to reserve heap frames");

        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };

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
        let phys_ptr = self.memory_map as u64;
        let offset = crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let raw_ptr = phys_ptr + offset;
        
        unsafe {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: RAW - Memory Map Info:\n");
            let mut buf = [0u8; 16];
            
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"  ptr: 0x");
            let len = petroleum::serial::format_hex_to_buffer(raw_ptr, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b" size: 0x");
            let len = petroleum::serial::format_hex_to_buffer(self.memory_map_size as u64, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");

            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: RAW - Memory Map Dump (first 64 bytes):\n");
            for i in 0..8 {
                let val = core::ptr::read_volatile((raw_ptr + i * 8) as *const u64);
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"  [");
                let len = petroleum::serial::format_hex_to_buffer(val, &mut buf, 16);
                petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"] ");
                if (i + 1) % 4 == 0 {
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
                }
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        }

        let kernel_args = unsafe { petroleum::page_table::mapper::transition::KERNEL_ARGS };
        if kernel_args.is_null() {
            unsafe { petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: ERROR - KERNEL_ARGS is null!\n"); }
            return;
        }
        let descriptor_item_size = unsafe { (*kernel_args).descriptor_size };
        
        // Adaptive offset: If the first 8 bytes are within a reasonable descriptor size range (40-64), skip them.
        let mut base_ptr = raw_ptr as *const u8;
        unsafe {
            let first_val = core::ptr::read_volatile(raw_ptr as *const usize);
            if first_val >= 40 && first_val <= 64 {
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: Detected descriptor_size at head, skipping 8 bytes\n");
                base_ptr = base_ptr.add(core::mem::size_of::<usize>());
            } else {
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: No descriptor_size at head, starting from 0\n");
            }
        }

        unsafe {
            let mut buf = [0u8; 16];
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: Using descriptor_size from KERNEL_ARGS: 0x");
            let len = petroleum::serial::format_hex_to_buffer(descriptor_item_size as u64, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        }

        let config_size = core::mem::size_of::<ConfigWithMetadata>();
        let has_config = if self.memory_map_size >= config_size {
            unsafe {
                let ptr = base_ptr.add(self.memory_map_size - config_size)
                    as *const ConfigWithMetadata;
                !ptr.is_null() && (*ptr).magic == FRAMEBUFFER_CONFIG_MAGIC
            }
        } else {
            false
        };
        let actual_descriptors_size = self.memory_map_size
            .saturating_sub(if has_config { config_size } else { 0 });
        let descriptors_base = base_ptr;
        let num_descriptors = actual_descriptors_size / descriptor_item_size;
        
        unsafe {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: RAW - num_descriptors: 0x");
            let mut buf = [0u8; 16];
            let len = petroleum::serial::format_hex_to_buffer(num_descriptors as u64, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        }

        let actual_num = num_descriptors.min(crate::heap::MAX_DESCRIPTORS);
        unsafe {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Starting MEMORY_MAP_BUFFER fill\n");
            for i in 0..actual_num {
                let desc_ptr = descriptors_base.add(i * descriptor_item_size);
                crate::heap::MEMORY_MAP_BUFFER[i] =
                    MemoryMapDescriptor::new(desc_ptr, descriptor_item_size);
                if i % 10 == 0 {
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b".");
                }
            }
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\nDEBUG: MEMORY_MAP_BUFFER fill done\n");
            
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Setting MEMORY_MAP via lock\n");
            *crate::heap::MEMORY_MAP.lock() = Some(&crate::heap::MEMORY_MAP_BUFFER[0..actual_num]);
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: MEMORY_MAP set successfully\n");
        }
        unsafe { petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: RAW - MEMORY_MAP initialized\n"); }
    }
}