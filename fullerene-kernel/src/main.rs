#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

pub(crate) mod font;
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
// mod serial; // Removed, now using petroleum
mod vga;

extern crate alloc;

use core::panic::PanicInfo;

use petroleum::serial::{SERIAL_PORT_WRITER as SERIAL1, serial_init, serial_log};

use core::ffi::c_void;
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;
use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::hlt;

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($msg:expr) => {
        serial_log(concat!($msg, "\n"));
    };
}

// Helper function to find framebuffer config
fn find_framebuffer_config(system_table: &EfiSystemTable) -> Option<&FullereneFramebufferConfig> {
    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };
    for entry in config_table_entries {
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

// Helper function to find heap start from memory map
fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> x86_64::PhysAddr {
    // First, try to find EfiLoaderData
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiLoaderData && desc.number_of_pages > 0 {
            return x86_64::PhysAddr::new(desc.physical_start);
        }
    }
    // If not found, find EfiConventionalMemory large enough
    let required_pages = (heap::HEAP_SIZE + 4095) / 4096;
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiConventionalMemory
            && desc.number_of_pages >= required_pages as u64
        {
            return x86_64::PhysAddr::new(desc.physical_start);
        }
    }
    panic!("No suitable memory region found for heap");
}

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    petroleum::handle_panic(info)
}

#[cfg(target_os = "uefi")]
#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut c_void,
    _memory_map: *mut c_void,
    _memory_map_size: usize,
) -> ! {
    // Early debug print to confirm kernel entry point is reached using direct port access
    use x86_64::instructions::port::Port;
    let mut port = Port::new(0x3F8);
    unsafe {
        let msg = b"Kernel: efi_main entered.\n";
        for &byte in msg {
            while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
            port.write(byte);
        }
    }

    // Helper function for kernel debug prints
    fn print_kernel(msg: &str) {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(0x3F8);
        for byte in msg.bytes() {
            unsafe {
                while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
                port.write(byte);
            }
        }
    }

    print_kernel("Kernel: starting to parse parameters.\n");

    // Verify our own address as sanity check for PE relocation
    let self_addr = efi_main as u64;
    serial_log("Kernel: efi_main located at ");
    serial_log(&alloc::format!("{:#x}", self_addr));

    // Initialize serial immediately after entry (before any complex initialization)
    serial_init();
    print_kernel("Kernel: serial_init done.\n");

    // Confirm ExitBootServices has been called by bootloader (it should have)
    // The bootloader already calls ExitBootServices before jumping to kernel
    kernel_log!("UEFI ExitBootServices assumed called by bootloader");

    // Debugging: print CR3 before page table operations
    use x86_64::registers::control::Cr3;
    kernel_log!("CR3 before page table init:");
    let cr3_before = unsafe { Cr3::read() };
    serial_log("  CR3: ");
    serial_log(&alloc::format!("{:#x}", cr3_before.0.start_address().as_u64()));

    // Initialize GDT after page tables are initialized, but before heap operations
    // For now, keep temp heap - we'll reinitialize later with proper heap
    let temp_heap_start = VirtAddr::new(0x1000000); // Use 16MB temporarily
    let temp_heap_start = gdt::init(temp_heap_start);
    print_kernel("Kernel: GDT init done (temp).\n");

    // Initialize IDT early with exception handlers
    interrupts::init();
    print_kernel("Kernel: IDT init done.\n");

    // Early serial log works now
    kernel_log!("Kernel: basic init complete");

    // Reinitialize page table after exit boot services
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            _memory_map as *const EfiMemoryDescriptor,
            _memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    MEMORY_MAP.call_once(|| unsafe { &*(descriptors as *const _) });

    // Calculate physical_memory_offset from kernel's location in memory map
    let kernel_virt_addr = efi_main as u64;
    let mut physical_memory_offset = VirtAddr::new(0);
    let mut kernel_phys_start = x86_64::PhysAddr::new(0);

    // Find physical_memory_offset and kernel_phys_start
    for desc in descriptors {
        let virt_start = desc.virtual_start;
        let virt_end = virt_start + desc.number_of_pages * 4096;
        if kernel_virt_addr >= virt_start && kernel_virt_addr < virt_end {
            physical_memory_offset = VirtAddr::new(desc.virtual_start - desc.physical_start);
            if desc.type_ == EfiMemoryType::EfiLoaderCode {
                kernel_phys_start = x86_64::PhysAddr::new(desc.physical_start);
            }
        }
    }

    if kernel_phys_start.is_null() {
        panic!("Could not determine kernel's physical start address.");
    }

    print_kernel("Kernel: phys offset found.\n");
    kernel_log!("Kernel: memory map parsed, kernel_phys_start found");
    kernel_log!("Starting heap frame allocator init...");

    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    print_kernel("Kernel: frame allocator init done.\n");
    heap::init_page_table(physical_memory_offset);
    print_kernel("Kernel: page table init done.\n");

    // For UEFI, skip reinit_page_table as UEFI already has proper page tables
    // Only do reinit for BIOS mode if needed
    #[cfg(not(target_os = "uefi"))]
    {
        kernel_log!("Kernel: page table init complete, starting reinit...");
        heap::reinit_page_table(physical_memory_offset, kernel_phys_start);
        print_kernel("Kernel: page table reinit done.\n");
        kernel_log!("Kernel: page table reinitialization complete");
    }
    #[cfg(target_os = "uefi")]
    {
        kernel_log!("Kernel: UEFI mode - using UEFI page tables without reinit");
        // Print CR3 after init to compare
        use x86_64::registers::control::Cr3;
        let cr3_after = unsafe { Cr3::read() };
        serial_log("  CR3 after init: ");
        serial_log(&alloc::format!("{:#x}", cr3_after.0.start_address().as_u64()));
    }

    // Common initialization for both UEFI and BIOS
    init_common(_memory_map, _memory_map_size);
    print_kernel("Kernel: init_common done.\n");

    kernel_log!("Kernel: efi_main entered (via serial_log).");
    kernel_log!("GDT initialized.");
    kernel_log!("IDT initialized.");
    kernel_log!("APIC initialized.");
    kernel_log!("Heap initialized.");
    kernel_log!("Serial initialized.");

    kernel_log!("Entering efi_main...");
    kernel_log!("Searching for framebuffer config table...");

    // Cast the system_table pointer to the correct type
    let system_table = unsafe { &*(system_table as *const EfiSystemTable) };

    if let Some(config) = find_framebuffer_config(system_table) {
        if config.address == 0 {
            panic!("Framebuffer address 0 - check bootloader GOP install");
        } else {
            let _ = core::fmt::write(
                &mut *SERIAL1.lock(),
                format_args!("  Address: {:#x}\n", config.address),
            );
            print_kernel("Kernel: about to init graphics.\n");
            graphics::init(config);
            print_kernel("Kernel: graphics init done.\n");
            kernel_log!("Graphics initialized.");
        }
    } else {
        kernel_log!("Fullerene Framebuffer Config Table not found.");
    }

    // Also initialize VGA text mode for reliable output
    vga::vga_init();
    print_kernel("Kernel: VGA init done.\n");

    // Main loop
    print_kernel("Kernel: about to print hello.\n");
    println!("Hello QEMU by FullereneOS");
    hlt_loop();
}

