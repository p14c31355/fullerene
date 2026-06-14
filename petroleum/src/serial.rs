pub const COM1_DATA_PORT: u16 = 0x3F8;
pub const COM1_STATUS_PORT: u16 = 0x3FD;

pub unsafe fn write_serial_bytes(port_addr: u16, status_port_addr: u16, bytes: &[u8]) {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        use x86_64::instructions::port::Port;
        let mut port = Port::<u8>::new(port_addr);
        let mut status_port = Port::<u8>::new(status_port_addr);
        for &byte in bytes {
            unsafe {
                let mut timeout = 1000000;
                while (status_port.read() & 0x20) == 0 && timeout > 0 {
                    timeout -= 1;
                }
                port.write(byte);
            }
        }
    }
    #[cfg(any(feature = "std", test))]
    {
        // Avoid direct port I/O in std environment or during tests to prevent SIGSEGV
    }
}

use crate::common::{EfiSimpleTextOutput, EfiStatus};
use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::port::Port;

// Generic serial port implementation that works with different bases
pub trait SerialPortOps {
    fn data_port(&self) -> Port<u8>;
    fn irq_enable_port(&self) -> Port<u8>;
    fn fifo_ctrl_port(&self) -> Port<u8>;
    fn line_ctrl_port(&self) -> Port<u8>;
    fn modem_ctrl_port(&self) -> Port<u8>;
    fn line_status_port(&self) -> Port<u8>;
}

/// Represents a serial port for communication.
pub struct SerialPort<S: SerialPortOps> {
    ops: S,
}

impl<S: SerialPortOps> SerialPort<S> {
    /// Creates a new instance of the SerialPort.
    pub const fn new(ops: S) -> SerialPort<S> {
        SerialPort { ops }
    }

    /// Initializes the serial port.
    pub fn init(&mut self) {
        crate::init_serial_port!(
            self.ops.line_ctrl_port(),
            self.ops.data_port(),
            self.ops.irq_enable_port(),
            self.ops.fifo_ctrl_port(),
            self.ops.modem_ctrl_port(),
            0x80,
            0x03,
            0x00,
            0x03,
            0xC7,
            0x0B
        );
    }

    /// Writes a single byte to the serial port.
    pub fn write_byte(&mut self, byte: u8) {
        #[cfg(all(not(feature = "std"), not(test)))]
        unsafe {
            let mut timeout = 1000000;
            while (self.ops.line_status_port().read() & 0x20) == 0 && timeout > 0 {
                timeout -= 1;
            }
            self.ops.data_port().write(byte);
        }
        #[cfg(any(feature = "std", test))]
        {
            // Avoid direct port I/O in std environment or during tests to prevent SIGSEGV
        }
    }

    /// Writes a string to the serial port.
    pub fn write_string(&mut self, s: &str) {
        for b in s.bytes() {
            self.write_byte(b);
        }
    }
}

/// COM1 implementation
pub struct Com1Ports;

impl SerialPortOps for Com1Ports {
    fn data_port(&self) -> Port<u8> {
        Port::new(0x3F8)
    }
    fn irq_enable_port(&self) -> Port<u8> {
        Port::new(0x3F9)
    }
    fn fifo_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FA)
    }
    fn line_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FB)
    }
    fn modem_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FC)
    }
    fn line_status_port(&self) -> Port<u8> {
        Port::new(0x3FD)
    }
}

impl<S: SerialPortOps> fmt::Write for SerialPort<S> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// Capability structure for managing serial and UEFI output
pub struct SerialManager {
    serial_port: SerialPort<Com1Ports>,
    uefi_writer: UefiWriter,
}

impl SerialManager {
    pub fn new() -> Self {
        Self {
            serial_port: SerialPort::new(Com1Ports),
            uefi_writer: UefiWriter::new(),
        }
    }

    pub fn init_serial(&mut self) {
        self.serial_port.init();
    }

    pub fn init_uefi(&mut self, con_out: *mut EfiSimpleTextOutput) {
        self.uefi_writer.init(con_out);
    }

