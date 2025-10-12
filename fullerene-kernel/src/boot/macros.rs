use petroleum::serial::SERIAL_PORT_WRITER;

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        // Use a single lock to prevent potential deadlocks and improve efficiency.
        let _ = writeln!(&mut *petroleum::serial::SERIAL_PORT_WRITER.lock(), $($arg)*);
    }};
}
