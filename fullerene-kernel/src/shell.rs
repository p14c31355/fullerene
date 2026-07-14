//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel's raw syscall I/O.

use crate::syscall::kernel_syscall;
use alloc::format;
use alloc::string::String;

// ── WASM/WASI runtime callbacks ──────────────────────────────────

fn wasm_write_stdout(data: &[u8]) {
    if solvent::is_initialized() {
        let s = alloc::string::String::from_utf8_lossy(data);
        solvent::write_terminal(&s);
    } else {
        kernel_syscall(4, 1, data.as_ptr() as u64, data.len() as u64);
    }
}

fn wasm_write_stderr(data: &[u8]) {
    if solvent::is_initialized() {
        let s = alloc::string::String::from_utf8_lossy(data);
        solvent::write_terminal(&s);
    } else {
        kernel_syscall(4, 2, data.as_ptr() as u64, data.len() as u64);
    }
}

fn wasm_read_stdin() -> Option<u8> {
    let mut byte = 0u8;
    let res = kernel_syscall(3, 0, &mut byte as *mut u8 as u64, 1);
    if res > 0 { Some(byte) } else { None }
}

fn wasm_yield_now() {
    kernel_syscall(22, 0, 0, 0);
}

fn wasm_read_entire_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    crate::fs::read_entire_file(path).map_err(|e| match e {
        crate::fs::FsError::FileNotFound => "file not found",
        _ => "read failed",
    })
}

fn wasm_read_directory(path: &str) -> Result<alloc::vec::Vec<(alloc::string::String, u8)>, &'static str> {
    let entries = crate::contexts::vfs::readdir(path).map_err(|_| "readdir failed")?;
    Ok(entries
        .iter()
        .map(|e| {
            let ft = if e.is_dir {
                wasi_runtime::wasi::FILETYPE_DIRECTORY
            } else {
                wasi_runtime::wasi::FILETYPE_REGULAR_FILE
            };
            (e.name.clone(), ft)
        })
        .collect())
}

fn wasm_get_monotonic_ns() -> u64 {
    if solvent::is_initialized() {
        solvent::GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed) * 1_000_000
    } else {
        let tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let tsc_per_ms = solvent::get_tsc_per_ms().max(1);
        // Use u128 to prevent overflow while maintaining full precision
        ((tsc as u128 * 1_000_000) / tsc_per_ms as u128) as u64
    }
}

/// Helper: write a formatted line to the terminal.
macro_rules! tline {
    ($t:expr, $($arg:tt)*) => { $t.write_str(&alloc::format!("{}{}", alloc::format!($($arg)*), '\n')) };
}
/// Helper: write a static string + newline to the terminal.
macro_rules! tstr {
    ($t:expr, $s:expr) => {
        $t.write_str(concat!($s, '\n'))
    };
}
/// Helper: match a launch function returning Result<ProcessId, Err>, write
/// ok/error messages to the terminal.
macro_rules! launch_cmd {
    ($t:expr, $launch:expr, $ok:expr) => {
        match $launch {
            Ok(pid) => tline!($t, $ok, pid.0),
            Err(e) => tline!($t, "Failed to launch: {:?}", e),
        }
    };
}

/// Read the entire contents of a file at `path`. Returns the raw bytes.
/// Limited to MAX_FILE_SIZE to prevent unbounded memory growth.
fn read_entire_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    crate::fs::read_entire_file(path).map_err(|e| match e {
        crate::fs::FsError::FileNotFound => "file not found",
        _ => "read failed",
    })
}

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    nitrogen::ps2::keyboard::init_keyboard();
    register_nozzle_hooks();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

// ── Nozzle hook registration ──────────────────────────────────────

