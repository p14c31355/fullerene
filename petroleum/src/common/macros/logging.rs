//! Logging and debug output macros for Fullerene OS

#[macro_export]
macro_rules! unified_logging {
    (mem_debug, $($args:tt)*) => {
        $crate::mem_debug!($($args)*);
    };
    (serial_str, $msg:literal) => {
        $crate::serial::debug_print_str_to_com1($msg);
    };
    (serial_hex, $value:expr) => {
        $crate::serial::debug_print_hex($value);
    };
    (verbose_print, literal, $msg:literal) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!($msg);
        } else {
            $crate::serial::_print(format_args!("{}\n", $msg));
        }
    };
    (verbose_print, args, $($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("{}\n", format_args!($($arg)*)));
        }
    };
}

#[macro_export]
macro_rules! debug_log_no_alloc {
    ($msg:literal) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, concat!($msg, "\n").as_bytes());
    }};
    ($value:expr) => {{
        $crate::serial::DebugNoLock::debug_print_no_lock($value);
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }};
    ($msg:literal, $($value:expr),* $(,)?) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, $msg.as_bytes());
        $(
            $crate::serial::DebugNoLock::debug_print_no_lock($value);
        )*
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }};
    ($prefix:literal, $string_var:expr) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, $prefix.as_bytes());
        $crate::serial::DebugNoLock::debug_print_no_lock($string_var);
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }};
}

#[macro_export]
macro_rules! mem_debug {
    () => {};
    ($value:expr, $($rest:tt)*) => {
        $crate::serial::debug_print_no_lock($value);
        $crate::mem_debug!($($rest)*);
    };
    ($value:expr) => {
        $crate::serial::debug_print_no_lock($value);
    };
}

#[macro_export]
macro_rules! debug_print {
    ($msg:literal) => {
        $crate::serial::debug_print_str_no_lock($msg);
    };
    ($value:expr) => {
        $crate::serial::debug_print_no_lock($value);
    };
}

#[macro_export]
macro_rules! log_page_table_op {
    ($operation:literal) => {
        mem_debug!($operation, "\n");
    };
    ($operation:literal, $msg:literal, $addr:expr) => {
        mem_debug!($operation, $msg, " addr=", $addr, "\n");
    };
    ($operation:literal, $phys:expr, $virt:expr, $pages:expr) => {
        mem_debug!(
            $operation, " phys=", $phys, " virt=", $virt, " pages=", $pages, "\n"
        );
    };
    ($stage:literal, $phys:expr, $virt:expr, $pages:expr) => {
        mem_debug!(
            "Memory mapping stage=",
            $stage,
            " phys=",
            $phys,
            " virt=",
            $virt,
            " pages=",
            $pages,
            "\n"
        );
    };
    ($operation:literal, $msg:literal) => {
        mem_debug!($operation, $msg, "\n");
    };
}

#[macro_export]
macro_rules! debug_log_validate_macro {
    ($field:expr, $value:expr) => {
        mem_debug!($field, " validated: ", $value, "\n");
    };
}

#[macro_export]
macro_rules! println {
    () => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("");
        } else {
            $crate::serial::_print(format_args!("\n"));
        }
    };
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("{}\n", format_args!($($arg)*)));
        }
    };
}

#[macro_export]
macro_rules! print {
    () => {
        $crate::println!();
    };
    ($($arg:tt)*) => {
        $crate::println!($($arg)*);
    };
}

#[macro_export]
macro_rules! bootloader_log {
    ($msg:literal) => {{
        petroleum::println!($msg);
        petroleum::serial::_print(format_args!("{}\n", $msg));
    }};
    ($msg:literal, $($args:expr),*) => {{
        petroleum::println!($msg, $($args),*);
        petroleum::serial::_print(format_args!("{}\n", format_args!($msg, $($args),*)));
    }};
}

#[macro_export]
macro_rules! bootloader_debug {
    ($msg:literal) => {{
        petroleum::println!($msg);
        petroleum::serial::_print(format_args!(concat!($msg, "\n")));
    }};
    ($msg:literal, $($args:expr),*) => {{
        petroleum::println!($msg, $($args),*);
        petroleum::serial::_print(format_args!(concat!($msg, "\n"), $($args),*));
    }};
}

#[macro_export]
macro_rules! log_memory_descriptor {
    ($desc:expr, $i:expr) => {
        crate::mem_debug!("Memory descriptor ", $i);
        crate::mem_debug!(
            ": type=",
            $desc.type_ as usize,
            ", phys_start=0x",
            $desc.physical_start as usize
        );
        crate::mem_debug!(
            ", virt_start=",
            $desc.virtual_start as usize,
            ", pages=",
            $desc.number_of_pages as usize
        );
        crate::mem_debug!("\n");
    };
}