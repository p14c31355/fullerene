//! Consolidated logging system for the Fullerene project
//!
//! This module provides unified logging macros and functions
//! for all sub-crates, reducing duplication and improving maintainability.

/// Global logger instance
#[derive(Clone)]
pub struct Logger {
    level: LogLevel,
}

impl Logger {
    pub const fn new() -> Self {
        Self {
            level: LogLevel::Info,
        }
    }

    pub fn set_level(&mut self, level: LogLevel) {
        self.level = level;
    }

    pub fn get_level(&self) -> LogLevel {
        self.level
    }
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
#[derive(Clone, Copy, PartialOrd, PartialEq)]
pub enum SystemError {
    InvalidSyscall = 0,
    BadFileDescriptor = 1,
    PermissionDenied = 2,
    FileNotFound = 3,
    NoSuchProcess = 4,
    InvalidArgument = 5,
    SyscallOutOfMemory = 6,
    FileExists = 7,
    FsInvalidFileDescriptor = 8,
    InvalidSeek = 9,
    DiskFull = 10,
    MappingFailed = 11,
    UnmappingFailed = 12,
    FrameAllocationFailed = 13,
    MemOutOfMemory = 14,
    InvalidFormat = 15,
    LoadFailed = 16,
    DeviceNotFound = 17,
    DeviceError = 18,
    PortError = 19,
    NotImplemented = 20,
    InternalError = 21,
    NotSupported = 22,
    UnknownError = 23,
}

/// Logging trait for system errors with context
pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
    fn log_debug(&self, message: &'static str);
    fn log_trace(&self, message: &'static str);
}

impl ErrorLogging for Logger {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        if self.level >= LogLevel::Error {
            use crate::serial;
            serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context));
        }
    }

    fn log_warning(&self, message: &'static str) {
        if self.level >= LogLevel::Warning {
            use crate::serial;
            serial::serial_log(format_args!("[WARNING] {}\n", message));
        }
    }

    fn log_info(&self, message: &'static str) {
        if self.level >= LogLevel::Info {
            use crate::serial;
            serial::serial_log(format_args!("[INFO] {}\n", message));
        }
    }

    fn log_debug(&self, message: &'static str) {
        if self.level >= LogLevel::Debug {
            use crate::serial;
            serial::serial_log(format_args!("[DEBUG] {}\n", message));
        }
    }

    fn log_trace(&self, message: &'static str) {
        if self.level >= LogLevel::Trace {
            use crate::serial;
            serial::serial_log(format_args!("[TRACE] {}\n", message));
        }
    }
}

static GLOBAL_LOGGER: spin::Mutex<Option<Logger>> = spin::Mutex::new(None);

/// Initialize global logger
pub fn init_global_logger() {
    let mut logger = GLOBAL_LOGGER.lock();
    *logger = Some(Logger::new());
}

/// Set global log level
pub fn set_global_log_level(level: LogLevel) {
    let mut logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_mut() {
        logger.set_level(level);
    }
}

/// Get global log level
pub fn get_global_log_level() -> LogLevel {
    let logger = GLOBAL_LOGGER.lock();
    logger.as_ref().map(|l| l.get_level()).unwrap_or(LogLevel::Info)
}

/// Global error logging functions
pub fn log_error(error: &SystemError, context: &'static str) {
    let logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_ref() {
        logger.log_error(error, context);
    }
}

pub fn log_warning(message: &'static str) {
    let logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_ref() {
        logger.log_warning(message);
    }
}

pub fn log_info(message: &'static str) {
    let logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_ref() {
        logger.log_info(message);
    }
}

pub fn log_debug(message: &'static str) {
    let logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_ref() {
        logger.log_debug(message);
    }
}

pub fn log_trace(message: &'static str) {
    let logger = GLOBAL_LOGGER.lock();
    if let Some(logger) = logger.as_ref() {
        logger.log_trace(message);
    }
}
