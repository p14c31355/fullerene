//! Port I/O operations
//!
//! Types delegated to `nitrogen`; macros kept here for `$crate` path
//! compatibility with existing petroleum consumers.

// ── Macros ───────────────────────────────────────────────────────────
// These are defined locally so that `$crate` resolves to `petroleum`
// (not `nitrogen`), preserving compatibility with existing callers.

/// Macro to safely write to a port with automatic type deduction
#[macro_export]
macro_rules! port_write {
    ($port_addr:expr, $value:expr) => {{
        let mut writer = nitrogen::port::PortWriter::new($port_addr);
        writer.write_safe($value);
    }};
}

/// Macro to safely read from a port with automatic type deduction
#[macro_export]
macro_rules! port_read_u8 {
    ($port_addr:expr) => {{
        let mut reader: nitrogen::port::PortWriter<u8> =
            nitrogen::port::PortWriter::new($port_addr);
        reader.read_safe()
    }};
}

#[macro_export]
macro_rules! port_read {
    ($port_addr:expr) => {
        port_read_u8!($port_addr)
    };
}

/// Enhanced macro for writing port sequences with automatic port management
#[macro_export]
macro_rules! write_port_sequence {
    ($($config:expr, $index_port:expr, $data_port:expr);*$(;)?) => {{
        $(
            let mut vga_ports = nitrogen::port::VgaPortOps::new($index_port, $data_port);
            vga_ports.write_sequence($config);
        )*
    }};
}

/// Simplified macro for single register writes
#[macro_export]
macro_rules! write_vga_register {
    ($index_port:expr, $data_port:expr, $index:expr, $data:expr) => {{
        let mut vga_ports = nitrogen::port::VgaPortOps::new($index_port, $data_port);
        vga_ports.write_register($index, $data);
    }};
}
