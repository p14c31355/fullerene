// Helper macros to reduce code duplication across the project

/// Macro for common Mutex.lock() pattern to reduce line count
#[macro_export]
macro_rules! lock_and_modify {
    ($mutex:expr, $ident:ident, $body:block) => {
        let mut $ident = $mutex.lock();
        $body
    };
}

/// Macro for read-only lock access
#[macro_export]
macro_rules! lock_and_read {
    ($mutex:expr, $ident:ident, $body:expr) => {{
        let $ident = $mutex.lock();
        $body
    }};
}

/// Convenient logging macros
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {
        $crate::common::logging::log_error($error, $context)
    };
}

#[macro_export]
macro_rules! log_warning {
    ($message:expr) => {
        $crate::common::logging::log_warning($message)
    };
}

#[macro_export]
macro_rules! log_info {
    ($message:expr) => {
        $crate::common::logging::log_info($message)
    };
}

#[macro_export]
macro_rules! log_debug {
    ($message:expr) => {
        $crate::common::logging::log_debug($message)
    };
}

#[macro_export]
macro_rules! log_trace {
    ($message:expr) => {
        $crate::common::logging::log_trace($message)
    };
}



/// Macro for kernel-specific logging
#[macro_export]
macro_rules! kernel_log {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        // Use a single lock to prevent potential deadlocks and improve efficiency.
        let _ = writeln!(&mut *petroleum::serial::SERIAL_PORT_WRITER.lock(), $($arg)*);
    }};
}



// Removed kernel_print macros - moved to fullerene-kernel crate

// Helper for repeated initialization patterns
#[macro_export]
macro_rules! init_once {
    ($once:expr, $init:expr) => {
        $once.call_once(|| $init)
    };
}

// Simplified panic handler to avoid code duplication
#[macro_export]
macro_rules! common_panic {
    ($info:expr) => {{
        #[cfg(any(target_os = "uefi", test))]
        {
            use core::fmt::Write;
            // For UEFI, try to output to serial/console then loop
            if let Some(st_ptr) = $crate::UEFI_SYSTEM_TABLE.lock().as_ref() {
                let st_ref = unsafe { &*st_ptr.0 };
                unsafe {
                    let msg = b"PANIC!\0";
                    let mut wide_msg = [0u16; 16];
                    for (i, &b) in msg.iter().enumerate() {
                        if b == 0 {
                            break;
                        }
                        wide_msg[i] = b as u16;
                    }
                    if let Some(con_out) = st_ref.con_out.as_mut() {
                        let _ = ((*con_out).output_string)(con_out, wide_msg.as_ptr());
                    }
                }
            }
            loop {
                unsafe {
                    x86_64::instructions::hlt();
                }
            }
        }
        #[cfg(not(any(target_os = "uefi", test)))]
        $crate::handle_panic($info)
    }};
}

// Common initialization pattern with logging
#[macro_export]
macro_rules! init_with_log {
    ($name:expr, $block:block) => {{
        $crate::serial::serial_log(format_args!("Initializing {}\n", $name));
        $block;
        $crate::serial::serial_log(format_args!("{} initialized successfully\n", $name));
    }};
}

// Helper macro for common PIC port write patterns
#[macro_export]
macro_rules! write_pic_register {
    ($pic_idx:expr, $icw1:expr, $icw2:expr, $icw3:expr) => {{
        use x86_64::instructions::port::Port;
        unsafe {
            let mut command_port = Port::<u8>::new(0x20 + $pic_idx * 0x80);
            let mut data_port = Port::<u8>::new(0x21 + $pic_idx * 0x80);

            command_port.write($icw1);
            data_port.write($icw2);
            data_port.write($icw3);
            data_port.write(0x01); // ICW4
        }
    }};
}
