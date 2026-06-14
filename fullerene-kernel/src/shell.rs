//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel's raw syscall I/O.

use crate::syscall::kernel_syscall;
use alloc::format;

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    nitrogen::ps2::keyboard::init_keyboard();
    register_nozzle_hooks();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

// ── Nozzle hook registration ──────────────────────────────────────

/// Register kernel implementations for nozzle's filesystem and system hooks.
fn register_nozzle_hooks() {
    // FS hooks — wire into kernel VFS
    nozzle::fs_hooks::set_fs_list_fn(|ctx| match crate::vfs::readdir("/") {
        Ok(entries) => {
            for ent in entries {
                let line = if ent.is_dir {
                    format!("  {}/\n", ent.name)
                } else {
                    format!("  {}  ({} bytes)\n", ent.name, ent.size)
                };
                ctx.terminal.write_str(&line);
            }
        }
        Err(e) => {
            let msg = format!("ls: {}\n", e);
            ctx.terminal.write_str(&msg);
        }
    });

    nozzle::fs_hooks::set_fs_read_fn(|ctx, path| match crate::vfs::open(path, 0) {
        Ok(fd) => {
            let mut buf = [0u8; 512];
            loop {
                match crate::vfs::read(fd.fd, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        ctx.terminal
                            .write_str(core::str::from_utf8(&buf[..n]).unwrap_or("(binary)"));
                    }
                    Err(e) => {
                        let msg = format!("cat: {}\n", e);
                        ctx.terminal.write_str(&msg);
                        break;
                    }
                }
            }
            let _ = crate::vfs::close(fd.fd);
            ctx.terminal.write_str("\n");
        }
        Err(e) => {
            let msg = format!("cat: {}: {}\n", path, e);
            ctx.terminal.write_str(&msg);
        }
    });

    nozzle::fs_hooks::set_fs_pwd_fn(|ctx| {
        ctx.terminal.write_str("/\n");
    });

    // Sys info hooks — provide real kernel data (unified handler)
    nozzle::sys_hooks::set_sys_info_fn(|ctx, cmd| match cmd {
        "mem" => {
            let (heap_start, heap_end) = petroleum::common::memory::get_heap_range();
            let total = if heap_end > heap_start {
                (heap_end - heap_start) / 1024
            } else {
                0
            };
            let msg = format!(
                "Memory: heap {} KiB total (start=0x{:x}, end=0x{:x})\n",
                total, heap_start, heap_end
            );
            ctx.terminal.write_str(&msg);
        }
        "tasks" => {
            let list = crate::task::TASK_MANAGER.format_task_list();
            ctx.terminal.write_str(&list);
        }
        "taskmon" => {
            let list = crate::task::TASK_MANAGER.format_task_list();
            ctx.terminal.write_str(&list);
        }
        "devices" => {
            if let Some(ref manager) = *crate::hardware::device_manager::get_device_manager().lock()
            {
                let devs = manager.list_devices();
                if devs.is_empty() {
                    ctx.terminal.write_str("No devices registered.\n");
                } else {
                    ctx.terminal
                        .write_str("DEVICE            TYPE        ENABLED\n");
                    ctx.terminal
                        .write_str("----------------  ----------  -------\n");
                    for d in devs {
                        let status = if d.enabled { "yes" } else { "no" };
                        let line = format!("{:<16}  {:<10}  {}\n", d.name, d.device_type, status);
                        ctx.terminal.write_str(&line);
                    }
                }
            } else {
                ctx.terminal.write_str("Device manager not initialized.\n");
            }
        }
        "calc" => {
            ctx.terminal.write_str("Usage: calc <expression>\n");
            ctx.terminal.write_str("Example: calc (2+3)*4\n");
        }
        "theme" => {
            let current = solvent::current_theme_variant();
            let name = match current {
                solvent::ThemeVariant::Dark => "dark",
                solvent::ThemeVariant::Light => "light",
            };
            let msg = format!("Current theme: {}\n", name);
            ctx.terminal.write_str(&msg);
            ctx.terminal
                .write_str("Usage: theme toggle | theme dark | theme light\n");
        }
        "wallpaper" => {
            let current = solvent::get_wallpaper();
            let name = match current {
                solvent::WallpaperMode::SolidColor => "solid",
                solvent::WallpaperMode::GridPattern => "grid",
                solvent::WallpaperMode::Gradient => "gradient",
                solvent::WallpaperMode::Preset(idx) => {
                    let presets = solvent::wallpaper_presets();
                    presets.get(idx).map_or("unknown", |p| p.name)
                }
            };
            let msg = format!("Current wallpaper: {}\n", name);
            ctx.terminal.write_str(&msg);
            ctx.terminal
                .write_str("Usage: wallpaper solid | grid | gradient | beach | mountain | city\n");
        }
        "windows" => {
            if solvent::is_initialized() {
                ctx.terminal
                    .write_str("Windows: managed by Lattice compositor\n");
                ctx.terminal
                    .write_str("Use the GUI to interact with windows.\n");
            } else {
                ctx.terminal.write_str("Windowing system not active.\n");
            }
        }
        "dmesg" => {
            let klog_len = crate::klog::len();
            if klog_len > 0 {
                ctx.terminal.write_str("=== Kernel log ===\n");
                let snap = crate::klog::snapshot();
                let s = alloc::string::String::from_utf8_lossy(&snap);
                ctx.terminal.write_str(&s);
                if !s.ends_with('\n') {
                    ctx.terminal.write_str("\n");
                }
                ctx.terminal.write_str("=== End kernel log ===\n");
            }
            // ── HDA diagnostic info (read via KernelContext) ──
            {
                let diag = crate::contexts::kernel::with_kernel(|k| k.audio.diag).unwrap_or(
                    nitrogen::hda::controller::HdaDiagInfo {
                        gcap: 0,
                        gcap64: false,
                        corb_phys: 0,
                        rirb_phys: 0,
                        states_after_crst: 0,
                        populated: false,
                    },
                );
                if diag.populated {
                    ctx.terminal.write_str("\n=== HDA diagnostic ===\n");
                    let line = alloc::format!(
                        "GCAP: 0x{:08x}  (64-bit: {})\nCORB phys: 0x{:016x}\nRIRB phys: 0x{:016x}\nSTATESTS after CRST: 0x{:04x} (SDIN0={})\n",
                        diag.gcap,
                        if diag.gcap64 { "YES" } else { "NO" },
                        diag.corb_phys,
                        diag.rirb_phys,
                        diag.states_after_crst,
                        if diag.states_after_crst & 0x0001 != 0 {
                            1u8
                        } else {
                            0u8
                        },
                    );
                    ctx.terminal.write_str(&line);
                    ctx.terminal.write_str("=== End HDA diagnostic ===\n");
                }
            }
            ctx.terminal.write_str("\n=== Kernel trace buffer ===\n");
            let events = crate::tracing::snapshot();
            if events.is_empty() {
                ctx.terminal.write_str("(no trace events recorded)\n");
            } else {
                for ev in events {
                    let cat = core::str::from_utf8(&ev.category)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    let msg = core::str::from_utf8(&ev.message)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    let line = format!("[{}] {}: {}\n", ev.tick, cat, msg);
                    ctx.terminal.write_str(&line);
                }
            }
        }
        "run" => {
            ctx.terminal.write_str("Usage: run <app_name>\n");
            ctx.terminal.write_str("Available: toluene, hello\n");
        }
        "pci" => {
            use alloc::format;
            use nitrogen::pci::PciScanner;
            ctx.terminal
                .write_str("BUS  DEV  FUN  VENDOR  DEVICE  CLASS      SUBCLASS  DESCRIPTION\n");
            ctx.terminal
                .write_str("---- ---- ----  ------  ------  ---------  --------  -----------\n");
            let mut scanner = PciScanner::new();
            if scanner.scan_all_buses().is_ok() {
                for dev in scanner.get_devices() {
                    let desc = pci_device_description(dev.class_code, dev.subclass);
                    let line = format!(
                        "{:<4}  {:<4} {:<4}  0x{:04x} 0x{:04x}  0x{:02x}       0x{:02x}       {}\n",
                        dev.bus,
                        dev.device,
                        dev.function,
                        dev.vendor_id,
                        dev.device_id,
                        dev.class_code,
                        dev.subclass,
                        desc,
                    );
                    ctx.terminal.write_str(&line);
                }
            } else {
                ctx.terminal.write_str("PCI scan failed.\n");
            }
        }
        "badapple" => {
            ctx.terminal
                .write_str("Playing Bad Apple!! (press any key to stop)...\n");
            crate::badapple::play_badapple();
            ctx.terminal.write_str("Bad Apple finished.\n");
        }
        _ => {
            let msg = format!("Unknown sys info command: {}\n", cmd);
            ctx.terminal.write_str(&msg);
        }
    });

    // Sys control hooks — theme/wallpaper/reboot/shutdown (via solvent bridges)
    nozzle::sys_hooks::set_sys_ctl_fn(|cmd| match cmd {
        "theme dark" => {
            solvent::set_theme(solvent::ThemeVariant::Dark);
            solvent::force_desktop_redraw();
        }
        "theme light" => {
            solvent::set_theme(solvent::ThemeVariant::Light);
            solvent::force_desktop_redraw();
        }
        "theme toggle" => {
            solvent::toggle_theme();
            solvent::force_desktop_redraw();
        }
        "wallpaper solid" => {
            solvent::set_wallpaper(solvent::WallpaperMode::SolidColor);
            solvent::force_desktop_redraw();
        }
        "wallpaper grid" => {
            solvent::set_wallpaper(solvent::WallpaperMode::GridPattern);
            solvent::force_desktop_redraw();
        }
        "wallpaper gradient" => {
            solvent::set_wallpaper(solvent::WallpaperMode::Gradient);
            solvent::force_desktop_redraw();
        }
        _ if cmd.starts_with("wallpaper ") => {
            let name = &cmd[10..];
            if let Some(idx) = solvent::find_preset(name) {
                solvent::set_wallpaper(solvent::WallpaperMode::Preset(idx));
                solvent::force_desktop_redraw();
            } else {
                solvent::write_terminal("wallpaper: preset not found\n");
            }
        }
        "reboot" => {
            petroleum::serial::serial_log(format_args!("Reboot requested via shell\n"));
            unsafe {
                let port: u16 = 0x64;
                while x86_64::instructions::port::PortReadOnly::<u8>::new(port).read() & 0x02 != 0 {
                }
                x86_64::instructions::port::PortWriteOnly::<u8>::new(port).write(0xFEu8);
            }
        }
        "shutdown" => {
            petroleum::serial::serial_log(format_args!("Shutdown requested via shell\n"));
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u16>::new(0x604).write(0x2000u16);
            }
            unsafe {
                let shutdown_str = b"Shutdown";
                let mut port = x86_64::instructions::port::PortWriteOnly::<u8>::new(0xB004);
                for &byte in shutdown_str {
                    port.write(byte);
                }
            }
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u16>::new(0x4004).write(0x3400u16);
            }
            loop {
                x86_64::instructions::hlt();
            }
        }
        _ => {}
    });
}

