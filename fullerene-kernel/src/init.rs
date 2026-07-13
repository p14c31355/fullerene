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

/// Format a PCI device descriptor into a byte buffer for serial debug.
fn hex_fmt(buf: &mut [u8; 72], bus: u8, dev: u8, func: u8, vid: u16, did: u16, cls: u8, scls: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut i = 0;
    macro_rules! push { ($b:expr) => { if i < buf.len() { buf[i] = $b; i += 1; } } }
    macro_rules! hex { ($v:expr) => { push!(HEX[($v >> 4) as usize]); push!(HEX[($v & 0xF) as usize]); } }
    macro_rules! bytes { ($s:expr) => { for &b in $s { push!(b); } } }
    bytes!(b"[probe] "); hex!(bus); push!(':' as u8); hex!(dev); push!('.' as u8); hex!(func); push!(' ' as u8);
    hex!((vid >> 8) as u8); hex!(vid as u8); push!(':' as u8);
    hex!((did >> 8) as u8); hex!(did as u8);
    bytes!(b" class="); hex!(cls); push!('/' as u8); hex!(scls); push!('\n' as u8);
}

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
                    petroleum::ALLOCATOR
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
            let registry = crate::drivers::registry::build_registry();
            let ctx = &crate::driver_context_impl::KernelDriverContext;
            let mut scanner = nitrogen::pci::PciScanner::new();
            let _ = scanner.scan_all_buses();
            for dev in scanner.get_devices() {
                // ── Safety gates before any non-posted MMIO read ───
                // PCIe devices in D3 or L1 cannot complete MMIO reads,
                // hanging the CPU forever.  These use port I/O only.
                // Log each device so we can identify where real hardware hangs.
                {
                    let mut buf = [0u8; 72];
                    hex_fmt(&mut buf, dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id, dev.class_code, dev.subclass);
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf);
                }

                // Skip PCI bridges — drivers only match endpoints.
                if dev.class_code == 0x06 { continue; }

                dev.disable_pcie_aspm();
                dev.enable_memory_access();
                dev.ensure_d0();

                // Quick MMIO-safety check: read config-space Vendor ID again
                // after enable_memory.  If it returns 0xFFFF the device is gone
                // (phantom / unpopulated slot) — skip to avoid MMIO hang.
                let vid = nitrogen::pci::PciConfigSpace::read_config_word(
                    dev.bus, dev.device, dev.function, 0,
                );
                if vid == 0xFFFF || vid == 0x0000 { continue; }

                let _box = registry.match_device(ctx, dev);
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init] Device probe step done\n");
            Ok(())
        }),
        // draw_step_hint shows the step name at the bottom of the boot
        // screen so we can identify hangs without serial access.
        petroleum::init_step!("PS2 Controller", || {
            crate::boot_stage::draw_step_hint(b"ps2_ctrl");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_ctrl start\n");
            let devices = nitrogen::ps2::init_ps2_controller();
            petroleum::serial::serial_log(format_args!("PS/2 controller initialized (keyboard={}, mouse={})\n", devices & 1 != 0, devices & 2 != 0));
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_ctrl done\n");
            Ok(())
        }),
        petroleum::init_step!("PS2 Mouse", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_mouse start\n");
            match nitrogen::ps2::mouse::init_mouse() {
                Ok(()) => { petroleum::serial::serial_log(format_args!("PS/2 mouse initialised\n")); }
                Err(e) => { petroleum::serial::serial_log(format_args!("PS/2 mouse init failed: {}\n", e)); }
            }
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_mouse done\n");
            Ok(())
        }),
        petroleum::init_step!("PS2 Keyboard", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_kbd start\n");
            nitrogen::ps2::keyboard::init_keyboard();
            petroleum::serial::serial_log(format_args!("PS/2 keyboard initialised\n"));
            crate::boot_stage!(BootStage::InputReady);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] ps2_kbd done\n");
            Ok(())
        }),
        petroleum::init_step!("process", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] process start\n");
            let heap_start = core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as usize;
            let heap_end = heap_start + crate::heap::HEAP_SIZE;
            crate::process::init(heap_start, heap_end);
            crate::boot_stage!(BootStage::ProcessReady);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] process done\n");
            Ok(())
        }),
        petroleum::init_step!("syscall", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] syscall start\n");
            crate::syscall::init();
            crate::boot_stage!(BootStage::SyscallReady);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] syscall done\n");
            Ok(())
        }),
        petroleum::init_step!("fs", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] fs start\n");
            crate::fs::init();
            crate::boot_stage!(BootStage::FilesystemReady);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] fs done\n");
            Ok(())
        }),
        petroleum::init_step!("loader", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] loader start\n");
            crate::loader::init();
            crate::boot_stage!(BootStage::LoaderReady);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] loader done\n");
            Ok(())
        }),
        petroleum::init_step!("initramfs", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] initramfs start\n");
            crate::boot_stage::draw_boot_label(b"INITRAMFS");
            crate::linux::launch::init_initramfs();
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] initramfs done\n");
            Ok(())
        }),
        petroleum::init_step!("device_manager", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] device_mgr start\n");
            crate::boot_stage::draw_boot_label(b"DEVICE MANAGER");
            crate::hardware::device_manager::init_device_manager()
                .map_err(|_| "Failed to initialize device manager")?;
            petroleum::serial::serial_log(format_args!("Device manager initialised\n"));
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] device_mgr done\n");
            Ok(())
        }),
        petroleum::init_step!("usb_storage", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] usb_storage start\n");
            crate::boot_stage::draw_boot_label(b"USB STORAGE");
            crate::boot_stage::draw_step_hint(b"pre_log"); // drawn BEFORE serial_log
            petroleum::serial::serial_log(format_args!("USB storage subsystem initialised\n"));
            crate::boot_stage::draw_step_hint(b"usb_ok ");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] usb_storage done\n");
            Ok(())
        }),
        petroleum::init_step!("sd_card", || {
            crate::boot_stage::draw_step_hint(b"sd_strt");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] sd_card start\n");
            crate::boot_stage::draw_boot_label(b"SD CARD");
            petroleum::serial::serial_log(format_args!("SD card subsystem initialised\n"));
            crate::boot_stage::draw_step_hint(b"sd_ok  ");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] sd_card done\n");
            Ok(())
        }),
        petroleum::init_step!("wifi", || {
            crate::boot_stage::draw_step_hint(b"wf_strt");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] wifi start\n");
            crate::boot_stage::draw_boot_label(b"WIFI");
            nitrogen::iwlwifi::set_wifi_driver_context(&WIFI_DRIVER_CTX);
            petroleum::serial::serial_log(format_args!("WiFi driver context set\n"));
            crate::boot_stage::draw_step_hint(b"wifi_ok");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] wifi done\n");
            Ok(())
        }),
        petroleum::init_step!("gui", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] gui start\n");
            crate::boot_stage::draw_boot_label(b"DESKTOP SERVICES");
            crate::gui::init();
            petroleum::serial::serial_log(format_args!("GUI subsystem initialised\n"));
            crate::boot_stage!(BootStage::GuiReady);
            crate::boot_stage::draw_step_hint(b"gui_ok ");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] gui done\n");
            Ok(())
        }),
        petroleum::init_step!("task_manager", || {
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] task_mgr start\n");
            crate::task::init_task_manager();
            petroleum::serial::serial_log(format_args!("Task manager initialised\n"));
            crate::boot_stage!(BootStage::TaskManagerReady);
            crate::boot_stage::draw_step_hint(b"task_ok");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[step] task_mgr done\n");
            Ok(())
        }),

    ];
    InitSequence::new(&common_steps).run();

    // Shell is no longer auto-started.  It is launched on demand via
    // the AppGrid overlay or the desktop context menu (NewShell action).
    // See `crate::scheduler::request_shell_launch()`.
}
