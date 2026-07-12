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
use petroleum::initializer::FrameAllocator;

static WIFI_DRIVER_CTX: super::driver_context_impl::KernelDriverContext =
    super::driver_context_impl::KernelDriverContext;

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
        let heap_ptr = core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as *mut u8;
        petroleum::common::memory::set_heap_range(heap_ptr as usize, crate::heap::HEAP_TOTAL);
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
                let mut allocator = crate::hardware::pci_allocator::PciAllocator::new(0x40000000);
                allocator.assign_bars(scanner.get_devices());
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] PCI BARs step done\n");
            crate::boot_stage!(BootStage::PciBarsReady);
            Ok(())
        }),
        petroleum::init_step!("IOMMU", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] IOMMU step start\n");
            // Install memory callbacks before init
            nitrogen::iommu::set_mem_callbacks(nitrogen::iommu::MemCallbacks {
                alloc_frame: || {
                    let mut mgr = crate::memory_management::get_memory_manager().lock();
                    mgr.as_mut().and_then(|m| m.allocate_frame().ok().map(|p| p as u64))
                },
                free_frame: |phys| {
                    let mut mgr = crate::memory_management::get_memory_manager().lock();
                    if let Some(m) = mgr.as_mut() { let _ = m.free_frame(phys as usize); }
                },
                phys_to_virt: |phys| (phys + petroleum::common::memory::get_physical_memory_offset() as u64) as usize,
                map_mmio: |phys, size| {
                    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
                    let virt = (phys as u64 + off) as usize;
                    let mut mgr = crate::memory_management::get_memory_manager().lock();
                    let m = mgr.as_mut().ok_or(())?;
                    m.map_mmio_region(phys, virt, size).map_err(|_| ())?;
                    Ok(virt)
                },
            });
            // Try UEFI Configuration Table RSDP first, then BootContext, then legacy scan
            let uefi_rsdp =
                crate::boot::UEFI_RSDP_ADDRESS.load(core::sync::atomic::Ordering::Relaxed);
            let rsdp = if uefi_rsdp != 0 {
                uefi_rsdp
            } else {
                crate::contexts::boot::with_boot(|b| b.rsdp_address).unwrap_or(0)
            };
            let rsdp_source = if uefi_rsdp != 0 {
                "UEFI config table"
            } else if crate::contexts::boot::with_boot(|b| b.rsdp_address).unwrap_or(0) != 0 {
                "boot context"
            } else {
                "ACPI scan"
            };
            // Set ACPI phys_to_virt offset for the new acpi module
            let acpi_phys_off = petroleum::common::memory::get_physical_memory_offset() as u64;
            nitrogen::acpi::set_phys_to_virt_offset(acpi_phys_off);
            // Fall back to legacy ACPI scan if no RSDP from boot
            let rsdp = if rsdp != 0 { rsdp } else { nitrogen::acpi::find_rsdp().unwrap_or(0) };
            match nitrogen::iommu::init(rsdp) {
                Ok(()) => log::info!("IOMMU initialized (RSDP from {})", rsdp_source),
                Err(e) => {
                    log::warn!("IOMMU not available: {e} (RSDP={rsdp:#018x} from {rsdp_source})");
                    log::warn!("IOMMU: VT-d may be disabled in firmware, or hardware does not support it");
                }
            }
            // ── ECAM setup via MCFG ─────────────────────────────
            // Parse the MCFG ACPI table to find the ECAM MMIO base
            // address.  This is required for extended PCIe config
            // space (offsets ≥ 0x100), used by L1Sub disable and AER.
            //
            // Note: no explicit map_mmio_region is needed here.
            // The bootloader already identity- and higher-half-maps
            // 0-64 GB with 2 MiB huge pages.  ECAM resides well
            // within this range (typically 0xB0000000–0xBFFFFFFF),
            // so phys_to_virt(ecam_base) is directly accessible.
            if let Some(mcfg) = nitrogen::acpi::mcfg::parse_mcfg(rsdp) {
                let phys_off = petroleum::common::memory::get_physical_memory_offset() as u64;
                log::info!(
                    "MCFG: ECAM at phys={:#018x}, segment={}, buses {}-{}",
                    mcfg.base_address, mcfg.segment, mcfg.start_bus, mcfg.end_bus,
                );
                nitrogen::pci::set_ecam_info(mcfg.base_address, phys_off, mcfg.start_bus, mcfg.end_bus);
            } else {
                log::warn!("MCFG: table not found — extended PCIe config space unavailable");
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] IOMMU step done\n");
            Ok(())
        }),
        petroleum::init_step!("PAT", || {
            // Configure PAT[1] = WC for framebuffer write-combining.
            // This must run before Graphics init so that subsequent
            // WC page-table mappings use the correct memory type.
            let pat_ok = crate::memory_management::configure_framebuffer_pat();
            petroleum::write_serial_bytes(0x3F8, 0x3FD, if pat_ok { b"[init] PAT configured\n" } else { b"[init] PAT unavailable\n" });
            Ok(())
        }),
        petroleum::init_step!("Graphics", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Graphics step start\n");
            crate::graphics::init_graphics();
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Graphics step done\n");
            crate::boot_stage!(BootStage::GraphicsReady);
            Ok(())
        }),
        petroleum::init_step!("device_probe", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Device probe step start\n");
            // Build the driver registry and probe every PCI device.
            let registry = crate::drivers::registry::build_registry();
            let ctx = &crate::driver_context_impl::KernelDriverContext;
            let mut scanner = nitrogen::pci::PciScanner::new();
            let _ = scanner.scan_all_buses();
            for dev in scanner.get_devices() {
                // ── Safety gates before any non-posted MMIO read ───────
                // PCIe devices in D3 (power-gated) or L1 (ASPM link-down)
                // cannot complete MMIO reads, hanging the CPU forever.
                // These calls use port I/O (CF8/CFC) which always completes.
                dev.disable_pcie_aspm();
                dev.enable_memory_access();
                dev.ensure_d0();
                let _box = registry.match_device(ctx, dev);
                // The driver pushed its controller into its own static
                // list as a side‑effect — we don't keep the DriverBox
                // at this level.
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Device probe step done\n");
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
            crate::boot_stage!(BootStage::InputReady);
            Ok(())
        }),
        petroleum::init_step!("process", || {
            let heap_start = core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as usize;
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
        petroleum::init_step!("initramfs", || {
            crate::boot_stage::draw_boot_label(b"INITRAMFS");
            crate::linux::launch::init_initramfs();
            Ok(())
        }),
        petroleum::init_step!("device_manager", || {
            crate::boot_stage::draw_boot_label(b"DEVICE MANAGER");
            crate::hardware::device_manager::init_device_manager()
                .map_err(|_| "Failed to initialize device manager")?;
            petroleum::serial::serial_log(format_args!("Device manager initialised\n"));
            Ok(())
        }),
        petroleum::init_step!("usb_storage", || {
            crate::boot_stage::draw_boot_label(b"USB STORAGE");
            // USB is now probed by the DriverRegistry during device_probe.
            // This step is kept as a serial-log marker for boot progress.
            petroleum::serial::serial_log(format_args!("USB storage subsystem initialised\n"));
            Ok(())
        }),
        petroleum::init_step!("sd_card", || {
            crate::boot_stage::draw_boot_label(b"SD CARD");
            petroleum::serial::serial_log(format_args!("SD card subsystem initialised\n"));
            Ok(())
        }),
        petroleum::init_step!("wifi", || {
            crate::boot_stage::draw_boot_label(b"WIFI");
            nitrogen::iwlwifi::set_wifi_driver_context(&WIFI_DRIVER_CTX);
            petroleum::serial::serial_log(format_args!("WiFi driver context set\n"));
            Ok(())
        }),
        petroleum::init_step!("gui", || {
            crate::boot_stage::draw_boot_label(b"DESKTOP SERVICES");
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

    ];
    InitSequence::new(&common_steps).run();

    // Shell is no longer auto-started.  It is launched on demand via
    // the AppGrid overlay or the desktop context menu (NewShell action).
    // See `crate::scheduler::request_shell_launch()`.
}
