//! Initialization module containing common initialization logic for both UEFI and BIOS boot
use crate::interrupts;
use alloc::boxed::Box;

use petroleum::assembly::KernelArgs;
use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size2MiB, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub fn init_graphics(physical_memory_offset: x86_64::VirtAddr) {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: init_graphics start\n");
    #[cfg(not(target_os = "uefi"))]
    {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Using BIOS VGA path\n");
        crate::vga::init_vga(physical_memory_offset, 0xb8000);
    }

    #[cfg(target_os = "uefi")]
    {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Using UEFI GOP path\n");
        unsafe {
            let args_ptr = petroleum::transition::KERNEL_ARGS;
            if !args_ptr.is_null() {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: KERNEL_ARGS not null\n");
                let virt_addr_raw = args_ptr as u64;
                let virt_addr = if (virt_addr_raw & (1 << 47)) != 0 {
                    virt_addr_raw | 0xFFFF_0000_0000_0000
                } else {
                    virt_addr_raw & 0x0000_FFFF_FFFF_FFFF
                };
                
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Attempting to dereference KernelArgs\n");
                let args = &*(virt_addr as *const petroleum::assembly::KernelArgs);
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: KernelArgs dereferenced successfully\n");
                
                if args.fb_address != 0 && args.fb_width > 0 && args.fb_bpp > 0 {
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: FB info looks valid\n");
                    let stride = (args.fb_width as u64 * (args.fb_bpp as u64 / 8)) as u32;
                    
                    let cleansed_phys_addr = args.fb_address & 0x000F_FFFF_FFFF_FFFF;
                    let fb_phys_addr = PhysAddr::new(cleansed_phys_addr);
                    
                    // Rely on the existing Direct Mapping (physical_memory_offset)
                    // to avoid potential triple faults during page table manipulation.
                    let fb_virt_addr = physical_memory_offset + fb_phys_addr.as_u64();
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Using Direct Mapping for FB\n");

                    let fb_info = petroleum::graphics::color::FramebufferInfo {
                        address: fb_virt_addr.as_u64(),
                        width: args.fb_width,
                        height: args.fb_height,
                        stride,
                        pixel_format: None,
                        colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                    };
                    let writer = petroleum::graphics::UefiFramebufferWriter::Uefi32(
                        petroleum::graphics::FramebufferWriter::new(fb_info)
                    );
                    
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to clone writer\n");
                    let writer2 = writer.clone();
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to Box::new console\n");
                    let console = Box::new(writer2);
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to set_primary_console\n");
                    crate::graphics::set_primary_console(console);
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to Box::new renderer\n");
                    let renderer = Box::new(writer);
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to set_primary_renderer\n");
                    crate::graphics::set_primary_renderer(renderer);
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: UEFI Framebuffer registered\n");
                } else {
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: UEFI FB info invalid or missing\n");
                }
            } else {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: KERNEL_ARGS is null\n");
            }
        }
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: init_graphics end\n");
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