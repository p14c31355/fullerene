//! Global logging system

use crate::types::{ErrorLogging, LogLevel, SystemResult};
use spin::Mutex;

// Global logger instance
#[derive(Clone)]
pub struct GlobalLogger {
    level: LogLevel,
}

impl GlobalLogger {
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

impl ErrorLogging for GlobalLogger {
    fn log_error(&self, error: &crate::errors::SystemError, context: &'static str) {
        if self.level >= LogLevel::Error {
            // Output error with code and context
            match error {
                crate::errors::SystemError::InvalidSyscall => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::BadFileDescriptor => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::PermissionDenied => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::FileNotFound => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::NoSuchProcess => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::InvalidArgument => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::SyscallOutOfMemory => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::FileExists => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::FsInvalidFileDescriptor => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::InvalidSeek => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::DiskFull => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::MappingFailed => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::UnmappingFailed => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::FrameAllocationFailed => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::MemOutOfMemory => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::InvalidFormat => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::LoadFailed => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::DeviceNotFound => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::DeviceError => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::PortError => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::NotImplemented => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::InternalError => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
                crate::errors::SystemError::UnknownError => petroleum::serial::serial_log(format_args!("[ERROR {}] {}\n", *error as u64, context)),
            }
        }
    }

    fn log_warning(&self, message: &'static str) {
        if self.level >= LogLevel::Warning {
            petroleum::serial::serial_log(format_args!("[WARNING] {}\n", message));
        }
    }

    fn log_info(&self, message: &'static str) {
        if self.level >= LogLevel::Info {
            petroleum::serial::serial_log(format_args!("[INFO] {}\n", message));
        }
    }
}

static GLOBAL_LOGGER: Mutex<Option<GlobalLogger>> = Mutex::new(None);

// Initialize global logger
pub fn init_global_logger() {
    *GLOBAL_LOGGER.lock() = Some(GlobalLogger::new());
}

// Set global log level
pub fn set_global_log_level(level: LogLevel) {
}

// Get global log level
pub fn get_global_log_level() -> LogLevel {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.get_level()
    } else {
        LogLevel::Info // Default level
    }
}

// Global error logging functions
pub fn log_error(error: &crate::errors::SystemError, context: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_error(error, context);
    }
}

pub fn log_warning(message: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_warning(message);
    }
}

pub fn log_info(message: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_info(message);
    }
}
