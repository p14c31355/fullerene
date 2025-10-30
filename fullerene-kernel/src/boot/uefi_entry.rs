// Use crate imports
use crate::scheduler::scheduler_loop;

use crate::MEMORY_MAP;
use petroleum::FramebufferLike;
use crate::heap;

use crate::memory::find_heap_start;
use crate::{gdt, graphics, interrupts, memory};
use alloc::boxed::Box;
use core::ffi::c_void;
use petroleum::common::{EfiGraphicsOutputProtocol, EfiSystemTable};
use petroleum::common::uefi::{efi_print, find_gop_framebuffer, write_vga_string};

use petroleum::{
    allocate_heap_from_map, debug_log, debug_log_no_alloc, mem_debug, write_serial_bytes,
};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB, mapper::MapToError},
};

/// Virtual heap start offset from physical memory offset
#[cfg(target_os = "uefi")]
const VIRTUAL_HEAP_START_OFFSET: u64 = crate::memory_management::VIRTUAL_HEAP_START_OFFSET;

/// Helper struct for UEFI initialization context
#[cfg(target_os = "uefi")]
struct UefiInitContext {
    /// Reference to EFI system table
    system_table: &'static EfiSystemTable,
    /// Physical memory offset after page table reconfiguration
    physical_memory_offset: VirtAddr,
    /// Virtual heap start address
    virtual_heap_start: VirtAddr,
    /// Heap start after GDT allocation
    heap_start_after_gdt: VirtAddr,
    /// Heap start after stack allocation
    heap_start_after_stack: VirtAddr,
}

#[cfg(target_os = "uefi")]
impl UefiInitContext {
    fn new(system_table: &'static EfiSystemTable) -> Self {
        Self {
            system_table,
            physical_memory_offset: VirtAddr::new(0),
            virtual_heap_start: VirtAddr::new(0),
            heap_start_after_gdt: VirtAddr::new(0),
            heap_start_after_stack: VirtAddr::new(0),
        }
    }

