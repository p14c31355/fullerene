// Linux binary launcher
use crate::loader::LoadError;
use crate::process::ProcessId;
use alloc::boxed::Box;
use alloc::string::ToString;
#[cfg(any(linux_musl_smoke, linux_busybox_smoke))]
use core::sync::atomic::AtomicUsize;
#[cfg(any(linux_musl_smoke, linux_busybox_smoke))]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(linux_musl_smoke)]
static MUSL_SMOKE_PID: AtomicU64 = AtomicU64::new(u64::MAX);
#[cfg(linux_musl_smoke)]
static MUSL_SMOKE_OUTPUT_SEEN: AtomicBool = AtomicBool::new(false);
#[cfg(linux_musl_smoke)]
static MUSL_SMOKE_EXIT_OK: AtomicBool = AtomicBool::new(false);
#[cfg(linux_musl_smoke)]
static MUSL_SMOKE_OUTPUT_MATCHED: AtomicUsize = AtomicUsize::new(0);

#[cfg(linux_musl_smoke)]
const MUSL_SMOKE_OUTPUT: &[u8] = b"Hello from Rust std on musl!";

#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_PID: AtomicU64 = AtomicU64::new(u64::MAX);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_OUTPUT_SEEN: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_EXIT_OK: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_HARNESS_DONE: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_WINDOW: AtomicU64 = AtomicU64::new(u64::MAX);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_WAITING: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_WAIT_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_OUTPUT_MATCHED: AtomicUsize = AtomicUsize::new(0);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_OUTPUT: &[u8] = b"Fullerene BusyBox is running";

/// Launch the built-in test binary ("Hello from Linux!") to verify ABI.
pub fn launch_test_binary() -> Result<ProcessId, LoadError> {
    launch_linux_from_data(crate::linux::test_binary::HELLO_ELF, "hello-linux")
}

/// Launch the ordinary Rust `std` example compiled for static musl Linux.
#[cfg(have_linux_musl_hello)]
pub fn launch_rust_std_hello() -> Result<ProcessId, LoadError> {
    launch_linux_binary_named("/bin/rust-std-hello", "rust-std-musl-hello")
}

#[cfg(not(have_linux_musl_hello))]
pub fn launch_rust_std_hello() -> Result<ProcessId, LoadError> {
    Err(LoadError::FileNotFound)
}

/// Launch a Linux ELF binary from the VFS at `path`.
pub fn launch_linux_binary(path: &str) -> Result<ProcessId, LoadError> {
    // General-purpose callers can provide arbitrary paths, so retain a stable
    // process label for the process table.
    let static_name: &'static str = Box::leak(path.to_string().into_boxed_str());
    launch_linux_binary_named(path, static_name)
}

/// Launch a Linux ELF binary from the VFS with a caller-owned static label.
pub fn launch_linux_binary_named(path: &str, name: &'static str) -> Result<ProcessId, LoadError> {
    crate::klog_fmt!("[LINUX-DIAG] launch begin path={} name={}\n", path, name);
    let data = match crate::fs::read_entire_file(path) {
        Ok(d) => {
            crate::klog_fmt!(
                "[LINUX-DIAG] binary read exit path={} bytes={}\n",
                path,
                d.len()
            );
            d
        }
        Err(error) => {
            crate::klog_fmt!(
                "[LINUX-DIAG] binary read error path={} error={:?}\n",
                path,
                error
            );
            return Err(LoadError::FileNotFound);
        }
    };
    crate::klog_fmt!("[LINUX-DIAG] loader enter path={}\n", path);
    let pid = launch_linux_from_data(&data, name)?;
    crate::klog_fmt!("[LINUX-DIAG] loader exit path={} pid={}\n", path, pid.0);
    #[cfg(linux_musl_smoke)]
    if matches!(path, "/bin/rust-std-hello" | "/bin/rust_std_hello") {
        MUSL_SMOKE_OUTPUT_SEEN.store(false, Ordering::Release);
        MUSL_SMOKE_EXIT_OK.store(false, Ordering::Release);
        MUSL_SMOKE_OUTPUT_MATCHED.store(0, Ordering::Release);
        MUSL_SMOKE_PID.store(pid.0, Ordering::Release);
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] fixture launched as PID {}\n",
            pid.0
        ));
    }
    Ok(pid)
}

