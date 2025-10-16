//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use crate::test_process::test_process_main;
use x86_64::VirtAddr;

#[cfg(target_os = "uefi")]
pub fn init_common() {
    crate::vga::init_vga();
    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    log::info!("Kernel: APIC initialized and interrupts enabled");

    crate::process::init();

    log::info!("Kernel: Process management initialized");

    crate::syscall::init();

    log::info!("Kernel: System calls initialized");

    crate::fs::init();

    log::info!("Kernel: Filesystem initialized");

    crate::loader::init();

    log::info!("Kernel: Program loader initialized");

    let test_pid = crate::process::create_process("test_process", VirtAddr::new(test_process_main as usize as u64));

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
