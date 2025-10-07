// fullerene-kernel/src/serial.rs

use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Generic serial port implementation that works with different bases
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
        unsafe {
            self.ops.line_ctrl_port().write(0x80); // Enable DLAB
            self.ops.data_port().write(0x03); // Baud rate divisor low byte (38400 bps)
            self.ops.irq_enable_port().write(0x00);
            self.ops.line_ctrl_port().write(0x03); // 8 bits, no parity, one stop bit
            self.ops.fifo_ctrl_port().write(0xC7); // Enable FIFO, clear, 14-byte threshold
            self.ops.modem_ctrl_port().write(0x0B); // IRQs enabled, OUT2
        }
    }

    /// Writes a single byte to the serial port.
    pub fn write_byte(&mut self, byte: u8) {
        unsafe {
            while (self.ops.line_status_port().read() & 0x20) == 0 {}
            self.ops.data_port().write(byte);
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
pub struct Com1Ops;

impl SerialPortOps for Com1Ops {
    fn data_port(&self) -> Port<u8> { Port::new(0x3F8) }
    fn irq_enable_port(&self) -> Port<u8> { Port::new(0x3F9) }
    fn fifo_ctrl_port(&self) -> Port<u8> { Port::new(0x3FA) }
    fn line_ctrl_port(&self) -> Port<u8> { Port::new(0x3FB) }
    fn modem_ctrl_port(&self) -> Port<u8> { Port::new(0x3FC) }
    fn line_status_port(&self) -> Port<u8> { Port::new(0x3FD) }
}

// Provides a global singleton for the serial port
pub(crate) static SERIAL1: Mutex<SerialPort> = Mutex::new(SerialPort::new());

/// Initializes the global serial port.
pub fn serial_init() {
    SERIAL1.lock().init();
}

/// Logs a string to the serial port.
pub fn serial_log(s: &str) {
    let _ = writeln!(SERIAL1.lock(), "{}", s);
}

// Allows using `write!` and `writeln!` macros for the serial port
impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}
