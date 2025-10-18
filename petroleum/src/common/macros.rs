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
            $crate::log_error!($error, stringify!($condition));
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
            $crate::log_error!($error, $msg);
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
                $crate::log_error!($error, "Option was None");
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

/// Unified print macros using the log crate for consistent logging across all crates
/// Uses log::info! for println! and serial output for print!
#[macro_export]
macro_rules! println {
    () => {
        log::info!("");
    };
    ($($arg:tt)*) => {
        log::info!("{}", format_args!($($arg)*));
    };
}

/// Unified print macro using serial output for direct serial logging
#[macro_export]
macro_rules! print {
    () => {
        $crate::serial::_print(format_args!(""));
    };
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
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

/// Common logging macros (note: some may be defined in serial.rs)
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[INFO] {}\n", format_args!($($arg)*)));
    };
}

#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[ERROR] {}\n", format_args!($($arg)*)));
    };
}

#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[WARN] {}\n", format_args!($($arg)*)));
    };
}

/// PCI operation helper macros to reduce repetition in PCI handling
#[macro_export]
macro_rules! pci_read_bars {
    ($pci_io_ref:expr, $protocol_ptr:expr, $buf:expr, $count:expr, $offset:expr) => {{
        ($pci_io_ref.pci_read)(
            $protocol_ptr,
            2, // Dword width
            $offset,
            $count,
            $buf.as_mut_ptr() as *mut core::ffi::c_void,
        )
    }};
}

/// Safely extract BAR value and check if memory-mapped
#[macro_export]
macro_rules! extract_bar_info {
    ($bars:expr, $bar_index:expr) => {{
        let bar = $bars[$bar_index] & 0xFFFFFFF0; // Mask off lower 4 bits
        let bar_type = $bars[$bar_index] & 0xF;
        let is_memory = (bar_type & 0x1) == 0;
        (bar, bar_type, is_memory)
    }};
}

/// Macro for framebuffer detection patterns
#[macro_export]
macro_rules! test_framebuffer_mode {
    ($addr:expr, $width:expr, $height:expr, $bpp:expr, $stride:expr) => {{
        let fb_size = ($height * $stride * $bpp / 8) as u64;
        if crate::graphics_alternatives::probe_framebuffer_access($addr, fb_size) {
            info_log!(
                "Detected valid framebuffer: {}x{} @ {:#x}",
                $width,
                $height,
                $addr
            );
            Some($crate::common::FullereneFramebufferConfig {
                address: $addr,
                width: $width,
                height: $height,
                pixel_format:
                    $crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp: $bpp,
                stride: $stride,
            })
        } else {
            warn_log!("Framebuffer mode {}x{} invalid", $width, $height);
            None
        }
    }};
}

/// Macro to set a bit field in a u32 word, reducing line count for repetitive bit operations
#[macro_export]
macro_rules! bit_field_set {
    ($field:expr, $mask:expr, $shift:expr, $value:expr) => {
        $field = ($field & !($mask << $shift)) | (($value as u32 & $mask) << $shift);
    };
}

/// Macro to set or clear a single bit based on bool value
#[macro_export]
macro_rules! set_bool_bit {
    ($field:expr, $bit:expr, $value:expr) => {
        if $value {
            $field |= 1 << $bit;
        } else {
            $field &= !(1 << $bit);
        }
    };
}

/// Macro to clear a 2D buffer with a value for trait-based buffers, reducing nested loop code
#[macro_export]
macro_rules! clear_buffer {
    ($buffer:expr, $height:expr, $width:expr, $value:expr) => {
        for row in 0..$height {
            for col in 0..$width {
                $buffer.set_char_at(row, col, $value);
            }
        }
    };
}

/// Macro to scroll up a 2D buffer for trait-based buffers, reducing loop code
#[macro_export]
macro_rules! scroll_buffer_up {
    ($buffer:expr, $height:expr, $width:expr, $blank:expr) => {
        for row in 1..$height {
            for col in 0..$width {
                let chr = $buffer.get_char_at(row, col);
                $buffer.set_char_at(row - 1, col, chr);
            }
        }
        for col in 0..$width {
            $buffer.set_char_at($height - 1, col, $blank);
        }
    };
}

