//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::{fs, graphics, interrupts, loader, process, syscall, vga};

// Macro to reduce repetitive serial logging - local copy since we moved function here
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;

macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

#[cfg(target_os = "uefi")]
pub fn init_common() {
    crate::vga::init_vga();
    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    kernel_log!("Kernel: APIC initialized and interrupts enabled");

    // Initialize process management
    process::init();
    kernel_log!("Kernel: Process management initialized");

    // Initialize system calls
    syscall::init();
    kernel_log!("Kernel: System calls initialized");

    // Initialize filesystem
    fs::init();
    kernel_log!("Kernel: Filesystem initialized");

    // Initialize program loader
    loader::init();
    kernel_log!("Kernel: Program loader initialized");

    // Create a test user process
    let test_entry = x86_64::VirtAddr::new(crate::test_process::test_process_main as usize as u64);
    let test_pid = process::create_process("test_process", test_entry);
    kernel_log!("Kernel: Created test process with PID {}", test_pid);

    // Test interrupt handling - should not panic or crash if APIC is working
    kernel_log!("Testing interrupt handling with int3...");
    unsafe {
        x86_64::instructions::interrupts::int3();
    }
    kernel_log!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common() {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] = [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        crate::heap::ALLOCATOR.lock().init(heap_start_ptr, crate::heap::HEAP_SIZE);
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga();
}
