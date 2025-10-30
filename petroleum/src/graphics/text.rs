#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Color {
    Black = 0x0,
    Blue = 0x1,
    Green = 0x2,
    Cyan = 0x3,
    Red = 0x4,
    Magenta = 0x5,
    Brown = 0x6,
    LightGray = 0x7,
    DarkGray = 0x8,
    LightBlue = 0x9,
    LightGreen = 0xA,
    LightCyan = 0xB,
    LightRed = 0xC,
    Pink = 0xD,
    Yellow = 0xE,
    White = 0xF,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ColorCode(pub u8);

impl ColorCode {
    pub fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ScreenChar {
    pub ascii_character: u8,
    pub color_code: ColorCode,
}

// Trait for text buffer operations with default implementations
pub trait TextBufferOperations {
    fn get_width(&self) -> usize;
    fn get_height(&self) -> usize;
    fn get_color_code(&self) -> ColorCode;
    fn get_position(&self) -> (usize, usize);
    fn set_position(&mut self, row: usize, col: usize);
    fn set_char_at(&mut self, row: usize, col: usize, chr: ScreenChar);
    fn get_char_at(&self, row: usize, col: usize) -> ScreenChar;

    fn write_byte(&mut self, byte: u8) {
        let (row, col) = self.get_position();
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if col >= self.get_width() {
                    self.new_line();
                    let (new_row, _) = self.get_position();
                    self.set_char_at(
                        new_row,
                        0,
                        ScreenChar {
                            ascii_character: byte,
                            color_code: self.get_color_code(),
                        },
                    );
                    self.set_position(new_row, 1);
                } else {
                    self.set_char_at(
                        row,
                        col,
                        ScreenChar {
                            ascii_character: byte,
                            color_code: self.get_color_code(),
                        },
                    );
                    self.set_position(row, col + 1);
                }
            }
        }
    }

    fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        let (row, _) = self.get_position();
        self.set_position(row + 1, 0);
        if self.get_position().0 >= self.get_height() {
            self.scroll_up();
            self.set_position(self.get_height() - 1, 0);
        }
    }

    fn clear_row(&mut self, row: usize) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.get_color_code(),
        };
        crate::clear_line_range(self, row, row + 1, 0, self.get_width(), blank_char);
    }

    fn clear_screen(&mut self) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.get_color_code(),
        };
        crate::buffer_ops!(
            clear_buffer,
            self,
            self.get_height(),
            self.get_width(),
            blank_char
        );
        self.set_position(0, 0);
    }

    fn scroll_up(&mut self);
}

// Constants to reduce magic numbers and consolidate VGA implementation
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

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn reset(&mut self) {
        if self.enabled {
            self.clear_screen();
        } else {
            self.cursor_row = 0;
            self.cursor_col = 0;
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
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.get_color_code(),
        };
        let height = self.get_height();
        let width = self.get_width();
        if let Some(ref mut buffer) = self.get_buffer() {
            for row in 1..height {
                unsafe {
                    let src = buffer[row].as_ptr();
                    let dst = buffer[row - 1].as_mut_ptr();
                    core::ptr::copy_nonoverlapping(src, dst, width);
                }
            }
            buffer[height - 1].fill(blank_char);
        }
    }
}

// VGA text mode device implementation with consolidation
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
        self.buffer.set_color(foreground, background);
    }
}

impl crate::initializer::Initializable for VgaDevice {
    fn init(&mut self) -> crate::common::logging::SystemResult<()> {
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

impl crate::initializer::ErrorLogging for VgaDevice {
    fn log_error(&self, error: &crate::common::logging::SystemError, context: &'static str) {
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

impl crate::initializer::HardwareDevice for VgaDevice {
    fn device_name(&self) -> &'static str {
        "VGA Text Mode Display"
    }

    fn device_type(&self) -> &'static str {
        "Display"
    }

    fn enable(&mut self) -> crate::common::logging::SystemResult<()> {
        self.buffer.enable();
        log::info!("VGA device enabled");
        Ok(())
    }

    fn disable(&mut self) -> crate::common::logging::SystemResult<()> {
        self.buffer.disable();
        log::info!("VGA device disabled");
        Ok(())
    }

    fn reset(&mut self) -> crate::common::logging::SystemResult<()> {
        self.buffer.reset();
        log::info!("VGA device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.buffer.enabled
    }
}

// Re-export for backward compatibility and consolidation
pub use self::VgaBuffer as KernelVgaBuffer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::initializer::{ErrorLogging, HardwareDevice, Initializable};

    #[test]
    fn test_vga_device_creation() {
        let device = VgaDevice::new();
        assert_eq!(device.name(), "VgaDevice");
        assert!(!device.is_enabled());
    }

    #[test]
    fn test_vga_device_initializable_trait() {
        let mut device = VgaDevice::new();
        assert_eq!(device.name(), "VgaDevice");
        assert_eq!(device.priority(), 10);
        // Test init - should return Ok
        let result = device.init();
        assert!(result.is_ok());
    }

    #[test]
    fn test_vga_device_hardware_device_trait() {
        let mut device = VgaDevice::new();
        assert_eq!(device.device_name(), "VGA Text Mode Display");
        assert_eq!(device.device_type(), "Display");

        // Test enable/disable cycle
        assert!(!device.is_enabled());
        let enable_result = device.enable();
        assert!(enable_result.is_ok());
        assert!(device.is_enabled());

        let disable_result = device.disable();
        assert!(disable_result.is_ok());
        assert!(!device.is_enabled());

        // Test reset
        let reset_result = device.reset();
        assert!(reset_result.is_ok());
    }

    #[test]
    fn test_vga_device_error_logging_trait() {
        let device = VgaDevice::new();
        // Error logging methods don't return values, so just ensure they don't panic
        let error = crate::common::logging::SystemError::InternalError;
        device.log_error(&error, "test context");
        device.log_warning("test warning");
        device.log_info("test info");
        device.log_debug("test debug");
        device.log_trace("test trace");
    }

    #[test]
    fn test_vga_device_color_setting() {
        let mut device = VgaDevice::new();
        device.set_color(Color::Red, Color::Blue);

        // Can't directly test internal state, but method should not panic
        // and enable/disable/reset should still work
        assert!(!device.is_enabled());
        let _ = device.enable();
        assert!(device.is_enabled());
    }
}
