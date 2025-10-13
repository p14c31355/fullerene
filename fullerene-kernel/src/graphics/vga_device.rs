//! VGA Device Implementation
//!
//! This module provides a VGA device that implements the HardwareDevice trait
//! for unified hardware abstraction.

use crate::*;
use petroleum::{Color, ColorCode, ScreenChar};
use spin::Mutex;

/// VGA text mode device implementation
pub struct VgaDevice {
    enabled: bool,
    color_code: ColorCode,
    cursor_row: usize,
    cursor_col: usize,
}

impl VgaDevice {
    /// Create a new VGA device
    pub fn new() -> Self {
        Self {
            enabled: false,
            color_code: ColorCode::new(Color::Green, Color::Black),
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    /// Set the color code for text output
    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    /// Write a string to the VGA buffer
    pub fn write_string(&self, s: &str) {
        if self.enabled {
            let buffer = unsafe { &mut *(0xb8000 as *mut [[ScreenChar; 80]; 25]) };
            self.write_string_to_buffer(s, buffer);
        }
    }

    fn write_string_to_buffer(&self, s: &str, buffer: &mut [[ScreenChar; 80]; 25]) {
        let mut column = 0;
        let mut row = 0;

        for byte in s.bytes() {
            match byte {
                b'\n' => {
                    row += 1;
                    column = 0;
                    if row >= 25 {
                        self.scroll_buffer(buffer);
                        row = 24;
                    }
                }
                byte => {
                    if column >= 80 {
                        row += 1;
                        column = 0;
                        if row >= 25 {
                            self.scroll_buffer(buffer);
                            row = 24;
                        }
                    }

                    buffer[row][column] = ScreenChar {
                        ascii_character: byte,
                        color_code: self.color_code,
                    };
                    column += 1;
                }
            }
        }
    }

    fn scroll_buffer(&self, buffer: &mut [[ScreenChar; 80]; 25]) {
        for row in 1..25 {
            for col in 0..80 {
                buffer[row - 1][col] = buffer[row][col];
            }
        }

        // Clear the last row
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..80 {
            buffer[24][col] = blank;
        }
    }

    fn clear_buffer(&self, buffer: &mut [[ScreenChar; 80]; 25]) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for row in 0..25 {
            for col in 0..80 {
                buffer[row][col] = blank;
            }
        }
    }
}

impl Initializable for VgaDevice {
    fn init(&mut self) -> SystemResult<()> {
        // VGA buffer is accessed directly when needed
        log_info!("VGA device initialized");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "VgaDevice"
    }

    fn priority(&self) -> i32 {
        10 // High priority for display device
    }
}

impl ErrorLogging for VgaDevice {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log_error!(error, context);
    }

    fn log_warning(&self, message: &'static str) {
        log_warning!(message);
    }

    fn log_info(&self, message: &'static str) {
        log_info!(message);
    }
}

impl HardwareDevice for VgaDevice {
    fn device_name(&self) -> &'static str {
        "VGA Text Mode Display"
    }

    fn device_type(&self) -> &'static str {
        "Display"
    }

    fn enable(&mut self) -> SystemResult<()> {
        self.enabled = true;
        log_info!("VGA device enabled");
        Ok(())
    }

    fn disable(&mut self) -> SystemResult<()> {
        self.enabled = false;
        log_info!("VGA device disabled");
        Ok(())
    }

    fn reset(&mut self) -> SystemResult<()> {
        if self.enabled {
            let buffer = unsafe { &mut *(0xb8000 as *mut [[ScreenChar; 80]; 25]) };
            self.clear_buffer(buffer);
        }
        log_info!("VGA device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    #[test]
    fn test_vga_device_creation() {
        let device = VgaDevice::new();
        assert_eq!(device.device_name(), "VGA Text Mode Display");
        assert_eq!(device.device_type(), "Display");
        assert!(!device.is_enabled());
    }

    #[test]
    fn test_vga_device_initialization() {
        let mut device = VgaDevice::new();
        // Note: We can't actually test init() in unit tests due to unsafe code
        // This would require integration testing
        assert_eq!(device.name(), "VgaDevice");
        assert_eq!(device.priority(), 10);
    }

    #[test]
    fn test_vga_device_enable_disable() {
        let mut device = VgaDevice::new();

        assert!(device.enable().is_ok());
        assert!(device.is_enabled());

        assert!(device.disable().is_ok());
        assert!(!device.is_enabled());
    }

    #[test]
    fn test_vga_device_reset() {
        let mut device = VgaDevice::new();

        // Enable first, then reset should work
        device.enable().unwrap();
        assert!(device.reset().is_ok());
    }
}