/// Main shell entry point — called from the scheduler as a kernel process.
pub fn shell_main() {
    petroleum::debug_log!("Shell main started");

    register_nozzle_hooks();

    if solvent::is_initialized() {
        solvent::run_shell_on(&mut solvent::LatticeTerminal, "fullerene> ");
    } else {
        solvent::run_shell_on(&mut KernelTerminal, "fullerene> ");
    }
}

// ── Kernel terminal ─────────────────────────────────────────────────

struct KernelTerminal;

impl nozzle::Terminal for KernelTerminal {
    fn write_str(&mut self, s: &str) {
        kernel_syscall(4, 1, s.as_ptr() as u64, s.len() as u64);
    }

    fn read_byte(&mut self) -> Option<u8> {
        loop {
            let mut byte = 0u8;
            let res = kernel_syscall(3, 0, &mut byte as *mut u8 as u64, 1);
            if res > 0 {
                return Some(byte);
            }
            kernel_syscall(22, 0, 0, 0);
        }
    }

    fn input_available(&self) -> bool {
        nitrogen::ps2::keyboard::input_available()
    }
}

// ── PCI device description helper ────────────────────────────────

fn pci_device_description(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, _) => "Pre-PCI 2.0 device",
        (0x01, 0x01) => "IDE Controller",
        (0x01, 0x06) => "SATA Controller (AHCI)",
        (0x01, 0x08) => "NVMe Controller",
        (0x01, 0x00) => "SCSI Controller",
        (0x01, _) => "Mass Storage Controller",
        (0x02, 0x00) => "Ethernet Controller",
        (0x02, _) => "Network Controller",
        (0x03, 0x00) => "VGA Compatible",
        (0x03, _) => "Display Controller",
        (0x04, 0x00) => "HDA Audio Device",
        (0x04, 0x01) => "AC97 Audio Device",
        (0x04, 0x03) => "HD Audio Controller",
        (0x04, _) => "Multimedia Controller",
        (0x06, 0x00) => "Host Bridge",
        (0x06, 0x01) => "ISA Bridge",
        (0x06, 0x04) => "PCI-to-PCI Bridge",
        (0x06, _) => "Bridge Device",
        (0x0C, 0x03) => "USB Controller (UHCI/OHCI/EHCI/XHCI)",
        (0x0C, _) => "Serial Bus Controller",
        (0x08, _) => "System Peripheral",
        _ => "Unknown PCI device",
    }
}
