//! Initialization module containing common initialization logic for both UEFI and BIOS boot
use crate::interrupts;
use alloc::boxed::Box;

use petroleum::{common::InitSequence, init_log, write_serial_bytes};
use spin::Once;

pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] entered\n");
    
    let mut buf = [0u8; 16];
    let len = petroleum::serial::format_hex_to_buffer(rsp, &mut buf, 16);
    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [init_common] RSP: 0x");
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

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
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] initializing graphics from KernelArgs\n");
        
        unsafe {
            let args_ptr = petroleum::transition::KERNEL_ARGS;
            
            // DEBUG: Print the pointer value before dereferencing
            let mut buf = [0u8; 16];
            let len = petroleum::serial::format_hex_to_buffer(args_ptr as u64, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [init_common] KERNEL_ARGS ptr: 0x");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

            if !args_ptr.is_null() {
                let args = &*args_ptr;
                
                if args.fb_address != 0 {
                    let info = petroleum::graphics::color::FramebufferInfo {
                        address: args.fb_address,
                        width: args.fb_width,
                        height: args.fb_height,
                        stride: args.fb_width, // Default stride to width
                        pixel_format: None,    // Default to VGA/Simple format
                        colors: petroleum::graphics::color::ColorScheme::VGA_GREEN_ON_BLACK,
                    };
                    
                    // We still avoid calling .new() to be safe, but we can now use the actual values.
                    // If FramebufferWriter fields are private, we'll need to add a simple 
                    // public constructor or use a wrapper in petroleum.
                    // For now, we'll try to use the values to verify they are passed correctly.
                    
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] FB Info: ");
                    let mut buf = [0u8; 16];
                    let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b" size: ");
                    // Simple print for width/height
                    let w = args.fb_width;
                    let h = args.fb_height;
                    // (Simplified printing for brevity)
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b" [OK]\n");
                } else {
                    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] No FB address in KernelArgs\n");
                }
            } else {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] KERNEL_ARGS is null\n");
            }
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] graphics init from args completed\n");
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
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] About to create test process\n");
        let test_pid = crate::process::create_process(
            "test_process",
            x86_64::VirtAddr::new(crate::process::test_process_main as *const () as usize as u64),
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] create_process returned\n");
        match test_pid {
            Ok(pid) => {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] Test process created successfully\n");
                // Use write_serial_bytes instead of init_log to avoid potential deadlock with SERIAL_PORT_WRITER/UEFI_WRITER
                let mut buf = [0u8; 32];
                let len = petroleum::serial::format_dec_to_buffer(pid as usize, &mut buf);
                unsafe {
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"Test process created: ");
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
                }
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
            },
            Err(e) => {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] Test process creation failed\n");
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Failed to create test process\n");
            },
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] Post-init block completed\n");
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_common] End of init_common function\n");
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