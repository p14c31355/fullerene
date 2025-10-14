//! Logging and utility macros for the Fullerene kernel
//!
//! This module provides convenient macros for logging and error handling
//! throughout the kernel.

/// Log an error message with context
///
/// # Examples
/// ```
/// log_error!(SystemError::InvalidArgument, "Failed to process syscall");
/// ```
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {{
        use petroleum::common::logging;
        logging::log_error($error, $context)
    }};
}

/// Log a warning message
///
/// # Examples
/// ```
/// log_warning!("This is a warning message");
/// ```
#[macro_export]
macro_rules! log_warning {
    ($message:expr) => {
        $crate::log_warning($message)
    };
}

/// Log an info message
///
/// # Examples
/// ```
/// log_info!("System initialized successfully");
/// ```
#[macro_export]
macro_rules! log_info {
    ($message:expr) => {{
        use petroleum::common::logging;
        logging::log_info($message)
    }};
}

/// Log a debug message (only if debug level is enabled)
///
/// # Examples
/// ```
/// log_debug!("Debug value: {}", some_value);
/// ```
#[macro_export]
macro_rules! log_debug {
    ($message:expr) => {{
        use petroleum::common::logging;
        logging::log_debug($message)
    }};
}

/// Log a trace message (only if trace level is enabled)
///
/// # Examples
/// ```
/// log_trace!("Detailed trace information");
/// ```
#[macro_export]
macro_rules! log_trace {
    ($message:expr) => {{
        use petroleum::common::logging;
        logging::log_trace($message)
    }};
}

/// Initialize a component and log the result
///
/// # Examples
/// ```
/// let mut component = SomeComponent::new();
/// init_component!(component, "ComponentName");
/// ```
#[macro_export]
macro_rules! init_component {
    ($component:expr, $name:expr) => {{
        match $component.init() {
            Ok(()) => {
                $crate::log_info(concat!($name, " initialized successfully"));
                Ok(())
            }
            Err(e) => {
                $crate::log_error!(e, concat!("Failed to initialize ", $name));
                Err(e)
            }
        }
    }};
}

/// Ensure a condition is true, otherwise log an error and return it
///
/// # Examples
/// ```
/// ensure!(ptr.is_some(), SystemError::InvalidArgument);
/// ```


/// Ensure a condition is true with a custom error message
///
/// # Examples
/// ```
/// ensure_with_msg!(ptr.is_some(), SystemError::InvalidArgument, "Pointer must not be null");
/// ```
#[macro_export]
macro_rules! ensure_with_msg {
    ($condition:expr, $error:expr, $msg:expr) => {
        if !$condition {
            $crate::log_error!($error, $msg);
            return Err($error);
        }
    };
}

/// Convert an option to a result with error logging
///
/// # Examples
/// ```
/// let value = option_to_result!(some_option, SystemError::NotFound);
/// ```
#[macro_export]
macro_rules! option_to_result {
    ($option:expr, $error:expr) => {
        match $option {
            Some(value) => Ok(value),
            None => {
                $crate::log_error!($error, "Option was None");
                Err($error)
            }
        }
    };
}

/// Execute an expression and log if it fails
///
/// # Examples
/// ```
/// let result = try_or_log!(some_fallible_operation(), "Operation failed");
/// ```
#[macro_export]
macro_rules! try_or_log {
    ($expr:expr, $context:expr) => {
        match $expr {
            Ok(value) => value,
            Err(e) => {
                $crate::log_error!(e, $context);
                return Err(e);
            }
        }
    };
}

/// Create a static string slice for use in logging
///
/// # Examples
/// ```
/// const COMPONENT_NAME: &str = static_str!("MemoryManager");
/// ```
#[macro_export]
macro_rules! static_str {
    ($s:expr) => {{
        const S: &str = $s;
        S
    }};
}

/// Macro for debug logging in UEFI context
#[macro_export]
macro_rules! kernel_log {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        // Use a single lock to prevent potential deadlocks and improve efficiency.
        let _ = writeln!(&mut *petroleum::serial::SERIAL_PORT_WRITER.lock(), $($arg)*);
    }};
}

#[cfg(test)]
mod tests {

    use crate::*;

    #[test]
    fn test_logging_macros() {
        // Test that macros compile correctly
        log_error!(SystemError::InvalidArgument, "Test error");
        log_warning!("Test warning");
        log_info!("Test info");
        log_debug!("Test debug");
        log_trace!("Test trace");
    }

    #[test]
    fn test_utility_macros() {
        // Test ensure macro
        let result: SystemResult<()> = (|| {
            ensure!(true, SystemError::InvalidArgument);
            Ok(())
        })();
        assert!(result.is_ok());

        // Test ensure_with_msg macro
        let result: SystemResult<()> = (|| {
            ensure_with_msg!(false, SystemError::InvalidArgument, "Test message");
            Ok(())
        })();
        assert!(result.is_err());

        // Test option_to_result macro
        let some_value = Some(42);
        let none_value: Option<i32> = None;

        assert_eq!(
            option_to_result!(some_value, SystemError::FileNotFound),
            Ok(42)
        );
        assert_eq!(
            option_to_result!(none_value, SystemError::FileNotFound),
            Err(SystemError::FileNotFound)
        );
    }
}
