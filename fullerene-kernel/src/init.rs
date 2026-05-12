//! Initialization module containing common initialization logic for both UEFI and BIOS boot
use crate::interrupts;
use alloc::boxed::Box;

use petroleum::assembly::KernelArgs;
use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub fn init_graphics(physical_memory_offset: x86_64::VirtAddr) {
    #[cfg(not(target_os = "uefi"))]
    {
        crate::vga::init_vga(physical_memory_offset, 0xb8000);
    }

    #[cfg(target_os = "uefi")]
    {
        unsafe {
            let args_ptr = petroleum::transition::KERNEL_ARGS;
            if !args_ptr.is_null() {
                let virt_addr_raw = args_ptr as u64;
                let virt_addr = if (virt_addr_raw & (1 << 47)) != 0 {
                    virt_addr_raw | 0xFFFF_0000_0000_0000
                } else {
                    virt_addr_raw & 0x0000_FFFF_FFFF_FFFF
                };
                let args = &*(virt_addr as *const petroleum::assembly::KernelArgs);
                
                if args.fb_address != 0 && args.fb_width > 0 && args.fb_bpp > 0 {
                    let stride = (args.fb_width as u64 * (args.fb_bpp as u64 / 8)) as u32;
                    let fb_info = petroleum::graphics::color::FramebufferInfo {
                        address: args.fb_address,
                        width: args.fb_width,
                        height: args.fb_height,
                        stride,
                        pixel_format: None, // Simplified for now
                        colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                    };
                    let writer = petroleum::graphics::UefiFramebufferWriter::Uefi32(
                        petroleum::graphics::FramebufferWriter::new(fb_info)
                    );
                    
                    // Register as both console and renderer
                    crate::graphics::set_primary_console(Box::new(writer.clone()));
                    crate::graphics::set_primary_renderer(Box::new(writer));
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"UEFI Framebuffer registered\n");
                }
            }
        }
    }
}

pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Init common start\n");

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

    init_graphics(physical_memory_offset);

    crate::interrupts::init();
    let common_steps = [
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
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Test process created\n");
        }
    }
}