/// Command definition macro to reduce repetitive command array initialization scatter
///
/// # Examples
/// ```
/// define_commands!(CommandEntry,
///     ("help", "Show help", help_fn),
///     ("exit", "Exit", exit_fn)
/// )
/// ```
#[macro_export]
macro_rules! define_commands {
    ($entry_ty:ident, $(($name:expr, $desc:expr, $func:expr)),* $(,)?) => {
        &[
            $(
                $entry_ty {
                    name: $name,
                    description: $desc,
                    function: $func,
                }
            ),*
        ]
    };
}

/// Macro for volatile memory read operations
#[macro_export]
macro_rules! volatile_read {
    ($addr:expr, $ty:ty) => {
        unsafe { core::ptr::read_volatile($addr as *const $ty) }
    };
}

/// Macro for volatile memory write operations
#[macro_export]
macro_rules! volatile_write {
    ($addr:expr, $value:expr) => {{
        unsafe { core::ptr::write_volatile($addr, $value) }
    }};
}

/// Macro for safe buffer index access with bounds checking
#[macro_export]
macro_rules! safe_buffer_access {
    ($buffer:expr, $index:expr, $default:expr) => {
        if $index < $buffer.len() {
            &$buffer[$index]
        } else {
            &$default
        }
    };
}

/// Macro for scrolling up a 2D character buffer (generic version)
#[macro_export]
macro_rules! scroll_char_buffer_up {
    ($buffer:expr, $height:expr, $width:expr, $blank:expr) => {
        for row in 1..$height {
            for col in 0..$width {
                $buffer[row - 1][col] = $buffer[row][col];
            }
        }
        for col in 0..$width {
            $buffer[$height - 1][col] = $blank;
        }
    };
}

/// Macro for generic text buffer operations in write_byte
#[macro_export]
macro_rules! handle_write_byte {
    ($self:expr, $byte:expr, $newline:block, $write_char:block) => {
        match $byte {
            b'\n' => $newline,
            byte => $write_char,
        }
    };
}

/// Macro to reduce boilerplate in error conversion implementations
/// Converts an error type to SystemError using a mapping closure
#[macro_export]
macro_rules! impl_error_from {
    ($src:ty, $dst:ty, $map_fn:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                ($map_fn)(error)
            }
        }
    };
}

/// Compact error conversion macro for common patterns where variants map directly
#[macro_export]
macro_rules! error_variant_map {
    ($src:ty, $dst:ty, $pat:pat => $result:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $pat => $result,
                }
            }
        }
    };
}

/// Macro for chained error conversions
#[macro_export]
macro_rules! error_chain {
    ($src:ty, $dst:ty, $( $pat:pat => $result:expr ),* $(,)?) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $(
                        $pat => $result,
                    )*
                }
            }
        }
    };
}

/// Macro for simple module initialization with logging
#[macro_export]
macro_rules! declare_init {
    ($mod_name:expr) => {{
        $crate::serial::serial_log(format_args!("{} initialized\n", $mod_name));
    }};
}

/// Macro for initialization steps/done with serial logging
#[macro_export]
macro_rules! init_log {
    ($msg:literal) => {
        write_serial_bytes!(0x3F8, 0x3FD, concat!($msg, "\n").as_bytes());
    };
}

/// Macro to update VGA cursor position by writing to ports
#[macro_export]
macro_rules! update_vga_cursor {
    ($pos:expr) => {{
        port_write!($crate::graphics::ports::HardwarePorts::CRTC_INDEX, 0x0Fu8);
        port_write!($crate::graphics::ports::HardwarePorts::CRTC_DATA, (($pos & 0xFFusize) as u8));
        port_write!($crate::graphics::ports::HardwarePorts::CRTC_INDEX, 0x0Eu8);
        port_write!($crate::graphics::ports::HardwarePorts::CRTC_DATA, ((($pos >> 8) & 0xFFusize) as u8));
    }};
}
