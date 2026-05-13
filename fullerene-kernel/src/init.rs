//! Initialization module containing common initialization logic for both UEFI and BIOS boot
use crate::interrupts;
use alloc::boxed::Box;

use petroleum::assembly::KernelArgs;
use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size2MiB, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    petroleum::serial::serial_log(format_args!("Init common start\n"));

    #[cfg(not(target_os = "uefi"))]
    {
        use core::mem::MaybeUninit;
        let bios_init_steps = [
            petroleum::init_step!("BIOS Heap and GDT", || {
                static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] = [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
                unsafe {
                    let ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
                    petroleum::page_table::ALLOCATOR.lock().init(ptr, crate::heap::HEAP_SIZE);
                    petroleum::common::memory::set_heap_range(ptr as usize, crate::heap::HEAP_SIZE);
                    crate::gdt::init(x86_64::VirtAddr::from_ptr(ptr));
                }
                Ok(())
            }),
            petroleum::init_step!("Interrupts", || { interrupts::init(); Ok(()) }),
            petroleum::init_step!("Serial", || { petroleum::serial::serial_init(); Ok(()) }),
        ];
        InitSequence::new(&bios_init_steps).run();
    }

    #[cfg(target_os = "uefi")]
    {
        // UEFI specific memory mapping for KernelArgs is handled in bootloader/transition
    }

    let common_steps = [
        petroleum::init_step!("Graphics", || {
            crate::graphics::init_graphics();
            Ok(())
        }),
        petroleum::init_step!("Interrupts", || {
            crate::interrupts::init();
            Ok(())
        }),
        petroleum::init_step!("process", || { crate::process::init(); Ok(()) }),
        petroleum::init_step!("syscall", || { crate::syscall::init(); Ok(()) }),
        petroleum::init_step!("fs", || { crate::fs::init(); Ok(()) }),
        petroleum::init_step!("loader", || { crate::loader::init(); Ok(()) }),
    ];
    InitSequence::new(&common_steps).run();

    #[cfg(target_os = "uefi")]
    {
        if let Ok(_pid) = crate::process::create_process(
            "test_process",
            VirtAddr::new(crate::process::test_process_main as *const () as usize as u64),
            false,
        ) {
            petroleum::serial::serial_log(format_args!("Test process created\n"));
        }
    }
}