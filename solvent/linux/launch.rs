// Linux binary launcher
use crate::loader::LoadError;
use crate::process::ProcessId;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec::Vec;
#[cfg(linux_busybox_smoke)]
use core::sync::atomic::AtomicU8;
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
static BUSYBOX_SMOKE_LAUNCH_COUNT: AtomicU8 = AtomicU8::new(0);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_HOLD_INPUT: AtomicBool = AtomicBool::new(false);
#[cfg(linux_busybox_smoke)]
static BUSYBOX_SMOKE_OUTPUT: &[u8] = b"Fullerene BusyBox all applets passed";

#[cfg(linux_busybox_smoke)]
const BUSYBOX_SMOKE_APPLET_COUNT: usize =
    include!(concat!(env!("OUT_DIR"), "/busybox-applet-count.rs"));
#[cfg(linux_busybox_smoke)]
const BUSYBOX_SMOKE_HELP_CHECK_MARKER: &str = "__FULLERENE_BUSYBOX_HELP_CHECK__";

#[cfg(linux_busybox_smoke)]
fn busybox_smoke_script() -> alloc::string::String {
    let script = alloc::format!(
        "set -e\n\
    busybox rmdir /tmp/busybox-contract /tmp/busybox-dir || true\n\
    busybox rm -f /tmp/busybox-input || true\n\
    busybox busybox --help >/tmp/busybox-help\n\
    busybox busybox --list >/tmp/busybox-list\n\
    busybox test \"$(busybox wc -l </tmp/busybox-list)\" -eq {count}\n\
    __FULLERENE_BUSYBOX_HELP_CHECK__\n\
    busybox mkdir /tmp/busybox-contract\n\
    busybox printf 'alpha\\nbeta\\nalpha\\n' >/tmp/busybox-input\n\
    busybox '[' 1 -eq 1 ']'\n\
    busybox '[[' 1 -eq 1 ']]'\n\
    busybox arch >/tmp/busybox-arch\n\
    busybox ash -c 'exit 0'\n\
    busybox awk 'BEGIN {{ if (1+1 != 2) exit 1 }}'\n\
    busybox basename /tmp/busybox-input >/tmp/busybox-basename\n\
    busybox test \"$(busybox cat /tmp/busybox-basename)\" = busybox-input\n\
    busybox cat /tmp/busybox-input >/tmp/busybox-cat\n\
    busybox grep -q '^alpha$' /tmp/busybox-cat\n\
    busybox cksum /tmp/busybox-input >/tmp/busybox-cksum\n\
    busybox test -s /tmp/busybox-cksum\n\
    busybox clear\n\
    busybox cp /tmp/busybox-input /tmp/busybox-copy\n\
    busybox test -f /tmp/busybox-copy\n\
    busybox cut -d: -f2 /tmp/busybox-input >/tmp/busybox-cut\n\
    busybox test \"$(busybox wc -c </tmp/busybox-cut)\" -eq 17\n\
    busybox date >/tmp/busybox-date\n\
    busybox test -s /tmp/busybox-date\n\
    busybox dd if=/tmp/busybox-input of=/tmp/busybox-dd bs=1 count=1\n\
    busybox test \"$(busybox wc -c </tmp/busybox-dd)\" -eq 1\n\
    busybox dirname /tmp/busybox-input >/tmp/busybox-dirname\n\
    busybox test \"$(busybox cat /tmp/busybox-dirname)\" = /tmp\n\
    busybox echo smoke >/tmp/busybox-echo\n\
    busybox grep -q '^smoke$' /tmp/busybox-echo\n\
    busybox env -i /bin/busybox true >/tmp/busybox-env\n\
    busybox expr 1 + 1 >/tmp/busybox-expr\n\
    busybox test \"$(busybox cat /tmp/busybox-expr)\" = 2\n\
    if busybox false; then busybox echo false-unexpected; exit 1; fi\n\
    busybox fold -w 3 /tmp/busybox-input >/tmp/busybox-fold\n\
    busybox test -s /tmp/busybox-fold\n\
    busybox grep -q alpha /tmp/busybox-input\n\
    busybox head -n 1 /tmp/busybox-input >/tmp/busybox-head\n\
    busybox grep -q '^alpha$' /tmp/busybox-head\n\
    busybox hexdump -C /tmp/busybox-input >/tmp/busybox-hexdump\n\
    busybox test -s /tmp/busybox-hexdump\n\
    busybox hostname >/tmp/busybox-hostname\n\
    busybox test -s /tmp/busybox-hostname\n\
    busybox test -d /\n\
    busybox stat / >/tmp/busybox-root-stat\n\
    busybox test -s /tmp/busybox-root-stat\n\
    busybox ls / >/tmp/busybox-ls\n\
    busybox test -s /tmp/busybox-ls\n\
    busybox md5sum /tmp/busybox-input >/tmp/busybox-md5\n\
    busybox test -s /tmp/busybox-md5\n\
    busybox mkdir /tmp/busybox-dir\n\
    busybox test -d /tmp/busybox-dir\n\
    tmp=$(busybox mktemp /tmp/busybox.XXXXXX)\n\
    busybox test -f \"$tmp\"\n\
    busybox mv /tmp/busybox-copy /tmp/busybox-moved\n\
    busybox test -f /tmp/busybox-moved\n\
    busybox od /tmp/busybox-input >/tmp/busybox-od\n\
    busybox test -s /tmp/busybox-od\n\
    busybox printenv PATH >/tmp/busybox-path\n\
    busybox test -s /tmp/busybox-path\n\
    busybox printf '%s\\n' printf >/tmp/busybox-printf\n\
    busybox grep -q '^printf$' /tmp/busybox-printf\n\
    busybox pwd >/tmp/busybox-pwd\n\
    busybox test -d \"$(busybox cat /tmp/busybox-pwd)\"\n\
    busybox rm /tmp/busybox-moved\n\
    if busybox test -e /tmp/busybox-moved; then exit 1; fi\n\
    busybox rmdir /tmp/busybox-dir\n\
    if busybox test -e /tmp/busybox-dir; then exit 1; fi\n\
    busybox sed -n 's/^alpha$/ok/p' /tmp/busybox-input >/tmp/busybox-sed\n\
    busybox grep -q '^ok$' /tmp/busybox-sed\n\
    busybox seq 1 2 >/tmp/busybox-seq\n\
    busybox test \"$(busybox wc -l </tmp/busybox-seq)\" -eq 2\n\
    busybox sha256sum /tmp/busybox-input >/tmp/busybox-sha256\n\
    busybox test -s /tmp/busybox-sha256\n\
    busybox sh -c 'busybox true'\n\
    busybox sleep 0\n\
    busybox sort /tmp/busybox-input >/tmp/busybox-sort\n\
    busybox grep -q '^alpha$' /tmp/busybox-sort\n\
    busybox stat /tmp/busybox-input >/tmp/busybox-stat\n\
    busybox test -s /tmp/busybox-stat\n\
    busybox tail -n 1 /tmp/busybox-input >/tmp/busybox-tail\n\
    busybox grep -q '^alpha$' /tmp/busybox-tail\n\
    busybox tar -cf /tmp/busybox.tar /tmp/busybox-input\n\
    busybox test -s /tmp/busybox.tar\n\
    busybox tar -tf /tmp/busybox.tar >/tmp/busybox-tar-list\n\
    busybox grep -q 'busybox-input' /tmp/busybox-tar-list\n\
    busybox tee /tmp/busybox-tee </tmp/busybox-input >/tmp/busybox-tee-out\n\
    busybox grep -q '^alpha$' /tmp/busybox-tee-out\n\
    busybox test -f /tmp/busybox-input\n\
    busybox touch /tmp/busybox-touch\n\
    busybox test -f /tmp/busybox-touch\n\
    busybox tr a-z A-Z </tmp/busybox-input >/tmp/busybox-tr\n\
    busybox grep -q '^ALPHA$' /tmp/busybox-tr\n\
    busybox true\n\
    if busybox tty >/tmp/busybox-tty; then\n\
    busybox test -s /tmp/busybox-tty\n\
    else\n\
    tty_status=$?\n\
    busybox test \"$tty_status\" -eq 1\n\
    fi\n\
    busybox uname -a >/tmp/busybox-uname\n\
    busybox test -s /tmp/busybox-uname\n\
    busybox uniq /tmp/busybox-input >/tmp/busybox-uniq\n\
    busybox test \"$(busybox wc -l </tmp/busybox-uniq)\" -eq 3\n\
    busybox uptime >/tmp/busybox-uptime\n\
    busybox test -s /tmp/busybox-uptime\n\
    busybox wc -l /tmp/busybox-input >/tmp/busybox-wc\n\
    busybox grep -q '3' /tmp/busybox-wc\n\
    busybox which busybox >/tmp/busybox-which\n\
    busybox grep -q '/bin/busybox' /tmp/busybox-which\n\
    busybox whoami >/tmp/busybox-whoami\n\
    busybox test \"$(busybox cat /tmp/busybox-whoami)\" = root\n\
    busybox yes | busybox head -n 1 >/tmp/busybox-yes\n\
    busybox test -s /tmp/busybox-yes\n\
    busybox rmdir /tmp/busybox-contract /tmp/busybox-dir || true\n\
    echo Fullerene BusyBox all applets passed\n\
    exit 0\n",
        count = BUSYBOX_SMOKE_APPLET_COUNT
    );
    let help_check = alloc::string::String::from(
        "busybox grep -q 'Currently defined functions:' /tmp/busybox-help\n\
busybox awk 'BEGIN { while ((getline name < \"/tmp/busybox-list\") > 0) wanted[name]=1 } { for (name in wanted) if (index($0, name)) found[name]=1 } END { for (name in wanted) if (!(name in found)) exit 1 }' /tmp/busybox-help",
    );
    script.replace(BUSYBOX_SMOKE_HELP_CHECK_MARKER, &help_check)
}

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
    launch_linux_binary_with_args(path, &[])
}

