//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel's raw syscall I/O.

use crate::syscall::kernel_syscall;
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

const MAX_WASM_OUTPUT_BYTES: usize = 256 * 1024;
static WASM_OUTPUT: Mutex<Option<String>> = Mutex::new(None);
static LAST_WASM_OUTPUT_REFRESH: AtomicU64 = AtomicU64::new(u64::MAX);

fn buffer_wasm_output(data: &[u8]) {
    let mut output = WASM_OUTPUT.lock();
    let Some(output) = output.as_mut() else {
        return;
    };
    if output.len() >= MAX_WASM_OUTPUT_BYTES {
        return;
    }
    let remaining = MAX_WASM_OUTPUT_BYTES - output.len();
    let converted = String::from_utf8_lossy(data);
    let truncated = if converted.len() <= remaining {
        converted.as_ref()
    } else {
        // Truncate at a valid UTF-8 char boundary
        let mut end = remaining;
        while end > 0 && !converted.is_char_boundary(end) {
            end -= 1;
        }
        &converted[..end]
    };
    output.push_str(truncated);
}

fn begin_wasm_output_capture() {
    *WASM_OUTPUT.lock() = Some(String::new());
}

fn take_wasm_output() -> Option<String> {
    WASM_OUTPUT.lock().take()
}

// ── WASM/WASI runtime callbacks ──────────────────────────────────

/// Synchronous WASM normally prevents the event loop from repainting Klog
/// Live. Force one repaint after each diagnostic marker so a blocked command
/// remains observable without serial access.
fn wasm_diag_refresh() {
    if solvent::is_initialized() {
        solvent::mark_klog_live_dirty();
        crate::gui::render();
    }
}

fn wasm_diag_refresh_throttled() {
    if !solvent::is_initialized() {
        return;
    }
    let now = solvent::GLOBAL_TICK.load(Ordering::Relaxed);
    let last = LAST_WASM_OUTPUT_REFRESH.load(Ordering::Relaxed);
    if last != u64::MAX && now.wrapping_sub(last) < 5 {
        return;
    }
    LAST_WASM_OUTPUT_REFRESH.store(now, Ordering::Relaxed);
    solvent::mark_klog_live_dirty();
    crate::gui::render();
}

fn wasm_status(message: &str) {
    crate::klog_fmt!("[WASM-DIAG] status {}\n", message);
    nitrogen::debug_status!("WASM", "{}", message);
    wasm_diag_refresh();
}

fn wasm_write_stdout(data: &[u8]) {
    if let Ok(text) = core::str::from_utf8(data) {
        crate::klog_fmt!("[WASM-DIAG] stdout {}", text);
        wasm_diag_refresh_throttled();
    }
    if solvent::is_initialized() {
        buffer_wasm_output(data);
    } else {
        kernel_syscall(4, 1, data.as_ptr() as u64, data.len() as u64);
    }
}

fn wasm_write_stderr(data: &[u8]) {
    if let Ok(text) = core::str::from_utf8(data) {
        crate::klog_fmt!("[WASM-DIAG] stderr {}", text);
        wasm_diag_refresh_throttled();
    }
    if solvent::is_initialized() {
        buffer_wasm_output(data);
    } else {
        kernel_syscall(4, 2, data.as_ptr() as u64, data.len() as u64);
    }
}

fn wasm_read_stdin() -> Option<u8> {
    if solvent::is_initialized() {
        // The GUI shell reads from the PS/2 queue directly.  The kernel
        // read syscall is for user processes and must not be called while a
        // WASI module is running synchronously inside shell_main.
        return nitrogen::ps2::keyboard::read_char();
    }
    let mut byte = 0u8;
    let res = kernel_syscall(3, 0, &mut byte as *mut u8 as u64, 1);
    if res > 0 { Some(byte) } else { None }
}

fn wasm_yield_now() {
    if solvent::is_initialized() {
        // WASM is executed synchronously by the kernel shell. Poll devices
        // here, but do not re-enter the GUI scheduler from a host callback.
        solvent::poll_mouse_state();
        solvent::poll_keyboard();
    } else {
        kernel_syscall(22, 0, 0, 0);
    }
}

fn wasm_file_size(path: &str) -> Result<u64, genome::FsError> {
    crate::klog_fmt!("[WASM-DIAG] file_size begin path={}\n", path);
    let result = crate::fs::file_size(path);
    crate::klog_fmt!(
        "[WASM-DIAG] file_size end path={} result={:?}\n",
        path,
        result
    );
    result
}

