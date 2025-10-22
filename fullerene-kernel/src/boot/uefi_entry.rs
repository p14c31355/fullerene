// Use crate imports
use crate::scheduler::scheduler_loop;

use crate::MEMORY_MAP;
use crate::graphics::framebuffer::FramebufferLike;
use crate::heap;

use crate::memory::find_heap_start;
use crate::{gdt, graphics, interrupts, memory};
use alloc::boxed::Box;
use core::ffi::c_void;
use petroleum::common::EfiGraphicsOutputProtocol;
use petroleum::common::uefi::{efi_print, find_gop_framebuffer, write_vga_string};
use petroleum::common::{EfiSystemTable, FullereneFramebufferConfig};
use petroleum::{allocate_heap_from_map, debug_log, debug_log_no_alloc, write_serial_bytes};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Mapper, Size4KiB, mapper::MapToError},
};

/// Virtual heap start offset from physical memory offset
#[cfg(target_os = "uefi")]
const VIRTUAL_HEAP_START_OFFSET: u64 = 0x100000;

/// Helper function to map a range of memory pages
/// Takes the base physical address, number of pages, mapper, frame allocator, and flags
#[cfg(target_os = "uefi")]
fn map_memory_range(
    base_phys_addr: PhysAddr,
    num_pages: u64,
    physical_memory_offset: VirtAddr,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    for i in 0..num_pages {
        let phys_addr_u64 = base_phys_addr.as_u64() + (i * 4096);
        let phys_addr = PhysAddr::new(phys_addr_u64);
        let virt_addr = physical_memory_offset + phys_addr_u64;

        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt_addr);
        let frame =
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(phys_addr);

        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        }
    }
    Ok(())
}

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
        debug_log_no_alloc!("Kernel: efi_main entered");
        petroleum::serial::serial_init();
        debug_log_no_alloc!("Kernel: efi_main located at ", efi_main as usize);

        // UEFI uses framebuffer graphics, not legacy VGA hardware programming
        // Graphics initialization happens later with initialize_graphics_with_config()
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 0, b"Kernel boot (UEFI)", 0x1F00);
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
        );

        // Basic setup without full page table reinit
        debug_log_no_alloc!("Basic memory setup without page table reinit complete");
        #[cfg(feature = "verbose_boot_log")]
        write_serial_bytes!(0x3F8, 0x3FD, b"page table reinit completed\n");
        debug_log_no_alloc!("Page table reinit completed");

        // Set kernel CR3 - CRITICAL: this might cause issues if page table has wrong mappings
        debug_log_no_alloc!("About to read kernel CR3");
        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        debug_log_no_alloc!("Kernel CR3 read: ", kernel_cr3.0.start_address().as_u64() as usize);
        debug_log_no_alloc!("About to set kernel CR3 in syscall");
        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
        debug_log_no_alloc!("Kernel CR3 set in syscall");
        #[cfg(feature = "verbose_boot_log")]
        write_serial_bytes!(0x3F8, 0x3FD, b"About to set physical memory offset\n");

        // Set physical memory offset - CRITICAL: this changes virtual address calculation
        debug_log_no_alloc!("About to set physical memory offset to ", crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE as usize);
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
        write_serial_bytes!(0x3F8, 0x3FD, b"Skipping heap allocation temporarily for debugging\n");

        // TODO: Re-enable heap allocation once basic scheduler is working
        // Use simple fallback for now
        self.virtual_heap_start = self.physical_memory_offset + VIRTUAL_HEAP_START_OFFSET;
        write_serial_bytes!(0x3F8, 0x3FD, b"Simple heap fallback set\n");

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
        debug_log_no_alloc!("Basic heap allocator initialized");
        write_serial_bytes!(0x3F8, 0x3FD, b"Basic heap allocator initialized\n");

        (
            self.physical_memory_offset,
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR),
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

        // Switch to the kernel stack
        unsafe {
            let kernel_stack_top = self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64 - 8;
            core::arch::asm!("mov rsp, {}", in(reg) kernel_stack_top);
        }
        self.heap_start_after_stack = self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64;
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

    // Now that allocator is set up, initialize VGA buffer writer (will be done in init_common)

    // Early serial log works now
    write_serial_bytes!(0x3F8, 0x3FD, b"About to complete basic init\n");
    petroleum::serial::serial_log(format_args!("About to log basic init complete...\n"));
    log::info!("Kernel: basic init complete");
    write_serial_bytes!(0x3F8, 0x3FD, b"Basic init complete logged\n");
    petroleum::serial::serial_log(format_args!("basic init complete logged successfully\n"));

    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: About to init interrupts\n");

    // Common initialization for both UEFI and BIOS
    // Initialize IDT before enabling interrupts
    interrupts::init();
    log::info!("Kernel: IDT init done");

    log::info!("Kernel: Jumping straight to graphics testing");

    log::info!("About to call init_common");
    // Initialize interrupts and other components call init_common here
    crate::init::init_common(physical_memory_offset);
    log::info!("init_common completed");

    // Initialize graphics with framebuffer configuration
    log::info!("Initialize graphics with framebuffer configuration");
    let success = petroleum::initialize_graphics_with_config();
    log::info!("Graphics initialization result: {}", success);
    if !success {
        log::info!("Graphics initialization failed, continuing without graphics for debugging");
    }

    // Always enable interrupts and proceed to scheduler
    log::info!("Enabling interrupts...");
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // Initialize keyboard input driver
    crate::keyboard::init();
    log::info!("Keyboard initialized");

    // Initialize process management system
    crate::process::init();
    log::info!("Process management initialized");

    // Start the main kernel scheduler that orchestrates all system functionality
    log::info!("Starting full system scheduler...");
    write_serial_bytes!(0x3F8, 0x3FD, b"About to enter scheduler_loop\n");
    scheduler_loop();
    // scheduler_loop should never return in normal operation
    panic!("scheduler_loop returned unexpectedly - kernel critical failure!");
}

// Moved graphics initialization functions to petroleum::uefi_helpers