#[cfg(target_os = "uefi")]
fn init_common(_memory_map: *mut c_void, _memory_map_size: usize) {
    let descriptors = *MEMORY_MAP.get().unwrap();
    let heap_phys_start = find_heap_start(descriptors);
    let heap_start = heap::allocate_heap_from_map(heap_phys_start, heap::HEAP_SIZE);
    let heap_start = gdt::init(heap_start); // Initialize GDT with proper heap location
    heap::init(heap_start, heap::HEAP_SIZE); // Initialize heap

    kernel_log!("Kernel: heap and GDT fully initialized");

    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    kernel_log!("Kernel: APIC initialized and interrupts enabled");

    // Test interrupt handling - should not panic or crash if APIC is working
    kernel_log!("Testing interrupt handling with int3...");
    unsafe { x86_64::instructions::interrupts::int3(); }
    kernel_log!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
fn init_common() {
    use core::mem::MaybeUninit;
    use x86_64::VirtAddr;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; heap::HEAP_SIZE] = [MaybeUninit::uninit(); heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        heap::ALLOCATOR.lock().init(heap_start_ptr, heap::HEAP_SIZE);
    }

    gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    serial_init(); // Initialize serial early for debugging
}

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use petroleum::common::VgaFramebufferConfig;

    init_common();
    kernel_log!("Entering _start...");

    // Graphics initialization for VGA framebuffer (graphics mode)
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    kernel_log!("VGA graphics mode initialized.");

    // Main loop
    println!("Hello QEMU by FullereneOS");
    hlt_loop();
}

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}