    /// Early initialization: serial, VGA, memory maps
    fn early_initialization(
        &mut self,
        _memory_map: *mut c_void,
        memory_map_size: usize,
    ) -> PhysAddr {
        mem_debug!("Kernel: efi_main entered\n");
        petroleum::serial::serial_init();
        mem_debug!("Kernel: efi_main located at ", efi_main as usize, "\n");

        // UEFI uses framebuffer graphics, not legacy VGA hardware programming
        // Graphics initialization happens later with initialize_graphics_with_config()
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 0, b"Kernel boot (UEFI)", 0x1F00);
            write_vga_string(vga_buffer, 1, b"Early init start", 0x1F00);
        }
        petroleum::serial::serial_init();
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 2, b"Serial init done", 0x1F00);
        }
        write_serial_bytes!(0x3F8, 0x3FD, b"Early setup completed\n");

        let kernel_virt_addr = efi_main as u64;
        crate::memory::setup_memory_maps(_memory_map, memory_map_size, kernel_virt_addr)
    }

    fn memory_management_initialization(
        &mut self,
        kernel_phys_start: PhysAddr,
        system_table: &EfiSystemTable,
    ) -> (VirtAddr, PhysAddr, VirtAddr) {
        debug_log_no_alloc!("Entering memory_management_initialization");
        let memory_map_ref = MEMORY_MAP.get().expect("Memory map not initialized");
        // Get framebuffer config from petroleum global
        let framebuffer_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|mutex| *mutex.lock());
        if framebuffer_config.is_some() {
            debug_log_no_alloc!("Framebuffer config found");
        } else {
            debug_log_no_alloc!("No framebuffer config found");
        }

        let config = framebuffer_config.as_ref();
        let (fb_addr, fb_size) = if let Some(config) = config {
            let fb_size_bytes =
                (config.width as usize * config.height as usize * config.bpp as usize) / 8;
            debug_log_no_alloc!("Found framebuffer config width=", config.width as usize);
            debug_log_no_alloc!("Found framebuffer config height=", config.height as usize);
            debug_log_no_alloc!("Found framebuffer config address=", config.address);
            (
                Some(VirtAddr::new(config.address)),
                Some(fb_size_bytes as u64),
            )
        } else {
            debug_log_no_alloc!("No framebuffer config found");
            (None, None)
        };

        // Initialize a basic frame allocator first
        // Initialize heap frame allocator
        heap::init_frame_allocator(*memory_map_ref);
        debug_log_no_alloc!("Heap frame allocator initialized");
        let mut frame_allocator = crate::heap::FRAME_ALLOCATOR
            .get()
            .expect("Frame allocator not initialized")
            .lock();

        // Initialize page table with higher half mappings for UEFI
        self.physical_memory_offset = petroleum::page_table::reinit_page_table_with_allocator(
            kernel_phys_start,
            fb_addr,
            fb_size,
            &mut frame_allocator,
            *memory_map_ref,
            x86_64::VirtAddr::new(0),
        );

        // Basic setup without full page table reinit
        debug_log_no_alloc!("Basic memory setup without page table reinit complete");
        #[cfg(feature = "verbose_boot_log")]
        write_serial_bytes!(0x3F8, 0x3FD, b"page table reinit completed\n");
        debug_log_no_alloc!("Page table reinit completed");

        // Set kernel CR3 - CRITICAL: this might cause issues if page table has wrong mappings
        debug_log_no_alloc!("About to read kernel CR3");
        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        debug_log_no_alloc!(
            "Kernel CR3 read: ",
            kernel_cr3.0.start_address().as_u64() as usize
        );
        debug_log_no_alloc!("About to set kernel CR3 in syscall");
        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
        debug_log_no_alloc!("Kernel CR3 set in syscall");
        #[cfg(feature = "verbose_boot_log")]
        write_serial_bytes!(0x3F8, 0x3FD, b"About to set physical memory offset\n");

        // Set physical memory offset - CRITICAL: this changes virtual address calculation
        debug_log_no_alloc!(
            "About to set physical memory offset to ",
            crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as usize
        );
        crate::memory_management::set_physical_memory_offset(
            crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE,
        );
        debug_log_no_alloc!("Physical memory offset set successfully");
        #[cfg(feature = "verbose_boot_log")]
        write_serial_bytes!(0x3F8, 0x3FD, b"physical memory offset set\n");

        let heap_phys_start = find_heap_start(*memory_map_ref);
        let heap_phys_start_addr = if heap_phys_start.as_u64() < 0x1000
            || heap_phys_start.as_u64() >= 0x0000_8000_0000_0000
        {
            debug_log_no_alloc!("Invalid heap_phys_start, using fallback heap address");
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            heap_phys_start
        };
        // Allocate and map heap memory
        let heap_phys_addr =
            petroleum::allocate_heap_from_map(heap_phys_start_addr, heap::HEAP_SIZE);
        let heap_pages = (heap::HEAP_SIZE + 4095) / 4096;

        // Reserve heap memory region in frame allocator to prevent corruption
        frame_allocator
            .allocate_frames_at(heap_phys_addr.as_u64() as usize, heap_pages)
            .expect("Failed to reserve heap frames");

        // Create mapper for heap allocation
        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };

        // Map heap to higher half
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

        // Simple heap allocator init without advanced mapping
        debug_log_no_alloc!("Initializing basic heap allocator");
        use petroleum::page_table::{ALLOCATOR, HEAP_INITIALIZED};

        debug_log_no_alloc!("Using basic heap allocator init");

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
        // Set heap range for page fault detection
        petroleum::common::memory::set_heap_range(
            heap_start_for_allocator.as_u64() as usize,
            heap_size_for_allocator,
        );
        debug_log_no_alloc!("Basic heap allocator initialized");
        write_serial_bytes!(0x3F8, 0x3FD, b"Basic heap allocator initialized\n");

        (
            self.physical_memory_offset,
            heap_phys_addr,
            self.virtual_heap_start,
        )
    }

    fn setup_gdt_and_stack(
        &mut self,
        virtual_heap_start: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) {
        log::info!("Setting up GDT and kernel stack");
        let gdt_heap_start = virtual_heap_start;
        self.heap_start_after_gdt = gdt::init(gdt_heap_start);
        log::info!("GDT initialized");

        // Allocate and map kernel stack before switching
        let stack_phys_start = self.heap_start_after_gdt.as_u64() - physical_memory_offset.as_u64();
        let stack_pages = (crate::heap::KERNEL_STACK_SIZE + 4095) / 4096;

        let mut frame_allocator = crate::heap::FRAME_ALLOCATOR
            .get()
            .expect("Frame allocator not initialized")
            .lock();

        let mut mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };

        let stack_flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        petroleum::map_pages_to_offset!(
            mapper,
            &mut *frame_allocator,
            stack_phys_start,
            physical_memory_offset.as_u64(),
            stack_pages as u64,
            stack_flags
        );

        write_serial_bytes!(0x3F8, 0x3FD, b"Kernel stack allocated and mapped\n");

        // Switch to the kernel stack
        unsafe {
            let kernel_stack_top =
                (self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64 - 8).as_u64();
            core::arch::asm!("mov rsp, {}", in(reg) kernel_stack_top);
        }
        self.heap_start_after_stack =
            self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64;
        write_serial_bytes!(0x3F8, 0x3FD, b"Basic GDT and stack setup completed\n");
    }

    fn setup_allocator(&mut self, virtual_heap_start: VirtAddr) {
        // Check if heap was already initialized early in memory_management_initialization
        if petroleum::page_table::HEAP_INITIALIZED.get().is_some() {
            log::info!("Heap allocator already initialized early, skipping second initialization");
            return;
        }

        petroleum::serial::serial_log(format_args!("Initializing allocator\n"));
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
        log::info!("Allocator initialized");
    }

    fn map_mmio(&mut self) {
        log::info!("Mapping MMIO regions for APIC and IOAPIC");

        let mut frame_allocator = crate::heap::FRAME_ALLOCATOR
            .get()
            .expect("Frame allocator not initialized")
            .lock();

        let mut mapper = unsafe { petroleum::page_table::init(self.physical_memory_offset) };

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

        const LOCAL_APIC_ADDR: u64 = 0xfee00000;
        const IO_APIC_ADDR: u64 = 0xfec00000;
        const VGA_TEXT_BUFFER_ADDR: u64 = 0xb8000;

        // Map Local APIC at identity address 0xfee00000
        let apic_phys = PhysAddr::new(LOCAL_APIC_ADDR);
        let apic_virt = VirtAddr::new(LOCAL_APIC_ADDR);
        let page = Page::<Size4KiB>::containing_address(apic_virt);
        let frame = PhysFrame::<Size4KiB>::containing_address(apic_phys);
        unsafe {
            match mapper.map_to(page, frame, flags, &mut *frame_allocator) {
                Ok(flush) => flush.flush(),
                Err(MapToError::PageAlreadyMapped(_)) => {},
                Err(e) => panic!("Failed to map Local APIC: {:?}", e),
            }
        }
        *petroleum::LOCAL_APIC_ADDRESS.lock() =
            petroleum::LocalApicAddress(LOCAL_APIC_ADDR as *mut u32);
        log::info!("LOCAL APIC mapped at virt {:#x}", apic_virt.as_u64());

        // Map IO APIC at identity address 0xfec00000
        let io_apic_phys = PhysAddr::new(IO_APIC_ADDR);
        let io_apic_virt = VirtAddr::new(IO_APIC_ADDR);
        let page = Page::<Size4KiB>::containing_address(io_apic_virt);
        let frame = PhysFrame::<Size4KiB>::containing_address(io_apic_phys);
        unsafe {
            match mapper.map_to(page, frame, flags, &mut *frame_allocator) {
                Ok(flush) => flush.flush(),
                Err(MapToError::PageAlreadyMapped(_)) => {},
                Err(e) => panic!("Failed to map IO APIC: {:?}", e),
            }
        }
        log::info!("IO APIC mapped at virt {:#x}", io_apic_virt.as_u64());

        // Map VGA text buffer (0xB8000-0xC0000) for compatibility
        let vga_pages_size = (0xc0000 - VGA_TEXT_BUFFER_ADDR) / 4096; // 8 pages (32KB)
        for i in 0..vga_pages_size {
            let vga_phys = PhysAddr::new(VGA_TEXT_BUFFER_ADDR + i * 4096);
            let vga_virt = VirtAddr::new(VGA_TEXT_BUFFER_ADDR + i * 4096);
            let page = Page::<Size4KiB>::containing_address(vga_virt);
            let frame = PhysFrame::<Size4KiB>::containing_address(vga_phys);
            unsafe {
                match mapper.map_to(page, frame, flags, &mut *frame_allocator) {
                    Ok(flush) => flush.flush(),
                    Err(MapToError::PageAlreadyMapped(_)) => {},
                    Err(e) => panic!("Failed to map VGA buffer page {}: {:?}", i, e),
                }
            }
        }
        log::info!(
            "VGA text buffer mapped at identity address {:#x}",
            VGA_TEXT_BUFFER_ADDR
        );
    }
}

