//! Consolidated logging system for the Fullerene project
//!
//! This module provides unified logging macros and functions using the
//! log crate for all sub-crates, reducing duplication and improving maintainability.

// Note: log crate is configured with default-features = false in Cargo.toml
// Re-export log crate macros for easy access
pub use log::{debug, error, info, trace, warn};

/// Global logger instance using log crate
pub struct FullereneLogger {
    level: log::LevelFilter,
}

impl FullereneLogger {
    pub const fn new() -> Self {
        Self {
            level: log::LevelFilter::Warn,
        }
    }
}

impl log::Log for FullereneLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            use crate::serial;
            serial::serial_log(format_args!("[{}] {}\n", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

// Initialize global logger
static LOGGER: FullereneLogger = FullereneLogger::new();

static LOGGER_INITIALIZED: spin::Once<()> = spin::Once::new();

pub fn init_global_logger() -> Result<(), log::SetLoggerError> {
    log::set_logger(&LOGGER)?;
    log::set_max_level(LOGGER.level);

    // Mark logger as initialized
    LOGGER_INITIALIZED.call_once(|| {});

    // Log successful initialization (using serial directly to avoid recursion)
    crate::serial::serial_log(format_args!(
        "[INIT] Logger initialized at level {:?}\n",
        LOGGER.level
    ));

    Ok(())
}

/// Set global log level
pub fn set_global_log_level(level: log::LevelFilter) {
    log::set_max_level(level);
}

/// Get global log level
pub fn get_global_log_level() -> log::LevelFilter {
    LOGGER.level
}

/// Log levels for hierarchical logging control
#[derive(Clone, Copy, PartialOrd, PartialEq)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
}

/// Unified result type for system operations
pub type SystemResult<T> = Result<T, SystemError>;

/// System error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemError {
    // System call errors
    InvalidSyscall = 1,
    BadFileDescriptor = 9,
    PermissionDenied = 13,
    FileNotFound = 2,
    NoSuchProcess = 3,
    InvalidArgument = 22,
    SyscallOutOfMemory = 12,

    // File system errors
    FileExists = 17,
    InvalidSeek = 29,
    DiskFull = 28,

    // Memory management errors
    MappingFailed = 100,
    UnmappingFailed = 101,
    FrameAllocationFailed = 102,
    MemOutOfMemory = 103,

    // Loader errors
    InvalidFormat = 200,
    LoadFailed = 201,

    // Hardware errors
    DeviceNotFound = 300,
    DeviceError = 301,
    PortError = 302,

    // General errors
    NotImplemented = 400,
    NotSupported = 401,
    InternalError = 500,
    UnknownError = 999,

    // Additional errors from fullerene-kernel
    FsInvalidFileDescriptor = 8,
}

/// Logging trait for system errors with context
pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
    fn log_debug(&self, message: &'static str);
    fn log_trace(&self, message: &'static str);
}

// Provide a compatibility layer that still allows structured error logging
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

// Global instance for convenience
pub static ERROR_LOGGER: ErrorLogger = ErrorLogger;

/// Returns true if global logger has been initialized
pub fn is_logger_initialized() -> bool {
    LOGGER_INITIALIZED.is_completed()
}

/// Enhanced logging macro for common patterns throughout the codebase
/// Provides consistent prefixes and formatting
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

/// Unified logging macros that use log crate when initialized, fallback to serial

/// Common logging macros - use log crate when initialized, fallback to serial
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

/// Macro for logging errors with context
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {{
        log::error!("{}: {}", *$error as u64, $context);
    }};
}

/// Macro for initialization steps/done with serial logging
#[macro_export]
macro_rules! init_log {
    ($msg:literal) => {{
        let msg = concat!($msg, "\n");
        $crate::write_serial_bytes!(0x3F8, 0x3FD, msg.as_bytes());
    }};
    ($fmt:expr $(, $($arg:tt)*)?) => {{
        $crate::serial::serial_log(format_args!(concat!($fmt, "\n") $(, $($arg)*)?));
    }};
}

/// Macro for simple module initialization with logging
#[macro_export]
macro_rules! declare_init {
    ($mod_name:expr) => {{
        $crate::serial::serial_log(format_args!("{} initialized\n", $mod_name));
    }};
}