#[cfg(linux_musl_smoke)]
pub fn observe_smoke_output(pid: u64, bytes: &[u8]) {
    if MUSL_SMOKE_PID.load(Ordering::Acquire) != pid {
        return;
    }

    let mut matched = MUSL_SMOKE_OUTPUT_MATCHED.load(Ordering::Acquire);
    for &byte in bytes {
        matched = if byte == MUSL_SMOKE_OUTPUT[matched] {
            matched + 1
        } else if byte == MUSL_SMOKE_OUTPUT[0] {
            1
        } else {
            0
        };
        if matched == MUSL_SMOKE_OUTPUT.len() {
            MUSL_SMOKE_OUTPUT_SEEN.store(true, Ordering::Release);
            matched = 0;
        }
    }
    MUSL_SMOKE_OUTPUT_MATCHED.store(matched, Ordering::Release);
}

#[cfg(linux_musl_smoke)]
pub fn observe_smoke_exit(pid: ProcessId, code: i32) {
    if MUSL_SMOKE_PID.load(Ordering::Acquire) == pid.0
        && code == 0
        && MUSL_SMOKE_OUTPUT_SEEN.load(Ordering::Acquire)
    {
        MUSL_SMOKE_EXIT_OK.store(true, Ordering::Release);
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] verified fixture output and exit status\n"
        ));
    }
}

#[cfg(linux_musl_smoke)]
pub fn smoke_verified() -> bool {
    MUSL_SMOKE_EXIT_OK.load(Ordering::Acquire)
}

/// Launch a Linux ELF binary from raw bytes.
pub fn launch_linux_from_data(data: &[u8], name: &'static str) -> Result<ProcessId, LoadError> {
    let argv = [name];
    crate::loader::load_program_with_runtime_args(data, name, &argv, &[], true)
}

fn launch_busybox_with_args(path: &str) -> Result<ProcessId, LoadError> {
    crate::klog_fmt!("[BUSYBOX-DIAG] read begin path={}\n", path);
    let data = match crate::fs::read_entire_file(path) {
        Ok(d) => {
            crate::klog_fmt!("[BUSYBOX-DIAG] read exit path={} bytes={}\n", path, d.len());
            d
        }
        Err(error) => {
            crate::klog_fmt!(
                "[BUSYBOX-DIAG] read error path={} error={:?}\n",
                path,
                error
            );
            return Err(LoadError::FileNotFound);
        }
    };
    // Never start an interactive Linux process without its terminal window.
    // A missing runtime here would otherwise leave BusyBox polling the global
    // keyboard queue invisibly, which looks like a machine hang on hardware.
    crate::klog_fmt!("[BUSYBOX-DIAG] create_process_terminal begin\n");
    let Some(terminal_window) = solvent::create_process_terminal("BusyBox") else {
        crate::klog_fmt!(
            "[BUSYBOX-DIAG] create_process_terminal failed — aborting to avoid invisible hang\n"
        );
        petroleum::serial::serial_log(format_args!(
            "[LINUX-DIAG] BusyBox terminal window could not be created\n"
        ));
        return Err(LoadError::MappingFailed);
    };
    crate::klog_fmt!(
        "[BUSYBOX-DIAG] create_process_terminal exit window_id={}\n",
        terminal_window.0
    );
    #[cfg(linux_busybox_smoke)]
    let argv = ["busybox", "sh"];
    #[cfg(not(linux_busybox_smoke))]
    let argv = ["busybox", "sh"];
    let envp = [
        "PATH=/bin:/sbin:/usr/bin:/usr/sbin",
        "HOME=/root",
        "SHELL=/bin/sh",
        "TERM=xterm",
    ];
    crate::klog_fmt!("[BUSYBOX-DIAG] loader enter bytes={}\n", data.len());
    let pid = match crate::loader::load_program_with_runtime_args(
        data.as_slice(),
        "busybox",
        &argv,
        &envp,
        true,
    ) {
        Ok(pid) => {
            crate::klog_fmt!("[BUSYBOX-DIAG] loader exit pid={}\n", pid.0);
            pid
        }
        Err(error) => {
            crate::klog_fmt!("[BUSYBOX-DIAG] loader error={:?}\n", error);
            solvent::close_process_terminal(terminal_window);
            return Err(error);
        }
    };
    crate::klog_fmt!("[BUSYBOX-DIAG] attach terminal_window to pid={}\n", pid.0);
    let _ = crate::process::SCHEDULER.with_process(pid, |process| {
        if let Some(crate::linux::DispatchMode::Linux(runtime)) = process.dispatch_mode.as_mut() {
            runtime.terminal_window = Some(terminal_window);
        }
    });
    crate::klog_fmt!("[BUSYBOX-DIAG] launch complete pid={}\n", pid.0);
    #[cfg(linux_busybox_smoke)]
    {
        BUSYBOX_SMOKE_OUTPUT_SEEN.store(false, Ordering::Release);
        BUSYBOX_SMOKE_EXIT_OK.store(false, Ordering::Release);
        BUSYBOX_SMOKE_WINDOW_CLOSED.store(false, Ordering::Release);
        BUSYBOX_SMOKE_HARNESS_DONE.store(false, Ordering::Release);
        BUSYBOX_SMOKE_OUTPUT_MATCHED.store(0, Ordering::Release);
        BUSYBOX_SMOKE_WAITING.store(false, Ordering::Release);
        BUSYBOX_SMOKE_WAIT_COUNT.store(0, Ordering::Release);
        BUSYBOX_SMOKE_WINDOW.store(terminal_window.0, Ordering::Release);
        // Feed only the first command. The exit command is injected after
        // BusyBox has reached a real no-input wait.
        solvent::push_process_terminal_input(
            terminal_window,
            b"echo Fullerene BusyBox is running\n",
        );
        BUSYBOX_SMOKE_PID.store(pid.0, Ordering::Release);
        petroleum::serial::serial_log(format_args!(
            "[busybox-smoke] fixture launched as PID {}\n",
            pid.0
        ));
    }
    Ok(pid)
}