/// Launch a Linux ELF from the VFS with command-line arguments.
pub fn launch_linux_binary_with_args(path: &str, args: &[&str]) -> Result<ProcessId, LoadError> {
    // General-purpose callers can provide arbitrary paths, so retain a stable
    // process label for the process table.
    let static_name: &'static str = Box::leak(path.to_string().into_boxed_str());
    launch_linux_binary_named_with_args(path, static_name, args)
}

/// Launch a Linux ELF binary from the VFS with a caller-owned static label.
pub fn launch_linux_binary_named(path: &str, name: &'static str) -> Result<ProcessId, LoadError> {
    launch_linux_binary_named_with_args(path, name, &[])
}

/// Launch a Linux ELF from the VFS with a stable process label and argv.
pub fn launch_linux_binary_named_with_args(
    path: &str,
    name: &'static str,
    args: &[&str],
) -> Result<ProcessId, LoadError> {
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
    let pid = launch_linux_from_data_with_args(&data, name, args)?;
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
    launch_linux_from_data_with_args(data, name, &[])
}

/// Launch a Linux ELF from raw bytes with user-provided argv entries.
pub fn launch_linux_from_data_with_args(
    data: &[u8],
    name: &'static str,
    args: &[&str],
) -> Result<ProcessId, LoadError> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(name);
    argv.extend_from_slice(args);
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
    log_busybox_stage(terminal_window, "window-created");
    let argv = ["busybox", "sh"];
    let envp = [
        "PATH=/bin:/sbin:/usr/bin:/usr/sbin",
        "HOME=/root",
        "SHELL=/bin/sh",
        "TERM=xterm",
    ];
    crate::klog_fmt!("[BUSYBOX-DIAG] loader enter bytes={}\n", data.len());
    log_busybox_stage(terminal_window, "loader-enter");
    let pid = match crate::loader::load_program_with_runtime_args(
        data.as_slice(),
        "busybox",
        &argv,
        &envp,
        true,
    ) {
        Ok(pid) => {
            crate::klog_fmt!("[BUSYBOX-DIAG] loader exit pid={}\n", pid.0);
            log_busybox_stage(terminal_window, "process-created");
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
    log_busybox_stage(terminal_window, "runtime-attached");
    crate::klog_fmt!("[BUSYBOX-DIAG] launch complete pid={}\n", pid.0);
    log_busybox_stage(terminal_window, "handoff-ready");
    #[cfg(linux_busybox_smoke)]
    {
        let launch_number = BUSYBOX_SMOKE_LAUNCH_COUNT.fetch_add(1, Ordering::AcqRel);
        if launch_number == 0 {
            BUSYBOX_SMOKE_OUTPUT_SEEN.store(false, Ordering::Release);
            BUSYBOX_SMOKE_EXIT_OK.store(false, Ordering::Release);
            BUSYBOX_SMOKE_HARNESS_DONE.store(false, Ordering::Release);
            BUSYBOX_SMOKE_OUTPUT_MATCHED.store(0, Ordering::Release);
        }
        BUSYBOX_SMOKE_WINDOW_CLOSED.store(false, Ordering::Release);
        BUSYBOX_SMOKE_WAITING.store(false, Ordering::Release);
        BUSYBOX_SMOKE_WAIT_COUNT.store(0, Ordering::Release);
        BUSYBOX_SMOKE_WINDOW.store(terminal_window.0, Ordering::Release);
        BUSYBOX_SMOKE_HOLD_INPUT.store(true, Ordering::Release);
        if launch_number == 0 {
            // Run every applet in the generated contract through the bundled
            // BusyBox command. The exit command is injected only after the
            // success marker and a real no-input wait.
            let script = busybox_smoke_script();
            solvent::push_process_terminal_input(terminal_window, script.as_bytes());
        } else {
            // The second launch is deliberately a fresh interactive process;
            // its purpose is to catch stale terminal/page-table state after
            // the full contract has completed, not to duplicate the contract.
            solvent::push_process_terminal_input(terminal_window, b"exit\n");
        }
        BUSYBOX_SMOKE_PID.store(pid.0, Ordering::Release);
        petroleum::serial::serial_log(format_args!(
            "[busybox-smoke] fixture launched as PID {} pass={}\n",
            pid.0,
            launch_number == 0
        ));
    }
    Ok(pid)
}

/// Leave the launch milestone visible in the process window as well as in the
/// kernel log. Hardware runs often have no serial capture, while a frozen GUI
/// still preserves the last milestone that was rendered.
fn log_busybox_stage(window_id: lattice::window::WindowId, stage: &str) {
    crate::klog_fmt!("[BUSYBOX-DIAG] stage={} window_id={}\n", stage, window_id.0);
    solvent::request_frame();
    solvent::mark_klog_live_dirty();
    solvent::flush_frame_no_fb();
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
    let tracked = BUSYBOX_SMOKE_PID.load(Ordering::Acquire) == pid.0;
    if tracked {
        BUSYBOX_SMOKE_HOLD_INPUT.store(false, Ordering::Release);
    }
    if tracked && code == 0 && BUSYBOX_SMOKE_OUTPUT_SEEN.load(Ordering::Acquire) {
        let window_id = BUSYBOX_SMOKE_WINDOW.load(Ordering::Acquire);
        let window_closed = if window_id == u64::MAX {
            true
        } else {
            let window = lattice::window::WindowId(window_id);
            if solvent::process_terminal_exists(window) {
                solvent::close_process_terminal(window);
            }
            !solvent::process_terminal_exists(window)
        };
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
pub fn busybox_smoke_input_held() -> bool {
    BUSYBOX_SMOKE_HOLD_INPUT.load(Ordering::Acquire)
}

#[cfg(linux_busybox_smoke)]
pub fn reset_busybox_smoke_harness() {
    BUSYBOX_SMOKE_LAUNCH_COUNT.store(0, Ordering::Release);
    BUSYBOX_SMOKE_HOLD_INPUT.store(false, Ordering::Release);
}

#[cfg(linux_busybox_smoke)]
pub fn mark_busybox_smoke_harness_done() {
    BUSYBOX_SMOKE_HARNESS_DONE.store(true, Ordering::Release);
}

#[cfg(linux_busybox_smoke)]
pub fn busybox_smoke_complete() -> bool {
    BUSYBOX_SMOKE_LAUNCH_COUNT.load(Ordering::Acquire) >= 2
        && BUSYBOX_SMOKE_EXIT_OK.load(Ordering::Acquire)
        && BUSYBOX_SMOKE_WINDOW_CLOSED.load(Ordering::Acquire)
        && BUSYBOX_SMOKE_HARNESS_DONE.load(Ordering::Acquire)
}

/// Launch BusyBox shell from embedded initramfs data.
pub fn launch_busybox() -> Result<ProcessId, LoadError> {
    // Look for in standard locations
    let locations = [
        "/bin/busybox",
        "/sbin/busybox",
        "/usr/bin/busybox",
        "/usr/sbin/busybox",
        "/busybox",
        "/init",
    ];

    for path in &locations {
        crate::klog_fmt!("[BUSYBOX-DIAG] path check begin path={}\n", path);
        let present = crate::contexts::vfs::exists(path);
        crate::klog_fmt!(
            "[BUSYBOX-DIAG] path check exit path={} present={}\n",
            path,
            present
        );
        if present {
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
        "/lib/x86_64-linux-gnu",
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
    let _ = crate::fs::write_entire_file("/etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n");

    #[cfg(have_busybox)]
    {
        let busybox = include_bytes!(concat!(env!("OUT_DIR"), "/busybox"));
        if let Err(error) = crate::fs::write_entire_file("/bin/busybox", busybox) {
            log::warn!("Initramfs: failed to install /bin/busybox: {:?}", error);
        }

        let interpreter = include_bytes!(concat!(env!("OUT_DIR"), "/busybox-interpreter"));
        for path in [
            "/lib64/ld-linux-x86-64.so.2",
            "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
        ] {
            if let Err(error) = crate::fs::write_entire_file(path, interpreter) {
                log::warn!("Initramfs: failed to install {}: {:?}", path, error);
            }
        }
        let libc = include_bytes!(concat!(env!("OUT_DIR"), "/busybox-libc"));
        if let Err(error) = crate::fs::write_entire_file("/lib/x86_64-linux-gnu/libc.so.6", libc) {
            log::warn!(
                "Initramfs: failed to install /lib/x86_64-linux-gnu/libc.so.6: {:?}",
                error
            );
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

    // Install the embedded Linux ABI fixture at a normal executable path so
    // all Linux/WASI programs use the same `exec <path>` shell interface.
    if let Err(e) =
        crate::fs::write_entire_file("/bin/hello_linux", crate::linux::test_binary::HELLO_ELF)
    {
        log::warn!("Initramfs: failed to write /bin/hello_linux: {:?}", e);
    }

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
