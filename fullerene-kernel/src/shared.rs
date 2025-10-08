// Shared helper macros and traits for reducing code duplication

use petroleum::{Color, ColorCode, ScreenChar};

/// Macro to implement common text buffer operations
macro_rules! impl_text_buffer_operations {
    ($impl_target:ty) => {
        impl $impl_target {
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
                for col in 0..self.get_width() {
                    self.set_char_at(row, col, blank_char);
                }
            }
        }
    };
}

pub(crate) use impl_text_buffer_operations;