#[cfg(linux_busybox_smoke)]
pub fn observe_busybox_output(pid: u64, bytes: &[u8]) {
    if BUSYBOX_SMOKE_PID.load(Ordering::Acquire) != pid {
        return;
    }
    let mut matched = BUSYBOX_SMOKE_OUTPUT_MATCHED.load(Ordering::Acquire);
    for &byte in bytes {
        matched = if byte == BUSYBOX_SMOKE_OUTPUT[matched] {
            matched + 1
        } else if byte == BUSYBOX_SMOKE_OUTPUT[0] {
            1
        } else {
            0
        };
        if matched == BUSYBOX_SMOKE_OUTPUT.len() {
            BUSYBOX_SMOKE_OUTPUT_SEEN.store(true, Ordering::Release);
            BUSYBOX_SMOKE_WAIT_COUNT.store(0, Ordering::Release);
            BUSYBOX_SMOKE_WAITING.store(true, Ordering::Release);
            matched = 0;
        }
    }
    BUSYBOX_SMOKE_OUTPUT_MATCHED.store(matched, Ordering::Release);
}

/// Advance the interactive smoke only after BusyBox has entered a real
/// no-input wait. This deliberately keeps the second command out of the
/// terminal queue until the blocking poll/read path has yielded a few times.
#[cfg(linux_busybox_smoke)]
pub fn observe_busybox_wait(pid: u64) {
    if BUSYBOX_SMOKE_PID.load(Ordering::Acquire) != pid
        || !BUSYBOX_SMOKE_WAITING.load(Ordering::Acquire)
    {
        return;
    }
    let waits = BUSYBOX_SMOKE_WAIT_COUNT.fetch_add(1, Ordering::AcqRel) + 1;
    if waits < 3 {
        return;
    }
    BUSYBOX_SMOKE_WAITING.store(false, Ordering::Release);
    let window_id = BUSYBOX_SMOKE_WINDOW.load(Ordering::Acquire);
    if window_id != u64::MAX {
        // Inject the second command as real PS/2 scancodes. The focused GUI
        // terminal must route these decoded bytes from Nitrogen into the
        // process terminal; writing directly to its queue would not test the
        // user-facing keyboard path.
        for scancode in [0x12, 0x2d, 0x17, 0x14, 0x1c] {
            nitrogen::ps2::keyboard::handle_keyboard_scancode(scancode);
            nitrogen::ps2::keyboard::handle_keyboard_scancode(scancode | 0x80);
        }
    }
}

