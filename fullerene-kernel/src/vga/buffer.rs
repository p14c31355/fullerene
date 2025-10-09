// Import necessary types and constants from parent module
use super::{BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, ScreenChar, TextBufferOperations};
use petroleum::graphics::ports::VgaPorts;
use x86_64::instructions::port::Port;

const CURSOR_POS_LOW_REG: u8 = 0x0F;
const CURSOR_POS_HIGH_REG: u8 = 0x0E;

/// Represents the VGA text buffer writer.
pub struct VgaBuffer {
    buffer: &'static mut [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
    column_position: usize,
    row_position: usize,
    color_code: ColorCode,
}

impl VgaBuffer {
    /// Creates a new VgaBuffer instance.
    pub fn new() -> VgaBuffer {
        VgaBuffer {
            buffer: unsafe { &mut *(0xb8000 as *mut _) },
            column_position: 0,
            row_position: 0,
            color_code: ColorCode::new(Color::Green, Color::Black),
        }
    }

    /// Sets the color code for text output.
    pub fn set_color_code(&mut self, color_code: ColorCode) {
        self.color_code = color_code;
    }

    /// Updates the hardware cursor position.
    pub fn update_cursor(&self) {
        let pos = self.row_position * BUFFER_WIDTH + self.column_position;
        unsafe {
            let mut command_port = Port::new(VgaPorts::CRTC_INDEX);
            let mut data_port = Port::new(VgaPorts::CRTC_DATA);

            command_port.write(CURSOR_POS_LOW_REG);
            data_port.write((pos & 0xFF) as u8);
            command_port.write(CURSOR_POS_HIGH_REG);
            data_port.write(((pos >> 8) & 0xFF) as u8);
        }
    }
}

impl core::fmt::Write for VgaBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// To ensure thread-safety for `spin` crates.
unsafe impl Send for VgaBuffer {}
unsafe impl Sync for VgaBuffer {}

impl TextBufferOperations for VgaBuffer {
    fn get_width(&self) -> usize {
        BUFFER_WIDTH
    }

    fn get_height(&self) -> usize {
        BUFFER_HEIGHT
    }

    fn get_color_code(&self) -> ColorCode {
        self.color_code
    }

    fn get_position(&self) -> (usize, usize) {
        (self.row_position, self.column_position)
    }

    fn set_position(&mut self, row: usize, col: usize) {
        self.row_position = row;
        self.column_position = col;
    }

    fn set_char_at(&mut self, row: usize, col: usize, chr: ScreenChar) {
        if row < BUFFER_HEIGHT && col < BUFFER_WIDTH {
            self.buffer[row][col] = chr;
        }
    }

    fn get_char_at(&self, row: usize, col: usize) -> ScreenChar {
        if row < BUFFER_HEIGHT && col < BUFFER_WIDTH {
            self.buffer[row][col]
        } else {
            ScreenChar {
                ascii_character: 0,
                color_code: self.color_code,
            }
        }
    }

    fn scroll_up(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                self.buffer[row - 1][col] = self.buffer[row][col];
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
    }
}
