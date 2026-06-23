//! Consolidated logging system for the Fullerene project.
//!
//! Provides unified logging macros and functions for all sub-crates.
//! Uses serial output directly instead of log crate to avoid std dependencies.

// Note: log crate dependency removed to avoid std pull-in.
// Re-export serial functions for logging.

/// ── EARLY-ONLY GLOBAL STATE ─────────────────────────────────────────
// The logger is initialised during boot and its static state (`LOGGER`,
// `LOGGER_INITIALIZED`) lives in the `.data` / `.bss` sections.
// After the world-switch (CR3 reload), this state may become stale or
// inaccessible. The runtime kernel SHOULD use `early::console::EarlyConsole`
// or `graphics::PRIMARY_RENDERER` for output instead.

pub struct FullereneLogger {
    level: log::LevelFilter,
}

impl FullereneLogger {
    pub const fn new() -> Self {
        Self {
            level: log::LevelFilter::Info,
        }
    }
}

impl log::Log for FullereneLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            // Format into a stack-allocated buffer to avoid dynamic
            // allocation inside the global logger (which can deadlock
            // if invoked from interrupt context while the allocator
            // lock is held by a thread in process context).
            use core::fmt::Write;
            const BUF_CAP: usize = 256;
            let mut buf = [0u8; BUF_CAP];
            let len = {
                let mut writer = StackWriter {
                    buf: &mut buf[..],
                    pos: 0,
                };
                let _ = write!(writer, "[{}] {}\n", record.level(), record.args());
                writer.pos
            };
            let msg = core::str::from_utf8(&buf[..len]).unwrap_or("[log error]");
            crate::serial::serial_log(format_args!("{}", msg));
            // Forward to kernel log hook (dmesg) when registered.
            // Copy the function pointer out of the lock first to avoid
            // deadlock if the callback itself triggers logging.
            let hook = *LOG_HOOK.lock();
            if let Some(hook) = hook {
                hook(record.level(), msg);
            }
        }
    }

    fn flush(&self) {}
}

static LOGGER: FullereneLogger = FullereneLogger::new();
static LOGGER_INITIALIZED: spin::Once<()> = spin::Once::new();

/// Optional hook registered by the kernel to capture log messages
/// for in-OS display (e.g. dmesg).
pub static LOG_HOOK: spin::Mutex<Option<fn(log::Level, &str)>> = spin::Mutex::new(None);

pub fn init_global_logger() -> Result<(), log::SetLoggerError> {
    log::set_logger(&LOGGER)?;
    log::set_max_level(LOGGER.level);
    LOGGER_INITIALIZED.call_once(|| {});
    crate::serial::serial_log(format_args!(
        "[INIT] Logger initialized at level {:?}\n",
        LOGGER.level
    ));
    Ok(())
}

pub fn is_logger_initialized() -> bool {
    LOGGER_INITIALIZED.is_completed()
}

/// Unified result type for system operations
pub type SystemResult<T> = Result<T, SystemError>;

/// System error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemError {
    InvalidSyscall = 1,
    BadFileDescriptor = 9,
    PermissionDenied = 13,
    FileNotFound = 2,
    NoSuchProcess = 3,
    InvalidArgument = 22,
    SyscallOutOfMemory = 12,
    FileExists = 17,
    InvalidSeek = 29,
    DiskFull = 28,
    MappingFailed = 100,
    UnmappingFailed = 101,
    FrameAllocationFailed = 102,
    MemOutOfMemory = 103,
    InvalidFormat = 200,
    LoadFailed = 201,
    DeviceNotFound = 300,
    DeviceError = 301,
    PortError = 302,
    NotImplemented = 400,
    NotSupported = 401,
    InternalError = 500,
    UnknownError = 999,
    FsInvalidFileDescriptor = 8,
    TooManyProcesses = 600,
    OperationAgain = 11,
    OperationTimedOut = 110,
    NoSuchDevice = 19,
    BadHandle = 104,
    WouldBlock = 140,
}

/// Logging trait for system errors with context — used by initializer's HardwareDevice.
pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
    fn log_debug(&self, message: &'static str);
    fn log_trace(&self, message: &'static str);
}

pub struct ErrorLogger;
impl ErrorLogging for ErrorLogger {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log::error!("{}: {}", *error as u64, context);
    }
    fn log_warning(&self, message: &'static str) {
        log::warn!("{}", message);
    }
    fn log_info(&self, message: &'static str) {
        log::info!("{}", message);
    }
    fn log_debug(&self, message: &'static str) {
        log::debug!("{}", message);
    }
    fn log_trace(&self, message: &'static str) {
        log::trace!("{}", message);
    }
}

pub static ERROR_LOGGER: ErrorLogger = ErrorLogger;

/// Conditionally routes to `log` crate when the global logger is initialised,
/// otherwise writes to serial directly.  Avoids allocating in the early path.
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("[INFO] {}\n", format_args!($($arg)*)));
        }
    };
}

#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::error!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("[ERROR] {}\n", format_args!($($arg)*)));
        }
    };
}

#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::warn!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("[WARN] {}\n", format_args!($($arg)*)));
        }
    };
}

/// debug_log uses the no-alloc variant when the logger isn't ready.
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::debug!("{}", format_args!($($arg)*));
        } else {
            $crate::debug_log_no_alloc!($($arg)*);
        }
    };
}

/// Log an error with context payload.
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {{
        log::error!("{}: {}", *$error as u64, $context);
    }};
}

/// Initialisation-step serial log (always writes to COM1, no allocation).
#[macro_export]
macro_rules! init_log {
    ($msg:literal) => {
        $crate::write_serial_bytes(0x3F8, 0x3FD, concat!($msg, "\n").as_bytes());
    };
    ($fmt:expr $(, $($arg:tt)*)?) => {
        $crate::serial::serial_log(format_args!(concat!($fmt, "\n") $(, $($arg)*)?));
    };
}

/// Shorthand for "module initialized".
#[macro_export]
macro_rules! declare_init {
    ($mod_name:expr) => {{
        $crate::serial::serial_log(format_args!("{} initialized\n", $mod_name));
    }};
}

/// Stack-allocated `fmt::Write` target.
struct StackWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> core::fmt::Write for StackWriter<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.pos + bytes.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }
}

/// Enhanced logging macro for common patterns.
#[macro_export]
macro_rules! log {
    ($prefix:literal) => {
        $crate::serial::_print(format_args!(concat!($prefix, "\n")));
    };
    ($prefix:literal, $msg:expr) => {
        $crate::serial::_print(format_args!(concat!($prefix, ": {}\n"), $msg));
    };
    ($prefix:literal, $format:expr, $($args:tt)*) => {
        $crate::serial::_print(format_args!(concat!($prefix, ": ", $format, "\n"), $($args)*));
    };
}
