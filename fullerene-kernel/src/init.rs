//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use petroleum::{InitSequence, init_log, write_serial_bytes};

#[cfg(target_os = "uefi")]
fn init_vga_step() -> Result<(), &'static str> { crate::vga::init_vga(); Ok(()) }
#[cfg(target_os = "uefi")]
fn init_apic_step() -> Result<(), &'static str> { interrupts::init_apic(); Ok(()) }
#[cfg(target_os = "uefi")]
fn init_process_step() -> Result<(), &'static str> { crate::process::init(); Ok(()) }
#[cfg(target_os = "uefi")]
fn init_syscall_step() -> Result<(), &'static str> { crate::syscall::init(); Ok(()) }
#[cfg(target_os = "uefi")]
fn init_fs_step() -> Result<(), &'static str> { crate::fs::init(); Ok(()) }
#[cfg(target_os = "uefi")]
fn init_loader_step() -> Result<(), &'static str> { crate::loader::init(); Ok(()) }

#[cfg(target_os = "uefi")]
pub fn init_common() {
    let steps = [
        ("VGA", init_vga_step as fn() -> Result<(), &'static str>),
        ("APIC", init_apic_step as fn() -> Result<(), &'static str>),
        ("process", init_process_step as fn() -> Result<(), &'static str>),
        ("syscall", init_syscall_step as fn() -> Result<(), &'static str>),
        ("fs", init_fs_step as fn() -> Result<(), &'static str>),
        ("loader", init_loader_step as fn() -> Result<(), &'static str>),
    ];

    InitSequence::new(&steps).run();

    init_log!("About to create test process");
    let test_pid = crate::process::create_process(
        "test_process",
        x86_64::VirtAddr::new(crate::process::test_process_main as usize as u64),
    );
    init_log!("Test process created: {}", test_pid);
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
