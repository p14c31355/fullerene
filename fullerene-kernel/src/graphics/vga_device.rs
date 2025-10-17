use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::{Color, ColorCode, ScreenChar, TextBufferOperations};

use crate::traits::{ErrorLogging, HardwareDevice, Initializable};

// Constants to reduce magic numbers
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const VGA_BUFFER_ADDR: usize = 0xb8000;

#[derive(Clone)]
/// VGA text mode buffer wrapper that implements TextBufferOperations
pub struct VgaBuffer {
    enabled: bool,
    color_code: ColorCode,
    cursor_row: usize,
    cursor_col: usize,
}

impl VgaBuffer {
    pub fn new() -> Self {
        Self {
            enabled: false,
            color_code: ColorCode::new(Color::Green, Color::Black),
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn get_buffer(&mut self) -> Option<&mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]> {
        if self.enabled {
            Some(unsafe { &mut *(VGA_BUFFER_ADDR as *mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) })
        } else {
            None
        }
    }
}

impl TextBufferOperations for VgaBuffer {
    fn get_width(&self) -> usize {
        VGA_WIDTH
    }

    fn get_height(&self) -> usize {
        VGA_HEIGHT
    }

    fn get_color_code(&self) -> ColorCode {
        self.color_code
    }

    fn get_position(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    fn set_position(&mut self, row: usize, col: usize) {
        self.cursor_row = row;
        self.cursor_col = col;
    }

    fn set_char_at(&mut self, row: usize, col: usize, chr: ScreenChar) {
        if self.enabled && row < VGA_HEIGHT && col < VGA_WIDTH {
            if let Some(buffer) = self.get_buffer() {
                buffer[row][col] = chr;
            }
        }
    }

    fn get_char_at(&self, row: usize, col: usize) -> ScreenChar {
        if self.enabled && row < VGA_HEIGHT && col < VGA_WIDTH {
            // Get buffer directly for immutable access
            let buffer =
                unsafe { &*(VGA_BUFFER_ADDR as *const [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) };
            buffer[row][col]
        } else {
            ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            }
        }
    }

    fn scroll_up(&mut self) {
        if let Some(buffer) = self.get_buffer() {
            for row in 1..VGA_HEIGHT {
                for col in 0..VGA_WIDTH {
                    buffer[row - 1][col] = buffer[row][col];
                }
            }
            self.clear_row(VGA_HEIGHT - 1);
        }
    }

    fn clear_row(&mut self, row: usize) {
        if self.enabled {
            let color_code = self.color_code;
            if let Some(buffer) = self.get_buffer() {
                let blank = ScreenChar {
                    ascii_character: b' ',
                    color_code,
                };
                for col in 0..VGA_WIDTH {
                    buffer[row][col] = blank;
                }
            }
        }
    }
}

/// VGA text mode device implementation
#[derive(Clone)]
pub struct VgaDevice {
    buffer: VgaBuffer,
}

impl VgaDevice {
    pub fn new() -> Self {
        Self {
            buffer: VgaBuffer::new(),
        }
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.buffer.color_code = ColorCode::new(foreground, background);
    }
}

impl Initializable for VgaDevice {
    fn init(&mut self) -> SystemResult<()> {
        // VGA buffer is accessed directly when needed
        log::info!("VGA device initialized");
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
        log::error!("{}: {:?}", context, error);
    }

    fn log_warning(&self, message: &'static str) {
        log::warn!("{}", message);
    }

    fn log_info(&self, message: &'static str) {
        log::info!("{}", message);
    }

    fn log_debug(&self, message: &'static str) {
        log::debug!("{}", message);
    }

    fn log_trace(&self, message: &'static str) {
        log::trace!("{}", message);
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
        self.buffer.enabled = true;
        log::info!("VGA device enabled");
        Ok(())
    }

    fn disable(&mut self) -> SystemResult<()> {
        self.buffer.enabled = false;
        log::info!("VGA device disabled");
        Ok(())
    }

    fn reset(&mut self) -> SystemResult<()> {
        if self.buffer.enabled {
            // Clear the entire buffer
            let color_code = self.buffer.color_code;
            if let Some(buffer) = self.buffer.get_buffer() {
                for row in 0..VGA_HEIGHT {
                    let blank = ScreenChar {
                        ascii_character: b' ',
                        color_code,
                    };
                    for col in 0..VGA_WIDTH {
                        buffer[row][col] = blank;
                    }
                }
            }
            self.buffer.cursor_row = 0;
            self.buffer.cursor_col = 0;
        }
        log::info!("VGA device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.buffer.enabled
    }

    fn priority(&self) -> i32 {
        <Self as Initializable>::priority(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vga_device_creation() {
        let device = VgaDevice::new();
        assert_eq!(device.device_name(), "VGA Text Mode Display");
        assert_eq!(device.device_type(), "Display");
        assert!(!device.is_enabled());
    }

    #[test]
    fn test_vga_device_initialization() {
        let device = VgaDevice::new();
        // Note: We can't actually test init() in unit tests due to unsafe code
        // This would require integration testing
        assert_eq!(device.name(), "VgaDevice");
        assert_eq!(<VgaDevice as Initializable>::priority(&device), 10);
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
