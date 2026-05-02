//! Initialization module containing common initialization logic for both UEFI and BIOS boot
use crate::interrupts;
use alloc::boxed::Box;

use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;

pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] entered\n");
    init_log!("Initializing common components");

    // 1. Platform-specific early initialization
    #[cfg(not(target_os = "uefi"))]
    {
        use core::mem::MaybeUninit;
        let bios_init_steps = [
            petroleum::init_step!("BIOS Heap and GDT", || {
                static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
                    [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
                let heap_start_addr: x86_64::VirtAddr;
                unsafe {
                    let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
                    heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
                    use petroleum::page_table::ALLOCATOR;
                    ALLOCATOR.lock().init(heap_start_ptr, crate::heap::HEAP_SIZE);
                    petroleum::common::memory::set_heap_range(heap_start_ptr as usize, crate::heap::HEAP_SIZE);
                }
                crate::gdt::init(heap_start_addr);
                Ok(())
            }),
            petroleum::init_step!("Interrupts", || {
                interrupts::init();
                Ok(())
            }),
            petroleum::init_step!("Serial", || {
                petroleum::serial::serial_init();
                Ok(())
            }),
        ];
        InitSequence::new(&bios_init_steps).run();
        crate::vga::init_vga(physical_memory_offset);
    }

    #[cfg(target_os = "uefi")]
    {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] starting uefi_early_steps\n");
        let uefi_early_steps = [
            petroleum::init_step!("Graphics", init_early_graphics),
        ];
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] calling InitSequence::run for uefi_early_steps\n");
        InitSequence::new(&uefi_early_steps).run();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] uefi_early_steps completed\n");
    }

    // 2. Common initialization sequence
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] starting common_steps\n");
    let common_steps = [
        petroleum::init_step!("process", init_process_step),
        petroleum::init_step!("syscall", init_syscall_step),
        petroleum::init_step!("fs", init_fs_step),
        petroleum::init_step!("loader", init_loader_step),
    ];
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] calling InitSequence::run\n");
    InitSequence::new(&common_steps).run();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] InitSequence::run returned\n");

    // 3. Post-initialization (UEFI only for now)
    #[cfg(target_os = "uefi")]
    {
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
}

fn init_early_graphics() -> Result<(), &'static str> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init_fallback_graphics start\n");
    let _ = crate::graphics::text::init_fallback_graphics();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init_fallback_graphics done\n");
    Ok(())
}

fn init_process_step() -> Result<(), &'static str> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init process start\n");
    crate::process::init();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init process done\n");
    Ok(())
}

fn init_syscall_step() -> Result<(), &'static str> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init syscall start\n");
    crate::syscall::init();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init syscall done\n");
    Ok(())
}

fn init_fs_step() -> Result<(), &'static str> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init fs start\n");
    crate::fs::init();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init fs done\n");
    Ok(())
}

fn init_loader_step() -> Result<(), &'static str> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init loader start\n");
    crate::loader::init();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] init loader done\n");
    Ok(())
}