#[cfg(linux_busybox_smoke)]
pub fn observe_busybox_exit(pid: ProcessId, code: i32) {
    if BUSYBOX_SMOKE_PID.load(Ordering::Acquire) == pid.0
        && code == 0
        && BUSYBOX_SMOKE_OUTPUT_SEEN.load(Ordering::Acquire)
    {
        let window_id = BUSYBOX_SMOKE_WINDOW.load(Ordering::Acquire);
        let window_closed = window_id == u64::MAX
            || !solvent::process_terminal_exists(lattice::window::WindowId(window_id));
        BUSYBOX_SMOKE_WINDOW_CLOSED.store(window_closed, Ordering::Release);
        BUSYBOX_SMOKE_EXIT_OK.store(true, Ordering::Release);
        petroleum::serial::serial_log(format_args!(
            "[busybox-smoke] verified output and exit status\n"
        ));
    }
}

#[cfg(linux_busybox_smoke)]
pub fn busybox_smoke_verified() -> bool {
    BUSYBOX_SMOKE_EXIT_OK.load(Ordering::Acquire)
}

#[cfg(linux_busybox_smoke)]
pub fn mark_busybox_smoke_harness_done() {
    BUSYBOX_SMOKE_HARNESS_DONE.store(true, Ordering::Release);
}

#[cfg(linux_busybox_smoke)]
pub fn busybox_smoke_complete() -> bool {
    BUSYBOX_SMOKE_EXIT_OK.load(Ordering::Acquire)
        && BUSYBOX_SMOKE_WINDOW_CLOSED.load(Ordering::Acquire)
        && BUSYBOX_SMOKE_HARNESS_DONE.load(Ordering::Acquire)
}

/// Launch BusyBox shell from embedded initramfs data.
pub fn launch_busybox() -> Result<ProcessId, LoadError> {
    // Look for busybox in standard locations
    let locations = [
        "/bin/busybox",
        "/sbin/busybox",
        "/usr/bin/busybox",
        "/usr/sbin/busybox",
        "/busybox",
        "/init",
    ];

    for path in &locations {
        if crate::contexts::vfs::exists(path) {
            crate::klog_fmt!("[BUSYBOX-DIAG] found at {}\n", path);
            return launch_busybox_with_args(path);
        }
    }

    crate::klog_fmt!("[BUSYBOX-DIAG] binary not found in any standard location\n");
    Err(LoadError::FileNotFound)
}