fn wasm_read_file_range(
    path: &str,
    offset: u64,
    limit: usize,
) -> Result<alloc::vec::Vec<u8>, genome::FsError> {
    crate::klog_fmt!(
        "[WASM-DIAG] read_range callback begin path={} offset={} limit={}\n",
        path,
        offset,
        limit
    );
    let result = crate::fs::read_file_range(path, offset, limit);
    crate::klog_fmt!(
        "[WASM-DIAG] read_range callback end path={} offset={} result={:?}\n",
        path,
        offset,
        result.as_ref().map(alloc::vec::Vec::len)
    );
    result
}

fn wasm_write_file(path: &str, data: &[u8]) -> Result<(), genome::FsError> {
    crate::fs::write_entire_file(path, data)
}

fn wasm_write_file_chunk(
    path: &str,
    offset: u64,
    data: &[u8],
    replace: bool,
) -> Result<(), genome::FsError> {
    crate::klog_fmt!(
        "[WASM-DIAG] file chunk enter path={} offset={} bytes={} replace={}\n",
        path,
        offset,
        data.len(),
        replace
    );
    let result = crate::fs::write_file_chunk(path, offset, data, replace);
    crate::klog_fmt!(
        "[WASM-DIAG] file chunk exit path={} offset={} result={:?}\n",
        path,
        offset,
        result
    );
    result
}

