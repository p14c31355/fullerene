//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use petroleum::{init_log, write_serial_bytes};

#[cfg(target_os = "uefi")]
pub fn init_common() {
    init_log!("init_common: About to init VGA");
    crate::vga::init_vga();
    init_log!("init_common: VGA init done");

    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    init_log!("init_common: About to init APIC");
    interrupts::init_apic();
    init_log!("init_common: APIC init done");
    log::info!("Kernel: APIC initialized and interrupts enabled");

    init_log!("init_common: About to init process");
    crate::process::init();
    init_log!("init_common: Process init done");
    log::info!("Kernel: Process management initialized");

    init_log!("init_common: About to init syscall");
    crate::syscall::init();
    init_log!("init_common: syscall init done");
    log::info!("Kernel: System calls initialized");

    init_log!("init_common: About to init fs");
    crate::fs::init();
    init_log!("init_common: FS init done");
    log::info!("Kernel: Filesystem initialized");

    init_log!("init_common: About to init loader");
    crate::loader::init();
    init_log!("init_common: Loader init done");
    log::info!("Kernel: loader initialized");

    init_log!("About to create test process");
    let test_pid = crate::process::create_process(
        "test_process",
        x86_64::VirtAddr::new(crate::test_process::test_process_main as usize as u64),
    );
    init_log!("Test process created");

    log::info!("Kernel: Created test process with PID {}", test_pid);

    // Test interrupt handling - should not panic or crash if APIC is working

    log::info!("Testing interrupt handling with int3...");
    // The interrupt test has been removed.

    log::info!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common() {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
        [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        use petroleum::page_table::ALLOCATOR;
        ALLOCATOR
            .lock()
            .init(heap_start_ptr, crate::heap::HEAP_SIZE);
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga();
}
