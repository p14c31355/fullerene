use petroleum::{Color, ColorCode, ScreenChar};

use crate::traits::{ErrorLogging, HardwareDevice, Initializable};

// Constants to reduce magic numbers
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const VGA_BUFFER_ADDR: usize = 0xb8000;

/// VGA text mode device implementation
#[derive(Clone)]
pub struct VgaDevice {
    enabled: bool,
    color_code: ColorCode,
    cursor_row: usize,
    cursor_col: usize,
}

impl VgaDevice {
    pub fn new() -> Self {
        Self {
            enabled: false,
            color_code: ColorCode::new(Color::Green, Color::Black),
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    pub fn write_string(&mut self, s: &str) {
        if self.enabled {
            let buffer = unsafe { &mut *(VGA_BUFFER_ADDR as *mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) };
            self.write_string_to_buffer(s, buffer);
        }
    }

    fn write_string_to_buffer(&mut self, s: &str, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) {
        for byte in s.bytes() {
            match byte {
                b'\n' => self.handle_newline(buffer),
                byte => self.handle_character(byte, buffer),
            }
        }
    }

    fn handle_newline(&mut self, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) {
        self.cursor_row += 1;
        self.cursor_col = 0;
        if self.cursor_row >= VGA_HEIGHT {
            self.scroll_buffer(buffer);
            self.cursor_row = VGA_HEIGHT - 1;
        }
    }

    fn handle_character(&mut self, byte: u8, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) {
        if self.cursor_col >= VGA_WIDTH {
            self.handle_newline(buffer);
        }

        buffer[self.cursor_row][self.cursor_col] = ScreenChar {
            ascii_character: byte,
            color_code: self.color_code,
        };
        self.cursor_col += 1;
    }

    fn scroll_buffer(&self, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) {
        for row in 1..VGA_HEIGHT {
            for col in 0..VGA_WIDTH {
                buffer[row - 1][col] = buffer[row][col];
            }
        }
        self.clear_row(buffer, VGA_HEIGHT - 1);
    }

    fn clear_row(&self, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT], row: usize) {
        let blank = ScreenChar { ascii_character: b' ', color_code: self.color_code };
        for col in 0..VGA_WIDTH {
            buffer[row][col] = blank;
        }
    }

    fn clear_buffer(&self, buffer: &mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) {
        for row in 0..VGA_HEIGHT {
            self.clear_row(buffer, row);
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

    fn log_debug(&self, message: &'static str) {
        log_debug!(message);
    }

    fn log_trace(&self, message: &'static str) {
        log_trace!(message);
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
            let buffer = unsafe { &mut *(VGA_BUFFER_ADDR as *mut [[ScreenChar; VGA_WIDTH]; VGA_HEIGHT]) };
            self.clear_buffer(buffer);
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
        log_info!("VGA device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled
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
        let mut device = VgaDevice::new();
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