#[cfg(target_os = "uefi")]
#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut c_void,
    memory_map_size: usize,
) -> ! {
    let system_table = unsafe { &*system_table };
    let mut ctx = UefiInitContext::new(system_table);

    let kernel_phys_start = ctx.early_initialization(memory_map, memory_map_size);
    let (physical_memory_offset, heap_start, virtual_heap_start) =
        ctx.memory_management_initialization(kernel_phys_start, system_table);

    ctx.setup_gdt_and_stack(virtual_heap_start, physical_memory_offset);
    write_serial_bytes!(0x3F8, 0x3FD, b"GDT and stack setup completed\n");
    ctx.setup_allocator(virtual_heap_start);
    write_serial_bytes!(0x3F8, 0x3FD, b"Allocator setup completed\n");

    // Initialize the global memory manager with the EFI memory map
    log::info!("Initializing global memory manager...");
    write_serial_bytes!(0x3F8, 0x3FD, b"Calling MEMORY_MAP.get()\n");
    if let Some(memory_map) = MEMORY_MAP.get() {
        write_serial_bytes!(0x3F8, 0x3FD, b"MEMORY_MAP.get() succeeded\n");
        if let Err(e) = crate::memory_management::init_memory_manager(memory_map) {
            log::error!(
                "Failed to initialize global memory manager: {:?}. Halting.",
                e
            );
            petroleum::halt_loop();
        }
        petroleum::set_memory_initialized(true);
        log::info!("Memory management initialized and marked as ready");
    } else {
        log::error!("MEMORY_MAP not initialized. Cannot initialize memory manager. Halting.");
        petroleum::halt_loop();
    }

    // Common initialization for both UEFI and BIOS with correct physical memory offset
    log::info!("About to call init_common");
    crate::init::init_common(physical_memory_offset);
    log::info!("init_common completed");

    write_serial_bytes!(0x3F8, 0x3FD, b"About to complete basic init\n");
    petroleum::serial::serial_log(format_args!("About to log basic init complete...\n"));
    log::info!("Kernel: basic init complete");
    write_serial_bytes!(0x3F8, 0x3FD, b"Basic init complete logged\n");
    petroleum::serial::serial_log(format_args!("basic init complete logged successfully\n"));

    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: About to init interrupts\n");

    // Initialize IDT before enabling interrupts
    interrupts::init();
    log::info!("Kernel: IDT init done");

    // Map MMIO regions
    ctx.map_mmio();
    log::info!("MMIO mapping completed");

    // Initialize VGA for UEFI
    crate::vga::init_vga(physical_memory_offset);
    log::info!("VGA initialized for UEFI");

    // Always enable interrupts and proceed to scheduler
    log::info!("Enabling interrupts...");
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // After enabling interrupts, initialize APIC
    crate::interrupts::init_apic();
    log::info!("APIC initialized");

    // Initialize keyboard input driver
    // crate::keyboard::init();
    log::info!("Keyboard initialization skipped");

    // Initialize process management system
    // crate::process::init(); // Already called in init_common
    log::info!("Process management initialized");

    // Initialize graphics protocols using petroleum
    log::info!("Skipping graphics protocols initialization for now");
    // let _ = petroleum::init_graphics_protocols(system_table);

    // Initialize text/graphics output if framebuffer config is available
    if let Some(config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().map(|m| *m.lock()).flatten() {
        log::info!("Initializing framebuffer text output");
        crate::graphics::text::init(&config);
    } else {
        log::info!("No framebuffer config found, skipping fallback VGA graphics for now");
        // let _ = crate::graphics::text::init_fallback_graphics();
    }

    // Start the main kernel scheduler that orchestrates all system functionality
    log::info!("Starting full system scheduler...");
    write_serial_bytes!(0x3F8, 0x3FD, b"About to enter scheduler_loop\n");
    scheduler_loop();
    // scheduler_loop should never return in normal operation
    panic!("scheduler_loop returned unexpectedly - kernel critical failure!");
}

// Moved graphics initialization functions to petroleum::uefi_helpers