/// Initialize the initramfs: creates basic Linux filesystem structure
/// and unpacks any embedded CPIO archive into the VFS.
pub fn init_initramfs() {
    log::info!("Initramfs: creating Linux filesystem structure");

    // Create standard Linux directories
    let dirs = [
        "/bin",
        "/sbin",
        "/usr",
        "/usr/bin",
        "/usr/sbin",
        "/etc",
        "/dev",
        "/proc",
        "/sys",
        "/tmp",
        "/var",
        "/var/log",
        "/root",
        "/home",
        "/lib",
        "/lib64",
        "/mnt",
        "/usr/share",
        "/usr/share/sounds",
        "/usr/share/sounds/fullerene",
    ];

    for dir in &dirs {
        let _ = crate::contexts::vfs::mkdir(dir);
    }

    // /dev/null is provided by the dynamic DevFs mount.

    // Create a simple /etc/hostname
    let _ = crate::fs::write_entire_file("/etc/hostname", b"fullerene\n");

    #[cfg(have_busybox)]
    {
        let busybox = include_bytes!(concat!(env!("OUT_DIR"), "/busybox"));
        if let Err(error) = crate::fs::write_entire_file("/bin/busybox", busybox) {
            log::warn!("Initramfs: failed to install /bin/busybox: {:?}", error);
        }
    }

    #[cfg(have_linux_musl_hello)]
    {
        let fixture = include_bytes!(concat!(env!("OUT_DIR"), "/linux_musl_hello"));
        for path in ["/bin/rust-std-hello", "/bin/rust_std_hello"] {
            if let Err(error) = crate::fs::write_entire_file(path, fixture) {
                log::warn!("Initramfs: failed to install {}: {:?}", path, error);
            }
        }
    }

    // Create /apps directory for WASI applications
    let _ = crate::contexts::vfs::mkdir("/apps");

    // Embed the hello.wasm test binary (built at compile time by build.rs)
    if let Err(e) = crate::fs::write_entire_file(
        "/apps/hello.wasm",
        include_bytes!(concat!(env!("OUT_DIR"), "/hello.wasm")),
    ) {
        log::warn!("Initramfs: failed to write /apps/hello.wasm: {:?}", e);
    }

    // Embed the std-based startup sound player and both source encodings.
    if let Err(e) = crate::fs::write_entire_file(
        "/apps/startup_sound.wasm",
        include_bytes!(concat!(env!("OUT_DIR"), "/startup_sound.wasm")),
    ) {
        log::warn!(
            "Initramfs: failed to write /apps/startup_sound.wasm: {:?}",
            e
        );
    }
    let wav: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/audio/fullerene_startup_sound.wav"
    ));
    let mp3: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/audio/fullerene_startup_sound.mp3"
    ));
    for (path, data) in [
        (
            "/usr/share/sounds/fullerene/fullerene_startup_sound.wav",
            wav,
        ),
        (
            "/usr/share/sounds/fullerene/fullerene_startup_sound.mp3",
            mp3,
        ),
    ] {
        if let Err(e) = crate::fs::write_entire_file(path, data) {
            log::warn!("Initramfs: failed to write {}: {:?}", path, e);
        }
    }

    // Embed the viewer.wasm WASM app (built at compile time by build.rs)
    #[cfg(have_viewer_wasm)]
    if let Err(e) = crate::fs::write_entire_file(
        "/apps/viewer.wasm",
        include_bytes!(concat!(env!("OUT_DIR"), "/viewer.wasm")),
    ) {
        log::warn!("Initramfs: failed to write /apps/viewer.wasm: {:?}", e);
    }

    // Embed the Emulsion screenshot app (built at compile time by build.rs).
    #[cfg(have_emulsion_wasm)]
    if let Err(e) = crate::fs::write_entire_file(
        "/apps/emulsion.wasm",
        include_bytes!(concat!(env!("OUT_DIR"), "/emulsion.wasm")),
    ) {
        log::warn!("Initramfs: failed to write /apps/emulsion.wasm: {:?}", e);
    }

    // If a CPIO archive is embedded in the kernel, unpack it now.
    // This is the third layer of the storage stack foundation:
    //   block cache → FAT32 → initramfs.
    if let Some(archive) = embedded_initramfs() {
        log::info!(
            "Initramfs: unpacking {} bytes of CPIO archive",
            archive.len()
        );
        match crate::initramfs::unpack(archive) {
            Ok(n) => log::info!("Initramfs: unpacked {} entries from CPIO archive", n),
            Err(e) => log::warn!("Initramfs: CPIO unpack failed: {}", e),
        }
    }

    log::info!("Initramfs: Linux filesystem structure created");
}

/// Return the embedded CPIO archive, if one was compiled into the kernel.
///
/// Port packages are built from `toluene/<port>/` submodule sources by
/// `build.rs` at compile time.  Each port is compiled (or downloaded) to
/// a Linux ELF binary, packaged into a CPIO archive in `OUT_DIR`, and
/// embedded via `include_bytes!`.  `build.rs` sets `cfg(have_ports_cpio)`
/// when at least one port was built successfully.
///
/// The archive is unpacked into the VFS during the initramfs boot step,
/// making ports available at `/packages/<name>/app.bin` for `app run`.
#[cfg(have_ports_cpio)]
fn embedded_initramfs() -> Option<&'static [u8]> {
    Some(include_bytes!(concat!(env!("OUT_DIR"), "/ports.cpio")))
}

#[cfg(not(have_ports_cpio))]
fn embedded_initramfs() -> Option<&'static [u8]> {
    None
}
