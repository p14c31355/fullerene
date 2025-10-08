use petroleum::serial;
use spin::Mutex;

/// Provides a global singleton for the serial port
pub(crate) static SERIAL1: Mutex<serial::SerialPort<serial::Com1Ops>> = Mutex::new(serial::SerialPort::new(serial::Com1Ops));

/// Initializes the global serial port.
pub fn serial_init() {
    SERIAL1.lock().init();
}

/// Logs a string to the serial port.
pub fn serial_log(s: &str) {
    use core::fmt::Write;
    let _ = writeln!(SERIAL1.lock(), "{}", s);
}
