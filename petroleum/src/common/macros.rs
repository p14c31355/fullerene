/// Macro for reduce code duplication in command arrays
#[macro_export]
macro_rules! command_args {
    () => {
        &[]
    };
    ($($arg:expr),* $(,)?) => {
        &[$($arg.to_string()),*]
    };
}

/// Enhanced delegate call macro using generic patterns
#[macro_export]
macro_rules! delegate_to_variant {
    ($enum:expr, $method:ident $(, $args:expr)*) => {
        match $enum {
            $(#[$derive(Debug)])* $enum_variant($variant) => $variant.$method($($args),*)
        }
    };
}

/// Generic helper for pattern-matched operations
#[macro_export]
macro_rules! match_and_apply {
    ($value:expr, $(($pattern:pat, $body:block)),* $(,)?) => {
        match $value {
            $($pattern => $body,)*
        }
    };
}

/// Macro for common initialization patterns with cleanup
#[macro_export]
macro_rules! init_with_cleanup {
    ($name:expr, $init:block, $cleanup:block) => {{
        $crate::serial::serial_log(format_args!("Initializing {}\n", $name));
        $init;
        $crate::serial::serial_log(format_args!("{} initialized successfully\n", $name));
        // Store cleanup for later if needed - would be part of an RAII pattern
        || $cleanup
    }};
}

/// Macro for modifying contents protected by a Mutex lock
#[macro_export]
macro_rules! lock_and_modify {
    ($lock:expr, $var:ident, $code:block) => {{
        let mut $var = $lock.lock();
        $code
    }};
}

/// Macro for logging errors with context
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {{
        log::error!("{}: {}", *$error as u64, $context);
    }};
}

/// Macro for reading contents protected by a Mutex lock (returns a copy/clone)
#[macro_export]
macro_rules! lock_and_read {
    ($lock:expr, $var:ident, $val:expr) => {{
        let $var = $lock.lock();
        $val
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
                log::info!(concat!($name, " initialized successfully"));
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to initialize {}: {:?}", $name, e);
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
#[macro_export]
macro_rules! ensure {
    ($condition:expr, $error:expr) => {
        if !$condition {
            petroleum::log_error!($error, stringify!($condition));
            return Err(*$error);
        }
    };
}

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
            petroleum::log_error!($error, $msg);
            return Err(*$error);
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
                petroleum::log_error!($error, "Option was None");
                Err(*$error)
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
                petroleum::log_error!(e, $context);
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
