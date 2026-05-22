//! Terminal abstraction for Nozzle shell
//!
//! This trait decouples the shell from any specific I/O backend.
//! The kernel (or any other environment) provides a concrete implementation.

/// Abstract terminal I/O interface
pub trait Terminal {
    /// Write a string to the terminal
    fn write_str(&mut self, s: &str);

    /// Read a single byte from input, blocking until available.
    /// Returns `None` on end-of-input.
    fn read_byte(&mut self) -> Option<u8>;

    /// Check if input data is available without blocking
    fn input_available(&self) -> bool {
        false
    }
}
