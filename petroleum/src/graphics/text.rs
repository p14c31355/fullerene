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

use crate::{clear_buffer, scroll_buffer_up};

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
        clear_buffer!(self, 1, self.get_width(), blank_char);
    }

    fn clear_screen(&mut self) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.get_color_code(),
        };
        clear_buffer!(self, self.get_height(), self.get_width(), blank_char);
        self.set_position(0, 0);
    }

    fn scroll_up(&mut self) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.get_color_code(),
        };
        scroll_buffer_up!(self, self.get_height(), self.get_width(), blank_char);
        self.clear_row(self.get_height() - 1);
    }
}
