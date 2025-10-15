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