    pub fn write_serial(&mut self, s: &str) {
        self.serial_port.write_string(s);
    }

    pub fn write_uefi(&mut self, s: &str) {
        self.uefi_writer.write_string(s);
    }
}

// Global serial manager instance for kernel-wide access.
pub static SERIAL_MANAGER: Mutex<Option<SerialManager>> = Mutex::new(None);

// Replaced by line 153 definition

pub struct UefiWriter {
    con_out: *mut EfiSimpleTextOutput,
}

unsafe impl Sync for UefiWriter {}
unsafe impl Send for UefiWriter {}

impl Default for UefiWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl UefiWriter {
    pub const fn new() -> UefiWriter {
        UefiWriter {
            con_out: core::ptr::null_mut(),
        }
    }

    pub fn init(&mut self, con_out: *mut EfiSimpleTextOutput) {
        self.con_out = con_out;
    }

    pub fn write_string_heapless(&mut self, s: &str) -> Result<(), EfiStatus> {
        if self.con_out.is_null() {
            return Ok(());
        }

        let mut utf16_buf = [0u16; 512];
        let mut idx = 0;
        for c in s.encode_utf16() {
            if idx < utf16_buf.len() - 1 {
                utf16_buf[idx] = c;
                idx += 1;
            } else {
                break;
            }
        }
        utf16_buf[idx] = 0;

        let status = unsafe { ((*self.con_out).output_string)(self.con_out, utf16_buf.as_ptr()) };
        let efi_status = EfiStatus::from(status);
        if efi_status != EfiStatus::Success {
            unsafe { write_serial_bytes(COM1_DATA_PORT, COM1_STATUS_PORT, s.as_bytes()) };
            return Err(efi_status);
        }
        Ok(())
    }

    pub fn write_string(&mut self, s: &str) {
        self.write_string_heapless(s).ok();
    }
}

impl fmt::Write for UefiWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string_heapless(s).map_err(|_| fmt::Error)
    }
}

/// Global UEFI writer for panic/error handling in UEFI context.
pub static UEFI_WRITER: Mutex<UefiWriter> = Mutex::new(UefiWriter::new());

/// Serial logging using a SerialManager (capability-based version).
pub fn serial_log_with_manager(manager: &mut SerialManager, args: core::fmt::Arguments) {
    _print_with_manager(manager, args);
}

/// Serial logging without a SerialManager (uses direct port I/O).
/// This is the backward-compatible version used by macros and early boot code.
pub fn serial_log(args: core::fmt::Arguments) {
    _print(args);
}

/// Writes a single byte to the COM1 serial port using a SerialManager capability.
pub fn debug_print_byte_to_com1(manager: &mut SerialManager, byte: u8) {
    manager.serial_port.write_byte(byte);
}

/// Writes a string to the COM1 serial port using a SerialManager capability.
pub fn debug_print_str_to_com1(manager: &mut SerialManager, s: &str) {
    manager.write_serial(s);
}

/// Print using a SerialManager (capability-based version).
pub fn _print_with_manager(manager: &mut SerialManager, args: fmt::Arguments) {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        manager.serial_port.write_fmt(args).ok();
    }
}

/// Print directly to COM1 serial port without a SerialManager.
/// Uses direct port I/O for early boot / macro convenience.
///
/// If the xHCI Debug Capability (DbC) has been initialized (via
/// `nitrogen::xhci_dbc`), the same output is also sent through the
/// USB debug channel.
pub fn _print(args: fmt::Arguments) {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        use core::fmt::Write;

        // Format into a small buffer first to send to both COM1 and DbC.
        // We use a fixed-size stack buffer to avoid heap allocation.
        let mut buf: alloc::string::String = alloc::string::String::with_capacity(256);
        let _ = write!(buf, "{}", args);

        // Send to COM1 serial port (character-by-character)
        let mut port = SerialPort::new(Com1Ports);
        port.write_string(&buf);
    }
}

