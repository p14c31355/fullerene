//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::{self, Vec},
};
use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;

#[cfg(target_os = "uefi")]
macro_rules! init_step {
    ($name:expr, $closure:expr) => {
        (
            $name,
            Box::new($closure) as Box<dyn Fn() -> Result<(), &'static str>>,
        )
    };
}

#[cfg(target_os = "uefi")]
pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    init_log!("Initializing common components");

    let steps = [
        init_step!("VGA", move || {
            crate::vga::init_vga(physical_memory_offset);
            Ok(())
        }),
        init_step!("Graphics", || {
            crate::graphics::text::init_fallback_graphics()?;
            Ok(())
        }),
        init_step!("LOCAL_APIC", || {
            *petroleum::LOCAL_APIC_ADDRESS.lock() = petroleum::LocalApicAddress(0xfee00000 as *mut u32);
            Ok(())
        }),
        init_step!("APIC", || {
            interrupts::init_apic();
            Ok(())
        }),
        init_step!("process", || {
            crate::process::init();
            Ok(())
        }),
        init_step!("syscall", || {
            crate::syscall::init();
            Ok(())
        }),
        init_step!("fs", || {
            crate::fs::init();
            Ok(())
        }),
        init_step!("loader", || {
            crate::loader::init();
            Ok(())
        }),
    ];
    InitSequence::new(&steps).run();

    init_log!("About to create test process");
    let test_pid = crate::process::create_process(
        "test_process",
        x86_64::VirtAddr::new(crate::process::test_process_main as usize as u64),
    );
    match test_pid {
        Ok(pid) => init_log!("Test process created: {}", pid),
        Err(e) => init_log!("Failed to create test process: {:?}", e),
    }
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
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
        // Set heap range for page fault detection
        petroleum::common::memory::set_heap_range(
            heap_start_ptr as usize,
            crate::heap::HEAP_SIZE,
        );
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // For UEFI, APIC is used, for BIOS, use PIC initially
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga(physical_memory_offset);
    crate::process::init();
    crate::syscall::init();
    crate::fs::init();
    crate::loader::init();
}