fn wasm_read_directory(
    path: &str,
) -> Result<alloc::vec::Vec<(alloc::string::String, u8)>, genome::FsError> {
    let entries = crate::contexts::vfs::readdir(path)?;
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

fn blit_rgb(window_id: lattice::window::WindowId, width: u32, height: u32, pixels: &[u8]) -> i32 {
    if pixels.len() < 3 {
        return -1;
    }
    let img_w = width as usize;
    wasm_status("surface blit enter");
    let updated = solvent::with_window_surface(window_id, |surf_pixels, surf_w, surf_h| {
        let draw_h = (height as usize).min(surf_h as usize);
        let draw_w = (width as usize).min(surf_w as usize);
        for y in 0..draw_h {
            for x in 0..draw_w {
                let Some(src) = y
                    .checked_mul(img_w)
                    .and_then(|offset| offset.checked_add(x))
                    .and_then(|pixel| pixel.checked_mul(3))
                else {
                    return;
                };
                let Some(end) = src.checked_add(3) else {
                    return;
                };
                if let Some(rgb) = pixels.get(src..end) {
                    let color = (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
                    surf_pixels[y * surf_w as usize + x] = color;
                }
            }
        }
    });
    if updated.is_none() {
        wasm_status("surface blit no surface");
        return -1;
    }
    wasm_status("surface blit exit");
    wasm_status("window invalidate enter");
    solvent::invalidate_window(window_id);
    wasm_status("window invalidate exit");
    0
}

fn wasm_show_image(width: u32, height: u32, pixels: &[u8]) -> i32 {
    let message = alloc::format!(
        "show_image enter {}x{} bytes={}",
        width,
        height,
        pixels.len()
    );
    wasm_status(&message);
    if !solvent::is_initialized() || pixels.len() < 3 {
        wasm_status("show_image rejected");
        return -1;
    }
    let win_w = width.min(800).max(160);
    let win_h = height.min(600).max(120);
    wasm_status("create_window enter");
    let Some(id) = solvent::create_window("Image Viewer", 120, 80, win_w, win_h) else {
        wasm_status("create_window failed");
        return -1;
    };
    wasm_status("create_window exit");
    blit_rgb(id, width, height, pixels)
}

fn wasm_show_text(title: &str, text: &str) -> i32 {
    if solvent::is_initialized() {
        solvent::show_text_window(title, text);
    } else {
        let header = alloc::format!("--- {} ---\n", title);
        solvent::write_terminal(&header);
        solvent::write_terminal(text);
        solvent::write_terminal("\n--- end ---\n");
    }
    0
}

fn wasm_show_error(title: &str, msg: &str) -> i32 {
    if solvent::is_initialized() {
        solvent::show_text_window(title, msg);
    } else {
        let header = alloc::format!("[!] {}: ", title);
        solvent::write_terminal(&header);
        solvent::write_terminal(msg);
        solvent::write_terminal("\n");
    }
    0
}

fn wasm_create_window(title: &str, width: u32, height: u32) -> i32 {
    if !solvent::is_initialized() {
        return -1;
    }
    let Some(id) = solvent::create_window(
        title,
        120,
        80,
        width.min(800).max(160),
        height.min(600).max(120),
    ) else {
        return -1;
    };
    i32::try_from(id.0).unwrap_or(-1)
}

fn wasm_update_window(window_id: i32, width: u32, height: u32, pixels: &[u8]) -> i32 {
    if !solvent::is_initialized() {
        return -1;
    }
    if window_id < 0 {
        return -1;
    }
    let id = lattice::window::WindowId(window_id as u64);
    blit_rgb(id, width, height, pixels)
}

fn wasm_close_window(window_id: i32) -> i32 {
    if solvent::close_window(lattice::window::WindowId(window_id as u64)) {
        0
    } else {
        -1
    }
}

const WASM_CAPTURE_MAX_WIDTH: u32 = 1920;
const WASM_CAPTURE_MAX_HEIGHT: u32 = 1080;

fn wasm_screen_dimensions() -> (u32, u32) {
    let dimensions =
        solvent::scaled_framebuffer_dims(WASM_CAPTURE_MAX_WIDTH, WASM_CAPTURE_MAX_HEIGHT);
    crate::klog_fmt!(
        "[WASM-DIAG] screen dimensions {}x{}\n",
        dimensions.0,
        dimensions.1
    );
    dimensions
}

fn wasm_capture_screen() -> Option<(u32, u32, alloc::vec::Vec<u8>)> {
    wasm_status("capture_screen enter");
    crate::klog_fmt!("[WASM-DIAG] capture host callback enter\n");
    let result = solvent::capture_screen_scaled(WASM_CAPTURE_MAX_WIDTH, WASM_CAPTURE_MAX_HEIGHT);
    match &result {
        Some((width, height, pixels)) => wasm_status(&alloc::format!(
            "capture_screen exit {}x{} bytes={}",
            width,
            height,
            pixels.len()
        )),
        None => wasm_status("capture_screen unavailable (back buffer busy or missing)"),
    }
    crate::klog_fmt!(
        "[WASM-DIAG] capture host callback exit available={}\n",
        result.is_some()
    );
    result
}

fn wasm_capture_screen_chunk(offset: u32, pixels: &mut [u8]) -> Option<(u32, u32)> {
    crate::klog_fmt!(
        "[WASM-DIAG] capture chunk enter offset={} bytes={}\n",
        offset,
        pixels.len()
    );
    let result = solvent::capture_screen_chunk(
        WASM_CAPTURE_MAX_WIDTH,
        WASM_CAPTURE_MAX_HEIGHT,
        offset as usize,
        pixels,
    );
    crate::klog_fmt!(
        "[WASM-DIAG] capture chunk exit offset={} result={:?}\n",
        offset,
        result
    );
    result
}

/// Run a WASI application from the kernel without opening a shell window.
///
/// This is also used by the desktop file viewer. The shell `wasm` command
/// remains as a user-facing entry point, but both paths share the same
/// runtime setup and host callbacks.
pub fn run_wasm_app(path: &str, args: &[&str]) -> i32 {
    crate::klog_fmt!("[WASM-DIAG] run begin path={} argc={}\n", path, args.len());
    wasm_diag_refresh();
    let binary = match crate::fs::read_entire_file(path) {
        Ok(binary) => binary,
        Err(error) => {
            if solvent::is_initialized() {
                let message = alloc::format!("wasm: {}: {:?}\n", path, error);
                solvent::write_terminal(&message);
            }
            return -1;
        }
    };
    let (len, fingerprint, edges) = crate::fs::diagnostic_fingerprint(&binary);
    crate::klog_fmt!(
        "[WASM-DIAG] run binary path={} len={} fnv=0x{:016x} edges={}\n",
        path,
        len,
        fingerprint,
        edges
    );
    wasm_diag_refresh();

    let capture_output = solvent::is_initialized();
    if capture_output {
        begin_wasm_output_capture();
    }
    let code = wasi_runtime::runtime::run(
        &binary,
        args,
        wasm_write_stdout,
        wasm_write_stderr,
        wasm_read_stdin,
        wasm_yield_now,
        wasm_file_size,
        wasm_read_file_range,
        wasm_read_directory,
        wasm_write_file,
        wasm_write_file_chunk,
        wasm_get_monotonic_ns,
        wasm_screen_dimensions,
        wasm_capture_screen,
        wasm_capture_screen_chunk,
        wasm_show_image,
        wasm_show_text,
        wasm_show_error,
        wasm_create_window,
        wasm_update_window,
        wasm_close_window,
    );
    if capture_output && let Some(output) = take_wasm_output() {
        solvent::write_terminal(&output);
    }
    crate::klog_fmt!("[WASM-DIAG] run end path={} code={}\n", path, code);
    wasm_diag_refresh();
    code
}

/// Helper: write a formatted line to the terminal.
macro_rules! tline {
    ($t:expr, $($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut line = alloc::string::String::new();
        let _ = writeln!(&mut line, $($arg)*);
        $t.write_str(&line)
    }};
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
    ($t:expr, $launch:expr, $ok:expr) => {{
        crate::klog_fmt!("[LINUX-DIAG] launch command call enter\n");
        match $launch {
            Ok(pid) => {
                crate::klog_fmt!(
                    "[LINUX-DIAG] launch command created pid={} deferred-yield\n",
                    pid.0
                );
                tline!($t, $ok, pid.0);
                // The terminal input loop performs this direct handoff after
                // the callback returns, so no shell/runtime lock crosses the
                // context-switch assembly boundary.
                crate::process::defer_yield_to(pid);
                crate::klog_fmt!("[LINUX-DIAG] launch command returned pid={} ready\n", pid.0);
            }
            Err(e) => {
                crate::klog_fmt!("[LINUX-DIAG] launch command error={:?}\n", e);
                tline!($t, "Failed to launch: {:?}", e)
            }
        }
    }};
}

/// Read the entire contents of a file at `path`. Returns the raw bytes.
/// Limited to MAX_FILE_SIZE to prevent unbounded memory growth.
fn read_entire_file(path: &str) -> Result<alloc::vec::Vec<u8>, genome::FsError> {
    crate::fs::read_entire_file(path)
}

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    nitrogen::ps2::keyboard::init_keyboard();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

// ── Nozzle service construction ───────────────────────────────────

/// Build the immutable kernel services injected into one Nozzle session.
fn nozzle_services() -> nozzle::ShellServices {
    let fs = nozzle::fs_hooks::FsHooks {
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
        cd: Some(
            |ctx, path| match crate::contexts::vfs::change_directory(path) {
                Ok(()) => {}
                Err(e) => {
                    tline!(ctx.terminal, "cd: {}: {}", path, e);
                }
            },
        ),
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
    };

    let mount: Option<fn(&mut nozzle::CommandContext)> =
        Some(|ctx: &mut nozzle::CommandContext| {
            if ctx.args.len() < 3 {
                ctx.terminal
                    .write_str("Usage: mount /dev/<device> <mount_point>\n");
                ctx.terminal.write_str("Available devices:\n");
                for name in crate::devfs::list_block_device_names() {
                    tline!(ctx.terminal, "    /dev/{}", name);
                }
                return;
            }
            let (device, mount_point) = (ctx.args[1], ctx.args[2]);
            match crate::contexts::vfs::mount(device, mount_point, "auto") {
                Ok(()) => {
                    tline!(
                        ctx.terminal,
                        "mount: OK — {} mounted at {}",
                        device,
                        mount_point
                    );
                    let _ = crate::klog::flush_to_vfs();
                }
                Err(e) => {
                    tline!(ctx.terminal, "mount: {}: {:?}", device, e);
                }
            }
        });

    let sys = nozzle::sys_hooks::SysHooks {
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
            "metrics" => {
                ctx.terminal.write_str(&crate::metrics::format_snapshot());
            }
            "cpuinfo" => {
                ctx.terminal.write_str(&crate::smp::format_topology());
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
                if let Some(ref manager) =
                    *crate::hardware::device_manager::get_device_manager().lock()
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
                            let line =
                                format!("{:<16}  {:<10}  {}\n", d.name, d.device_type, status);
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
                if ctx.args.len() <= 1 {
                    return tstr!(ctx.terminal, "Usage: linux_run <path>");
                }
                tline!(ctx.terminal, "Loading Linux binary: {}", ctx.args[1]);
                launch_cmd!(
                    ctx.terminal,
                    crate::linux::launch::launch_linux_binary(ctx.args[1]),
                    "Linux process started (PID: {})"
                );
            }
            "run_busybox" => launch_cmd!(
                ctx.terminal,
                crate::linux::launch::launch_busybox(),
                "BusyBox shell started (PID: {})"
            ),
            "hello_linux" => launch_cmd!(
                ctx.terminal,
                crate::linux::launch::launch_test_binary(),
                "Test Linux binary started (PID: {})"
            ),
            "hello_rust_linux" => {
                crate::klog_fmt!("[LINUX-DIAG] hello_rust command enter\n");
                #[cfg(have_linux_musl_hello)]
                launch_cmd!(
                    ctx.terminal,
                    crate::linux::launch::launch_rust_std_hello(),
                    "Rust std/musl Linux process started (PID: {})"
                );
                #[cfg(not(have_linux_musl_hello))]
                ctx.terminal.write_str(
                    "Rust std/musl fixture is unavailable. Run \
                     `rustup target add --toolchain nightly x86_64-unknown-linux-musl`, \
                     then rebuild the ISO.\n",
                );
            }
            "wasm" => {
                if ctx.args.len() <= 1 {
                    return tstr!(ctx.terminal, "Usage: wasm <path> [args...]");
                }
                let path = ctx.args[1];
                let wasm_args: alloc::vec::Vec<&str> = ctx.args.iter().skip(1).copied().collect();
                tline!(ctx.terminal, "Loading WASM binary: {}", path);
                let code = run_wasm_app(path, &wasm_args);
                tline!(ctx.terminal, "WASI process exited with code {}", code);
            }
            "emulsion" => {
                let wasm_args = if ctx.args.len() > 1 {
                    ctx.args[1..].to_vec()
                } else {
                    alloc::vec!["capture"]
                };
                let mut args = alloc::vec!["/apps/emulsion.wasm"];
                args.extend(wasm_args);
                let code = run_wasm_app("/apps/emulsion.wasm", &args);
                tline!(ctx.terminal, "Emulsion exited with code {}", code);
            }
            "usb_rescan" => {
                ctx.terminal.write_str(
                "USB rescan: explicitly activating controller MMIO; this may not return on broken hardware.\n",
            );
                if crate::drivers::registry::rescan_usb_all() {
                    ctx.terminal
                        .write_str("USB rescan: storage device registered.\n");
                } else {
                    ctx.terminal
                        .write_str("USB rescan: no storage device registered.\n");
                }
            }
            "sd_rescan" => {
                #[cfg(not(nitrogen_no_storage))]
                {
                    use crate::drivers::registry::SdRescanResult;
                    match crate::drivers::registry::rescan_sd() {
                        SdRescanResult::Registered => {
                            ctx.terminal.write_str("SD rescan: /dev/sd0 registered.\n")
                        }
                        SdRescanResult::AlreadyRegistered => ctx
                            .terminal
                            .write_str("SD rescan: /dev/sd0 is already ready.\n"),
                        SdRescanResult::Mounted => ctx
                            .terminal
                            .write_str("SD rescan: /dev/sd0 is mounted; keeping it online.\n"),
                        SdRescanResult::Unavailable => ctx
                            .terminal
                            .write_str("SD rescan: no usable card; see dmesg for details.\n"),
                    }
                }
                #[cfg(nitrogen_no_storage)]
                {
                    ctx.terminal
                        .write_str("SD rescan: storage support not compiled in.\n");
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
                // Also show full USB context status without assuming a controller exists.
                if registry::try_with_ctx(|ctx_usb| {
                tline!(ctx.terminal, "USB controller: {}", if ctx_usb.is_enabled() { "active" } else { "deferred" });
                tline!(ctx.terminal, "USBContext: {} disk(s) enumerated", ctx_usb.disks().len());
                for disk in ctx_usb.disks() {
                    tline!(ctx.terminal, "  ctrl={} dev_addr={} ep_out=0x{:02x} ep_in=0x{:02x} blk_size={} total_blocks={}",
                        disk.ctrl_type, disk.dev_addr, disk.ep_out, disk.ep_in, disk.block_size, disk.total_blocks);
                }
            }).is_none() {
                tline!(ctx.terminal, "USB controller: unavailable");
            }
            }
            "pci" => {
                use alloc::format;
                use nitrogen::pci::PciScanner;
                ctx.terminal
                    .write_str("BUS  DEV  FUN  VENDOR  DEVICE  CLASS      SUBCLASS  DESCRIPTION\n");
                ctx.terminal.write_str(
                    "---- ---- ----  ------  ------  ---------  --------  -----------\n",
                );
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
                let cb = solvent::RUNTIME_CONTEXT.callback_snapshot().wall_clock;
                match cb.and_then(|f| f()) {
                    Some((y, mo, d, h, mi, s)) => tline!(
                        ctx.terminal,
                        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                        y,
                        mo,
                        d,
                        h,
                        mi,
                        s
                    ),
                    None => tstr!(ctx.terminal, "date: RTC not available"),
                }
            }
            "uptime" => {
                let seconds =
                    solvent::GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed) / 1000;
                let days = seconds / 86400;
                let hms = (seconds % 86400) / 3600;
                let mins = (seconds % 3600) / 60;
                let secs = seconds % 60;
                if days > 0 {
                    tline!(
                        ctx.terminal,
                        "up {} days {:02}:{:02}:{:02}",
                        days,
                        hms,
                        mins,
                        secs
                    );
                } else {
                    tline!(ctx.terminal, "up {:02}:{:02}:{:02}", hms, mins, secs);
                }
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
                if ctx.args.len() < 3 {
                    return tstr!(ctx.terminal, "grep: pattern and file required");
                }
                let pattern = ctx.args[1];
                let show_filename = ctx.args.len() > 3;
                for &path in &ctx.args[2..] {
                    match read_entire_file(path) {
                        Ok(data) => {
                            let text = alloc::string::String::from_utf8_lossy(&data);
                            for line in text.lines().filter(|l| l.contains(pattern)) {
                                if show_filename {
                                    ctx.terminal.write_str(&alloc::format!("{}:", path));
                                }
                                tline!(ctx.terminal, "{}", line);
                            }
                        }
                        Err(e) => tline!(ctx.terminal, "grep: {}: {}", path, e),
                    }
                }
            }
            "sort" => {
                let reverse = ctx.args.contains(&"-r");
                let path_idx = if ctx.args.len() > 1 && ctx.args[1] == "-r" {
                    2
                } else {
                    1
                };
                if path_idx >= ctx.args.len() {
                    return tstr!(ctx.terminal, "Usage: sort [-r] <file>");
                }
                match read_entire_file(ctx.args[path_idx]) {
                    Ok(data) => {
                        let text = alloc::string::String::from_utf8_lossy(&data);
                        let mut lines: alloc::vec::Vec<&str> = text.lines().collect();
                        lines.sort();
                        if reverse {
                            lines.reverse();
                        }
                        for line in lines {
                            tline!(ctx.terminal, "{}", line);
                        }
                    }
                    Err(e) => tline!(ctx.terminal, "sort: {}: {}", ctx.args[path_idx], e),
                }
            }
            "wc" => {
                if ctx.args.len() <= 1 {
                    return tstr!(ctx.terminal, "Usage: wc <file>");
                }
                match read_entire_file(ctx.args[1]) {
                    Ok(data) => {
                        let text = alloc::string::String::from_utf8_lossy(&data);
                        let lines = data.iter().filter(|&&b| b == b'\n').count();
                        let words = text.split_whitespace().count();
                        tline!(
                            ctx.terminal,
                            "{} {} {} {}",
                            lines,
                            words,
                            data.len(),
                            ctx.args[1]
                        );
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
                            .write_str("NAME         VERSION  RUNTIME  DESCRIPTION\n");
                        ctx.terminal
                            .write_str("-----------  -------  -------  -----------\n");
                        for p in &pkgs {
                            let line = format!(
                                "{:<12} {:<8} {:<8} {}\n",
                                p.name, p.version, p.runtime, p.description
                            );
                            ctx.terminal.write_str(&line);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("app list: {}\n", e);
                    ctx.terminal.write_str(&msg);
                }
            },
            "app_catalog" => ctx.terminal.write_str(&crate::ports::catalog_text()),
            _ => {
                let msg = format!("Unknown sys info command: {}\n", cmd);
                ctx.terminal.write_str(&msg);
            }
        }),
        ctl: Some(|cmd| match cmd {
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
            "theme toggle-style" => {
                solvent::toggle_style();
                solvent::force_desktop_redraw();
            }
            "theme classic" => {
                solvent::set_style(solvent::ThemeStyle::Classic);
                solvent::force_desktop_redraw();
            }
            "theme modern" => {
                solvent::set_style(solvent::ThemeStyle::Modern);
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
                    while x86_64::instructions::port::PortReadOnly::<u8>::new(port).read() & 0x02
                        != 0
                    {}
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
                let rest = &cmd[12..];
                if let Some((name, source)) = rest.split_once(' ') {
                    match crate::ports::install(name, source) {
                        Ok(()) => {
                            let msg = format!("Installed package '{}'\n", name);
                            solvent::write_terminal(&msg);
                        }
                        Err(e) => {
                            let msg = format!("app install: {:?}\n", e);
                            solvent::write_terminal(&msg);
                        }
                    }
                }
            }
            _ if cmd.starts_with("app_run ") => {
                let name = &cmd[8..];
                match crate::ports::launch(name) {
                    Ok(pid) => {
                        let msg = format!("Started '{}' (PID {})\n", name, pid);
                        solvent::write_terminal(&msg);
                    }
                    Err(error) => {
                        let msg = format!("app run: {:?}\n", error);
                        solvent::write_terminal(&msg);
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
    };
    nozzle::ShellServices::new(fs, sys, mount)
}

/// Main shell entry point — called from the scheduler as a kernel process.
pub fn shell_main() {
    petroleum::debug_log!("Shell main started");

    let services = nozzle_services();

    if solvent::is_initialized() {
        solvent::run_shell_on_with_command(
            &mut solvent::LatticeTerminal,
            "fullerene> ",
            services,
            None,
        );
    } else {
        let mut terminal = KernelTerminal::new();
        solvent::run_shell_on_with_command(&mut terminal, "fullerene> ", services, None);
    }
}

/// Run the Linux-musl smoke fixture through the real Nozzle command path.
#[cfg(linux_musl_smoke)]
pub fn run_linux_musl_smoke() {
    extern "C" fn unrelated_ready_task() -> ! {
        loop {
            petroleum::cpu_pause();
        }
    }

    struct ScriptedTerminal {
        input: alloc::collections::VecDeque<u8>,
    }

    impl ScriptedTerminal {
        fn new(script: &str) -> Self {
            Self {
                input: script.bytes().collect(),
            }
        }
    }

    impl nozzle::Terminal for ScriptedTerminal {
        fn write_str(&mut self, text: &str) {
            solvent::write_terminal(text);
        }

        fn read_byte(&mut self) -> Option<u8> {
            // The scripted input is already available, so it would never
            // enter the normal empty-keyboard yield path. Exercise the same
            // deferred launch handoff explicitly before consuming the next
            // command byte.
            crate::process::yield_from_scheduler_stack();
            self.input.pop_front()
        }

        fn input_available(&self) -> bool {
            !self.input.is_empty()
        }
    }

    // Keep a non-yielding Ready task ahead of the Linux process. This catches
    // launchers that use a generic round-robin yield instead of switching to
    // the PID they just created.
    let _ = crate::process::create_process(
        "linux-smoke-unrelated-ready",
        x86_64::VirtAddr::from_ptr(unrelated_ready_task as *const ()),
        false,
    );

    let services = nozzle_services();
    let mut terminal = ScriptedTerminal::new(
        "linux_run /bin/rust_std_hello\necho shell-resumed-after-linux\nexit\n",
    );
    solvent::run_shell_on_with_command(&mut terminal, "fullerene> ", services, None);
    if crate::linux::launch::smoke_verified() {
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] PASS: fixture output observed, exit=0, shell resumed\n"
        ));
        // isa-debug-exit maps 0x11 to host status 35. Flasks accepts that
        // status only while the explicit smoke mode is enabled.
        unsafe {
            x86_64::instructions::port::PortWriteOnly::<u32>::new(0xf4).write(0x11);
        }
    } else {
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] FAIL: fixture output or successful exit was not observed\n"
        ));
        petroleum::halt_loop();
    }
}

// ── Kernel terminal ─────────────────────────────────────────────────

struct KernelTerminal {
    history: alloc::collections::VecDeque<String>,
}

impl KernelTerminal {
    fn new() -> Self {
        Self {
            history: alloc::collections::VecDeque::with_capacity(128),
        }
    }
}

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

    fn record_history(&mut self, line: &str) {
        if line.is_empty() || self.history.front().is_some_and(|entry| entry == line) {
            return;
        }
        if self.history.len() >= 128 {
            self.history.pop_back();
        }
        self.history.push_front(String::from(line));
    }

    fn history_snapshot(&self) -> alloc::vec::Vec<String> {
        self.history.iter().cloned().collect()
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