/// Register kernel implementations for nozzle's filesystem and system hooks.
fn register_nozzle_hooks() {
    // ── Install all FS hooks at once (single lock) ──────────────
    nozzle::fs_hooks::FsHooks {
        list: Some(|ctx| {
            let path = if ctx.args.len() > 1 && !ctx.args[1].starts_with('-') {
                ctx.args[1]
            } else {
                "."
            };
            let long_format = ctx.args.contains(&"-l");
            match crate::contexts::vfs::readdir(path) {
                Ok(entries) => {
                    for ent in entries {
                        if long_format {
                            tline!(
                                ctx.terminal,
                                "{}  {:>8}  {}",
                                if ent.is_dir { "d" } else { "-" },
                                ent.size,
                                ent.name
                            );
                        } else if ent.is_dir {
                            tline!(ctx.terminal, "  {}/", ent.name);
                        } else {
                            tline!(ctx.terminal, "  {}", ent.name);
                        }
                    }
                }
                Err(e) => {
                    tline!(ctx.terminal, "ls: {}: {}", path, e);
                }
            }
        }),
        read: Some(|ctx, path| match read_entire_file(path) {
            Ok(data) => {
                ctx.terminal
                    .write_str(core::str::from_utf8(&data).unwrap_or("(binary)"));
                if !data.is_empty() && data.last() != Some(&b'\n') {
                    ctx.terminal.write_str("\n");
                }
            }
            Err(e) => tline!(ctx.terminal, "cat: {}: {}", path, e),
        }),
        pwd: Some(|ctx| match crate::contexts::vfs::working_directory() {
            Ok(wd) => {
                tline!(ctx.terminal, "{}", wd);
            }
            Err(e) => {
                tline!(ctx.terminal, "pwd: {}", e);
            }
        }),
        cd: Some(|ctx, path| match crate::contexts::vfs::change_directory(path) {
            Ok(()) => {}
            Err(e) => {
                tline!(ctx.terminal, "cd: {}: {}", path, e);
            }
        }),
        tree: Some(|ctx, path| {
            let resolved = if path == "." {
                match crate::contexts::vfs::working_directory() {
                    Ok(wd) => wd,
                    Err(_) => String::from("/"),
                }
            } else {
                String::from(path)
            };
            match crate::fs::walk_dir(&resolved) {
                Ok(entries) => {
                    for entry in &entries {
                        tline!(ctx.terminal, "{}", entry);
                    }
                }
                Err(e) => {
                    tline!(ctx.terminal, "tree: {}: {}", resolved, e);
                }
            }
        }),
        find: Some(|ctx, path, pattern| {
            let resolved = if path == "." {
                crate::contexts::vfs::working_directory().unwrap_or("/".into())
            } else {
                String::from(path)
            };
            match crate::fs::walk_dir(&resolved) {
                Ok(entries) => {
                    let mut found = false;
                    for entry in &entries {
                        if entry.contains(pattern) {
                            tline!(ctx.terminal, "{}", entry);
                            found = true;
                        }
                    }
                    if !found {
                        ctx.terminal.write_str("(no matches)\n");
                    }
                }
                Err(e) => {
                    tline!(ctx.terminal, "find: {}: {}", resolved, e);
                }
            }
        }),
        cp: Some(|ctx, src, dst| match crate::fs::copy_file(src, dst) {
            Ok(()) => {
                tline!(ctx.terminal, "Copied {} -> {}", src, dst);
            }
            Err(e) => {
                tline!(ctx.terminal, "cp: {} -> {}: {}", src, dst, e);
            }
        }),
        mv: Some(|ctx, src, dst| match crate::fs::move_file(src, dst) {
            Ok(()) => {
                tline!(ctx.terminal, "Moved {} -> {}", src, dst);
            }
            Err(e) => {
                tline!(ctx.terminal, "mv: {} -> {}: {}", src, dst, e);
            }
        }),
        write: Some(|ctx, path, content| {
            match crate::fs::write_entire_file(path, content.as_bytes()) {
                Ok(()) => {
                    tline!(ctx.terminal, "Wrote {} bytes to {}", content.len(), path);
                }
                Err(e) => {
                    tline!(ctx.terminal, "write: {}: {}", path, e);
                }
            }
        }),
        rm: Some(|ctx, path| match crate::fs::remove(path) {
            Ok(()) => {
                tline!(ctx.terminal, "Removed {}", path);
            }
            Err(e) => {
                tline!(ctx.terminal, "rm: {}: {}", path, e);
            }
        }),
        mkdir: Some(|ctx, path| match crate::contexts::vfs::mkdir(path) {
            Ok(()) => {
                tline!(ctx.terminal, "Created directory {}", path);
            }
            Err(e) => {
                tline!(ctx.terminal, "mkdir: {}: {}", path, e);
            }
        }),
        touch: Some(|ctx, path| match crate::contexts::vfs::open(path, 0) {
            Ok(fd) => {
                let _ = crate::contexts::vfs::close(fd.fd);
                tline!(ctx.terminal, "Touched {}", path);
            }
            Err(_) => match crate::contexts::vfs::create(path) {
                Ok(fd) => {
                    let _ = crate::contexts::vfs::close(fd.fd);
                    tline!(ctx.terminal, "Touched {}", path);
                }
                Err(e) => {
                    tline!(ctx.terminal, "touch: {}: {}", path, e);
                }
            },
        }),
        df: Some(|ctx| {
            match crate::fs::walk_dir("/") {
                Ok(entries) => {
                    let mut file_count = 0;
                    let mut dir_count = 0;
                    // Check each entry's type by querying its parent directory
                    for path in &entries {
                        if let Some(pos) = path.rfind('/') {
                            let parent = if pos == 0 { "/" } else { &path[..pos] };
                            let name = &path[pos + 1..];
                            if let Ok(parent_entries) = crate::fs::list_dir(parent) {
                                if let Some(entry) = parent_entries.iter().find(|e| e.name == name)
                                {
                                    if entry.is_dir {
                                        dir_count += 1;
                                    } else {
                                        file_count += 1;
                                    }
                                }
                            }
                        }
                    }
                    ctx.terminal
                        .write_str("Filesystem      Size  Used  Avail  Use%  Mounted on\n");
                    let msg = format!(
                        "ramfs           {:>4}K  {:>4}K  {:>4}K  {:>3}%  /\n",
                        0, 0, 0, 0
                    );
                    ctx.terminal.write_str(&msg);
                    let msg2 = format!("{} files, {} directories\n", file_count, dir_count);
                    ctx.terminal.write_str(&msg2);
                }
                Err(e) => {
                    let msg = format!("df: {}\n", e);
                    ctx.terminal.write_str(&msg);
                }
            }
        }),
    }
    .install();

    // ── Install sys info / control hooks ───────────────────────
    nozzle::sys_hooks::MOUNT_HOOK
        .lock()
        .replace(|ctx: &mut nozzle::CommandContext| {
            if ctx.args.len() < 3 {
                ctx.terminal.write_str("Usage: mount /dev/<device> <mount_point>\n");
                ctx.terminal.write_str("Available devices:\n");
                for name in crate::devfs::list_block_device_names() {
                    tline!(ctx.terminal, "    /dev/{}", name);
                }
                return;
            }
            let (device, mount_point) = (ctx.args[1], ctx.args[2]);
            match crate::contexts::vfs::mount(device, mount_point, "fat32") {
                Ok(()) => {
                    tline!(ctx.terminal, "mount: OK — {} mounted at {}", device, mount_point);
                    let _ = crate::klog::flush_to_vfs();
                }
                Err(e) => {
                    tline!(ctx.terminal, "mount: {}: {:?}", device, e);
                }
            }
        });

    nozzle::sys_hooks::SysHooks {
        info: Some(|ctx, cmd| match cmd {
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
            let style = solvent::current_style();
            let variant = solvent::current_theme_variant();
            let style_name = match style {
                solvent::ThemeStyle::Classic => "classic",
                solvent::ThemeStyle::Modern => "modern",
            };
            let var_name = match variant {
                solvent::ThemeVariant::Dark => "dark",
                solvent::ThemeVariant::Light => "light",
            };
            let msg = format!("Style: {}  Variant: {}\n", style_name, var_name);
            ctx.terminal.write_str(&msg);
            ctx.terminal.write_str(
                "Usage: theme ( classic | modern | dark | light | toggle | toggle-style )\n",
            );
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
                .write_str("Usage: wallpaper solid | grid | gradient | beach | mountain | city | fullerene | fullerene-sharp\n");
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
                crate::klog::write_to(|s| ctx.terminal.write_str(s));
                ctx.terminal.write_str("\n=== End kernel log ===\n");
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
            let events = resonance::tracing::snapshot();
            if events.is_empty() {
                ctx.terminal.write_str("(no trace events recorded)\n");
            } else {
                let mut buf = alloc::string::String::with_capacity(events.len() * 48);
                for ev in events {
                    let cat = core::str::from_utf8(&ev.category)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    let msg = core::str::from_utf8(&ev.message)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    use core::fmt::Write;
                    let _ = write!(buf, "[{}] {}: {}\n", ev.tick, cat, msg);
                }
                ctx.terminal.write_str(&buf);
            }
        }
        "run" => {
            ctx.terminal.write_str("Usage: run <app_name>\n");
            ctx.terminal.write_str("Available: toluene, hello\n");
        }
        "linux_run" => {
            if ctx.args.len() <= 1 { return tstr!(ctx.terminal, "Usage: linux_run <path>"); }
            tline!(ctx.terminal, "Loading Linux binary: {}", ctx.args[1]);
            launch_cmd!(ctx.terminal, crate::linux::launch::launch_linux_binary(ctx.args[1]), "Linux process started (PID: {})");
        }
        "run_busybox" => launch_cmd!(ctx.terminal, crate::linux::launch::launch_busybox(), "BusyBox shell started (PID: {})"),
        "hello_linux" => launch_cmd!(ctx.terminal, crate::linux::launch::launch_test_binary(), "Test Linux binary started (PID: {})"),
        "wasm" => {
            if ctx.args.len() <= 1 { return tstr!(ctx.terminal, "Usage: wasm <path> [args...]"); }
            let path = ctx.args[1];
            tline!(ctx.terminal, "Loading WASM binary: {}", path);
            match crate::fs::read_entire_file(path) {
                Ok(binary) => {
                    let wasm_args: alloc::vec::Vec<&str> = ctx.args.iter().skip(1).copied().collect();
                    let code = wasi_runtime::runtime::run(
                        &binary,
                        &wasm_args,
                        wasm_write_stdout,
                        wasm_write_stderr,
                        wasm_read_stdin,
                        wasm_yield_now,
                        wasm_read_entire_file,
                        wasm_read_directory,
                        wasm_get_monotonic_ns,
                    );
                    tline!(ctx.terminal, "WASI process exited with code {}", code);
                }
                Err(e) => {
                    let err_str = match e {
                        crate::fs::FsError::FileNotFound => "file not found",
                        _ => "read failed",
                    };
                    tline!(ctx.terminal, "wasm: {}: {}", path, err_str);
                }
            }
        }
        "usb_info" => {
            use crate::drivers::registry;
            let count = crate::devfs::list_block_device_names().len();
            tline!(ctx.terminal, "Registered block devices: {}", count);
            tline!(ctx.terminal, "Registered /dev/ entries:");
            for name in crate::devfs::list_block_device_names() {
                tline!(ctx.terminal, "  /dev/{}", name);
            }
            // Also show full USB context status
            registry::with_ctx(|ctx_usb| {
                tline!(ctx.terminal, "USBContext: {} disk(s) enumerated", ctx_usb.disks().len());
                for disk in ctx_usb.disks() {
                    tline!(ctx.terminal, "  ctrl={} dev_addr={} ep_out=0x{:02x} ep_in=0x{:02x} blk_size={} total_blocks={}",
                        disk.ctrl_type, disk.dev_addr, disk.ep_out, disk.ep_in, disk.block_size, disk.total_blocks);
                }
            });
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
        "date" => {
            let cb = solvent::SOLVENT_CALLBACKS.lock().wall_clock;
            match cb.and_then(|f| f()) {
                Some((y, mo, d, h, mi, s)) => tline!(ctx.terminal, "{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s),
                None => tstr!(ctx.terminal, "date: RTC not available"),
            }
        }
        "uptime" => {
            let seconds = solvent::GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed) / 1000;
            let days = seconds / 86400;
            let hms = (seconds % 86400) / 3600;
            let mins = (seconds % 3600) / 60;
            let secs = seconds % 60;
            if days > 0 { tline!(ctx.terminal, "up {} days {:02}:{:02}:{:02}", days, hms, mins, secs); }
            else { tline!(ctx.terminal, "up {:02}:{:02}:{:02}", hms, mins, secs); }
        }
        "sleep" => {
            if ctx.args.len() > 1 {
                if let Ok(secs) = ctx.args[1].parse::<u64>() {
                    let tsc_per_ms = solvent::get_tsc_per_ms();
                    let total_ticks = tsc_per_ms.saturating_mul(secs.saturating_mul(1000));
                    let start = unsafe { core::arch::x86_64::_rdtsc() };
                    // Yield via HLT-hinted syscall periodically to avoid
                    // starving other tasks during the wait.
                    let mut last_yield = start;
                    let yield_interval = tsc_per_ms.saturating_mul(10); // every ~10 ms
                    loop {
                        let now = unsafe { core::arch::x86_64::_rdtsc() };
                        if now.wrapping_sub(start) >= total_ticks {
                            break;
                        }
                        if now.wrapping_sub(last_yield) >= yield_interval {
                            crate::syscall::kernel_syscall(22, 0, 0, 0);
                            last_yield = now;
                        }
                        core::hint::spin_loop();
                    }
                } else {
                    ctx.terminal.write_str("sleep: invalid number of seconds\n");
                }
            }
        }
        "grep" => {
            if ctx.args.len() < 3 { return tstr!(ctx.terminal, "grep: pattern and file required"); }
            let pattern = ctx.args[1];
            let show_filename = ctx.args.len() > 3;
            for &path in &ctx.args[2..] {
                match read_entire_file(path) {
                    Ok(data) => {
                        let text = alloc::string::String::from_utf8_lossy(&data);
                        for line in text.lines().filter(|l| l.contains(pattern)) {
                            if show_filename { ctx.terminal.write_str(&alloc::format!("{}:", path)); }
                            tline!(ctx.terminal, "{}", line);
                        }
                    }
                    Err(e) => tline!(ctx.terminal, "grep: {}: {}", path, e),
                }
            }
        }
        "sort" => {
            let reverse = ctx.args.contains(&"-r");
            let path_idx = if ctx.args.len() > 1 && ctx.args[1] == "-r" { 2 } else { 1 };
            if path_idx >= ctx.args.len() {
                return tstr!(ctx.terminal, "Usage: sort [-r] <file>");
            }
            match read_entire_file(ctx.args[path_idx]) {
                Ok(data) => {
                    let text = alloc::string::String::from_utf8_lossy(&data);
                    let mut lines: alloc::vec::Vec<&str> = text.lines().collect();
                    lines.sort();
                    if reverse { lines.reverse(); }
                    for line in lines { tline!(ctx.terminal, "{}", line); }
                }
                Err(e) => tline!(ctx.terminal, "sort: {}: {}", ctx.args[path_idx], e),
            }
        }
        "wc" => {
            if ctx.args.len() <= 1 { return tstr!(ctx.terminal, "Usage: wc <file>"); }
            match read_entire_file(ctx.args[1]) {
                Ok(data) => {
                    let text = alloc::string::String::from_utf8_lossy(&data);
                    let lines = data.iter().filter(|&&b| b == b'\n').count();
                    let words = text.split_whitespace().count();
                    tline!(ctx.terminal, "{} {} {} {}", lines, words, data.len(), ctx.args[1]);
                }
                Err(e) => tline!(ctx.terminal, "wc: {}: {}", ctx.args[1], e),
            }
        }
        "app_list" => match crate::fs::list_packages() {
            Ok(pkgs) => {
                if pkgs.is_empty() {
                    ctx.terminal.write_str("No packages installed.\n");
                } else {
                    ctx.terminal
                        .write_str("NAME         VERSION  DESCRIPTION\n");
                    ctx.terminal
                        .write_str("-----------  -------  -----------\n");
                    for p in &pkgs {
                        let line = format!("{:<12} {:<8} {}\n", p.name, p.version, p.description);
                        ctx.terminal.write_str(&line);
                    }
                }
            }
            Err(e) => {
                let msg = format!("app list: {}\n", e);
                ctx.terminal.write_str(&msg);
            }
        },
        _ => {
            let msg = format!("Unknown sys info command: {}\n", cmd);
            ctx.terminal.write_str(&msg);
        }
        }),
        ctl: Some(|cmd| match cmd {
        "theme dark" => { solvent::set_theme(solvent::ThemeVariant::Dark); solvent::force_desktop_redraw(); }
        "theme light" => { solvent::set_theme(solvent::ThemeVariant::Light); solvent::force_desktop_redraw(); }
        "theme toggle" => { solvent::toggle_theme(); solvent::force_desktop_redraw(); }
        "theme toggle-style" => { solvent::toggle_style(); solvent::force_desktop_redraw(); }
        "theme classic" => { solvent::set_style(solvent::ThemeStyle::Classic); solvent::force_desktop_redraw(); }
        "theme modern" => { solvent::set_style(solvent::ThemeStyle::Modern); solvent::force_desktop_redraw(); }
        "wallpaper solid" => { solvent::set_wallpaper(solvent::WallpaperMode::SolidColor); solvent::force_desktop_redraw(); }
        "wallpaper grid" => { solvent::set_wallpaper(solvent::WallpaperMode::GridPattern); solvent::force_desktop_redraw(); }
        "wallpaper gradient" => { solvent::set_wallpaper(solvent::WallpaperMode::Gradient); solvent::force_desktop_redraw(); }
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
        _ if cmd.starts_with("app_install ") => {
            let rest = &cmd[12..]; // skip "app_install " (12 characters)
            if let Some((name, desc)) = rest.split_once(' ') {
                let dummy_bin: [u8; 4] = [0x90, 0x90, 0x90, 0x90]; // NOP placeholder
                match crate::fs::install_package(name, "0.1.0", desc, &dummy_bin) {
                    Ok(()) => {
                        let msg = format!("Installed package '{}'\n", name);
                        solvent::write_terminal(&msg);
                    }
                    Err(e) => {
                        let msg = format!("app install: {}\n", e);
                        solvent::write_terminal(&msg);
                    }
                }
            }
        }
        _ if cmd.starts_with("app_remove ") => {
            let name = &cmd[11..]; // skip "app_remove " (11 characters)
            match crate::fs::remove_package(name) {
                Ok(()) => {
                    let msg = format!("Removed package '{}'\n", name);
                    solvent::write_terminal(&msg);
                }
                Err(e) => {
                    let msg = format!("app remove: {}\n", e);
                    solvent::write_terminal(&msg);
                }
            }
        }
        _ => {}
    }),
}
.install();
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
