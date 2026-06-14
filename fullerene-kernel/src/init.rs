//! Initialization module containing common initialization logic for both UEFI and BIOS boot
//!
//! This module provides the `init_common` function which is called after the
//! bootloader has set up the basic environment. It initializes:
//! - Graphics (GOP framebuffer)
//! - Interrupts (IDT, exceptions)
//! - Process management
//! - Syscalls
//! - Filesystem
//! - Loader

use crate::boot_stage::BootStage;
use petroleum::common::InitSequence;
use x86_64::VirtAddr;

/// Common initialization function for both UEFI and BIOS boot paths
///
/// # Arguments
///
/// * `physical_memory_offset` - The offset for higher-half kernel mapping
pub fn init_common(_physical_memory_offset: x86_64::VirtAddr) {
    petroleum::serial::serial_log(format_args!("Init common start\n"));

    crate::boot_stage!(BootStage::KernelEntry);

    #[cfg(not(target_os = "uefi"))]
    {
        use core::mem::MaybeUninit;
        let bios_init_steps = [
            petroleum::init_step!("BIOS Heap and GDT", || {
                static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
                    [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
                unsafe {
                    let ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
                    petroleum::page_table::ALLOCATOR
                        .lock()
                        .init(ptr, crate::heap::HEAP_SIZE);
                    petroleum::common::memory::set_heap_range(ptr as usize, crate::heap::HEAP_SIZE);
                    crate::gdt::init(x86_64::VirtAddr::from_ptr(ptr));
                }
                Ok(())
            }),
            petroleum::init_step!("Serial", || {
                petroleum::serial::serial_init();
                Ok(())
            }),
        ];
        InitSequence::new(&bios_init_steps).run();
    }

    crate::boot_stage!(BootStage::HeapReady);

    #[cfg(target_os = "uefi")]
    {
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as *mut u8;
            petroleum::common::memory::set_heap_range(heap_ptr as usize, crate::heap::HEAP_TOTAL);
        }
    }

    // ── Log system initialisation ──────────────────────────────
    *petroleum::common::logging::LOG_HOOK.lock() = Some(|_level, msg| {
        crate::klog::write_bytes(msg.as_bytes());
    });
    let _ = petroleum::common::logging::init_global_logger();
    log::set_max_level(log::LevelFilter::Info);
    let common_steps = [
        petroleum::init_step!("Interrupts", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Interrupts step start\n");
            crate::interrupts::init();
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Interrupts step done\n");
            crate::boot_stage!(BootStage::InterruptsReady);
            Ok(())
        }),
        petroleum::init_step!("Kernel Context", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Kernel Context step start\n");
            crate::contexts::kernel::init_kernel();
            petroleum::serial::serial_log(format_args!("Kernel context initialised\n"));
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Kernel Context step done\n");
            crate::boot_stage!(BootStage::KernelContextReady);
            Ok(())
        }),
        petroleum::init_step!("PCI BARs", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] PCI BARs step start\n");
            petroleum::serial::serial_log(format_args!("Initializing PCI BARs...\n"));
            let mut scanner = nitrogen::pci::PciScanner::new();
            if scanner.scan_all_buses().is_ok() {
                let mut allocator = petroleum::hardware::pci::PciAllocator::new(0x40000000);
                allocator.assign_bars(scanner.get_devices());
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] PCI BARs step done\n");
            crate::boot_stage!(BootStage::PciBarsReady);
            Ok(())
        }),
        petroleum::init_step!("Graphics", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Graphics step start\n");
            crate::graphics::init_graphics();
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Graphics step done\n");
            crate::boot_stage!(BootStage::GraphicsReady);
            Ok(())
        }),
        petroleum::init_step!("PS2 Controller", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] PS2 Controller step start\n");
            let devices = nitrogen::ps2::init_ps2_controller();
            petroleum::serial::serial_log(format_args!(
                "PS/2 controller initialized (keyboard={}, mouse={})\n",
                devices & 1 != 0,
                devices & 2 != 0
            ));
            Ok(())
        }),
        petroleum::init_step!("PS2 Mouse", || {
            match nitrogen::ps2::mouse::init_mouse() {
                Ok(()) => {
                    petroleum::serial::serial_log(format_args!("PS/2 mouse initialised\n"));
                    Ok(())
                }
                Err(e) => {
                    petroleum::serial::serial_log(format_args!("PS/2 mouse init failed: {}\n", e));
                    Ok(())
                }
            }
        }),
        petroleum::init_step!("PS2 Keyboard", || {
            nitrogen::ps2::keyboard::init_keyboard();
            petroleum::serial::serial_log(format_args!("PS/2 keyboard initialised\n"));
            Ok(())
        }),
        petroleum::init_step!("process", || {
            let heap_start =
                unsafe { core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as usize };
            let heap_end = heap_start + crate::heap::HEAP_SIZE;
            crate::process::init(heap_start, heap_end);
            crate::boot_stage!(BootStage::ProcessReady);
            Ok(())
        }),
        petroleum::init_step!("syscall", || {
            crate::syscall::init();
            crate::boot_stage!(BootStage::SyscallReady);
            Ok(())
        }),
        petroleum::init_step!("fs", || {
            crate::fs::init();
            crate::boot_stage!(BootStage::FilesystemReady);
            Ok(())
        }),
        petroleum::init_step!("loader", || {
            crate::loader::init();
            crate::boot_stage!(BootStage::LoaderReady);
            Ok(())
        }),
        petroleum::init_step!("device_manager", || {
            crate::hardware::device_manager::init_device_manager()
                .map_err(|_| "Failed to initialize device manager")?;
            petroleum::serial::serial_log(format_args!("Device manager initialised\n"));
            Ok(())
        }),
        petroleum::init_step!("gui", || {
            crate::gui::init();
            petroleum::serial::serial_log(format_args!("GUI subsystem initialised\n"));
            crate::boot_stage!(BootStage::GuiReady);
            Ok(())
        }),
        petroleum::init_step!("task_manager", || {
            crate::task::init_task_manager();
            petroleum::serial::serial_log(format_args!("Task manager initialised\n"));
            crate::boot_stage!(BootStage::TaskManagerReady);
            Ok(())
        }),
        petroleum::init_step!("app_runner", || {
            crate::app_runner::init();
            petroleum::serial::serial_log(format_args!("App runner initialised\n"));
            crate::boot_stage!(BootStage::AppRunnerReady);
            Ok(())
        }),
    ];
    InitSequence::new(&common_steps).run();

    // Shell is no longer auto-started.  It is launched on demand via
    // the AppGrid overlay or the desktop context menu (NewShell action).
    // See `crate::scheduler::request_shell_launch()`.

    crate::boot_stage!(BootStage::ShellRunning);
}