/// Initializes the serial port and returns a SerialManager capability.
pub fn serial_init() -> SerialManager {
    let mut manager = SerialManager::new();

    unsafe {
        crate::write_serial_bytes(
            COM1_DATA_PORT,
            COM1_STATUS_PORT,
            b"DEBUG: Inside serial_init\n",
        );
    }

    manager.init_serial();

    unsafe {
        crate::write_serial_bytes(
            COM1_DATA_PORT,
            COM1_STATUS_PORT,
            b"DEBUG: serial_init completed successfully\n",
        );
    }

    manager
}

/// Formats a u64 value as hex to a byte buffer with limited digits.
pub fn format_hex_to_buffer(value: u64, buf: &mut [u8], max_digits: usize) -> usize {
    let mut temp = value;
    let mut i = 0;
    let mut digit_buf = [0u8; 16];
    if temp == 0 {
        buf[0] = b'0';
        return 1;
    }
    while temp > 0 && i < max_digits && i < 16 {
        let digit = (temp % 16) as u8;
        digit_buf[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        temp /= 16;
        i += 1;
    }
    for j in 0..i {
        buf[j] = digit_buf[i - 1 - j];
    }
    i
}

/// Formats a usize value as decimal to a byte buffer.
pub fn format_dec_to_buffer(value: usize, buf: &mut [u8]) -> usize {
    let mut temp = value;
    let mut i = 0;
    let mut digit_buf = [0u8; 16];
    if temp == 0 {
        buf[0] = b'0';
        return 1;
    }
    while temp > 0 && i < 16 {
        let digit = (temp % 10) as u8;
        digit_buf[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        temp /= 16;
        i += 1;
    }
    for j in 0..i {
        buf[j] = digit_buf[i - 1 - j];
    }
    i
}

/// Hex print using a SerialManager (capability-based version).
pub fn debug_print_hex_no_lock_with_manager(manager: &mut SerialManager, value: usize) {
    let mut buf = [0u8; 16];
    let len = format_hex_to_buffer(value as u64, &mut buf, 16);
    manager.write_serial(core::str::from_utf8(&buf[..len]).unwrap());
}

/// Hex print without a SerialManager (uses direct port I/O).
pub fn debug_print_hex_no_lock(value: usize) {
    let mut buf = [0u8; 16];
    let len = format_hex_to_buffer(value as u64, &mut buf, 16);
    unsafe {
        write_serial_bytes(COM1_DATA_PORT, COM1_STATUS_PORT, &buf[..len]);
    }
}

/// High-level, safe logging function for early-boot (no locking, direct port I/O).
pub fn early_log(msg: &str) {
    unsafe {
        write_serial_bytes(COM1_DATA_PORT, COM1_STATUS_PORT, msg.as_bytes());
        write_serial_bytes(COM1_DATA_PORT, COM1_STATUS_PORT, b"\n");
    }
}

/// Early-boot non-locking string print
pub fn debug_print_str_no_lock(s: &str) {
    unsafe { write_serial_bytes(COM1_DATA_PORT, COM1_STATUS_PORT, &s.as_bytes()[..]) };
}

/// Trait for non-locking debug printing
pub trait DebugNoLock {
    fn debug_print_no_lock(self);
}

macro_rules! impl_debug_no_lock {
    ($($t:ty),*) => {
        $(
            impl DebugNoLock for $t {
                fn debug_print_no_lock(self) {
                    debug_print_hex_no_lock(self as usize);
                }
            }
        )*
    };
}

impl_debug_no_lock!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl DebugNoLock for &str {
    fn debug_print_no_lock(self) {
        debug_print_str_no_lock(self);
    }
}

/// Free function that dispatches to the DebugNoLock trait.
/// This allows macros to call `debug_print_no_lock(value)` without importing the trait.
pub fn debug_print_no_lock<T: DebugNoLock>(value: T) {
    value.debug_print_no_lock();
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_uefi_writer_new() {
        let writer = super::UefiWriter::new();
        assert!(writer.con_out.is_null());
    }
}
