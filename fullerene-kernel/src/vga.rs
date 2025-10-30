use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations, clear_buffer, clear_line_range, graphics::{ports::{HardwarePorts, VgaPortOps}, registers::{
        ATTRIBUTE_TEXT_CONFIG as ATTRIBUTE_CONFIG, CRTC_TEXT_CONFIG as CRTC_CONFIG, GRAPHICS_TEXT_CONFIG as GRAPHICS_CONFIG, SEQUENCER_TEXT_CONFIG as SEQUENCER_CONFIG
    }}, handle_write_byte, impl_text_buffer_operations, port_read_u8, port_write, ports::write_vga_attribute_register, scroll_char_buffer_up, update_vga_cursor
};

// Consolidated port operations from petroleum crate
use spin::{Mutex, Once};
use alloc::vec::Vec;

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

const CURSOR_POS_LOW_REG: u8 = 0x0F;
const CURSOR_POS_HIGH_REG: u8 = 0x0E;

/// Representsthe VGA text buffer writer.
pub struct VgaBuffer {
    buffer: &'static mut [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
    column_position: usize,
    row_position: usize,
    color_code: ColorCode,
}

impl VgaBuffer {
    /// Creates a new VgaBuffer instance with the given VGA address.
    pub fn new(vga_address: usize) -> VgaBuffer {
        VgaBuffer {
            buffer: unsafe { &mut *(vga_address as *mut _) },
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
        update_vga_cursor!(pos);
    }
}

impl core::fmt::Write for VgaBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

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

    fn write_byte(&mut self, byte: u8) {
        handle_write_byte!(self, byte, { self.new_line() }, {
            if self.column_position >= BUFFER_WIDTH {
                self.new_line();
            }
            if self.row_position >= BUFFER_HEIGHT {
                self.scroll_up();
                self.row_position = BUFFER_HEIGHT - 1;
            }
            let screen_char = ScreenChar {
                ascii_character: byte,
                color_code: self.color_code,
            };
            self.buffer[self.row_position][self.column_position] = screen_char;
            self.column_position += 1;
        });
    }

    fn new_line(&mut self) {
        self.column_position = 0;
        if self.row_position < BUFFER_HEIGHT - 1 {
            self.row_position += 1;
        } else {
            self.scroll_up();
        }
    }

    fn clear_row(&mut self, row: usize) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        clear_line_range!(self, row, row + 1, 0, self.get_width(), blank_char);
    }

    fn clear_screen(&mut self) {
        self.row_position = 0;
        self.column_position = 0;
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: ColorCode(0),
        };
        clear_buffer!(self, self.get_height(), self.get_width(), blank_char);
    }

    fn scroll_up(&mut self) {
        scroll_char_buffer_up!(self.buffer, BUFFER_HEIGHT, BUFFER_WIDTH, ScreenChar { ascii_character: b' ', color_code: self.color_code });
    }
}

// Global singleton
pub static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

// Initialize the VGA screen with the given physical memory offset
pub fn init_vga(physical_memory_offset: x86_64::VirtAddr) {
    const VGA_PHY_ADDR: usize = 0xb8000;
    let vga_virt_addr = physical_memory_offset.as_u64() as usize + VGA_PHY_ADDR;
    VGA_BUFFER.call_once(|| Mutex::new(VgaBuffer::new(vga_virt_addr)));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen();
    writer.set_color_code(ColorCode::new(Color::Green, Color::Black));

    // Set VGA to text mode 3 (80x25 color text)
    use x86_64::instructions::port::*;

    // Write misc register
    port_write!(HardwarePorts::MISC_OUTPUT, 0x67u8);

    // Sequencer registers
    let sequencer_configs = SEQUENCER_CONFIG;
    let mut sequencer_ops = VgaPortOps::new(HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA);
    sequencer_ops.write_sequence(sequencer_configs);

    // CRTC registers for 80x25 text mode
    let crtc_configs = CRTC_CONFIG;
    let mut crtc_ops = VgaPortOps::new(HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA);
    crtc_ops.write_sequence(crtc_configs);

    // Graphics controller
    let graphics_configs = GRAPHICS_CONFIG;
    let mut graphics_ops = VgaPortOps::new(HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA);
    graphics_ops.write_sequence(graphics_configs);

    // Attribute controller (text mode)
    // Set palette registers (0-15) and other registers
    for i in 0..16 {
        write_vga_attribute_register(i, i as u8);
    }
    write_vga_attribute_register(0x10, 0x0C);
    write_vga_attribute_register(0x11, 0x00);
    write_vga_attribute_register(0x12, 0x0F);
    write_vga_attribute_register(0x13, 0x08);
    write_vga_attribute_register(0x14, 0x00);

    // Finally, enable video by writing 0x20 to the index port (with palette access enabled)
    port_read_u8!(HardwarePorts::STATUS);
    port_write!(0x3C0u16, 0x20u8);

    // DAC registers (optional for text mode, simplified)
    // Skip DAC initialization for now as it's not strictly necessary for text mode

    writer.write_string("Hello QEMU by FullereneOS!\n");
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
    // Force display refresh by reading status register
    let _: u8 = port_read_u8!(HardwarePorts::STATUS);
}

#[cfg(test)]
mod tests {
    use super::{BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, ScreenChar, TextBufferOperations};
    use alloc::vec;
    use alloc::vec::Vec;

    struct MockVgaBuffer {
        buffer: Vec<ScreenChar>,
        column_position: usize,
        row_position: usize,
        color_code: ColorCode,
        height: usize,
        width: usize,
    }

    impl TextBufferOperations for MockVgaBuffer {
        fn get_width(&self) -> usize {
            self.width
        }

        fn get_height(&self) -> usize {
            self.height
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
            if row < self.height && col < self.width {
                let index = row * self.width + col;
                self.buffer[index] = chr;
            }
        }

        fn get_char_at(&self, row: usize, col: usize) -> ScreenChar {
            if row < self.height && col < self.width {
                let index = row * self.width + col;
                self.buffer[index]
            } else {
                ScreenChar {
                    ascii_character: 0,
                    color_code: self.color_code,
                }
            }
        }
    }

    impl MockVgaBuffer {
        fn new(width: usize, height: usize) -> Self {
            MockVgaBuffer {
                buffer: vec![
                    ScreenChar {
                        ascii_character: b' ',
                        color_code: ColorCode::new(Color::White, Color::Black),
                    };
                    width * height
                ],
                column_position: 0,
                row_position: 0,
                color_code: ColorCode::new(Color::White, Color::Black),
                height,
                width,
            }
        }

        fn get_char(&self, row: usize, col: usize) -> Option<ScreenChar> {
            if row < self.height && col < self.width {
                let index = row * self.width + col;
                Some(self.buffer[index])
            } else {
                None
            }
        }
    }

    #[test]
    fn test_color_code_new() {
        let color_code = ColorCode::new(Color::Red, Color::Blue);
        assert_eq!(color_code.0, (Color::Blue as u8) << 4 | (Color::Red as u8));
    }

    #[test]
    fn test_write_byte() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.write_byte(b'H');
        mock_buffer.write_byte(b'i');
        let char_h = mock_buffer.get_char(0, 0).unwrap();
        assert_eq!(char_h.ascii_character, b'H');
        assert_eq!(
            char_h.color_code.0,
            ColorCode::new(Color::White, Color::Black).0
        );
        let char_i = mock_buffer.get_char(0, 1).unwrap();
        assert_eq!(char_i.ascii_character, b'i');
    }

    #[test]
    fn test_write_byte_newline_at_end_of_line() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        for _ in 0..BUFFER_WIDTH {
            mock_buffer.write_byte(b'A');
        }
        mock_buffer.write_byte(b'B');
        assert_eq!(mock_buffer.row_position, 1);
        assert_eq!(mock_buffer.column_position, 1);
        assert_eq!(
            mock_buffer
                .get_char(0, BUFFER_WIDTH - 1)
                .unwrap()
                .ascii_character,
            b'A'
        );
        assert_eq!(mock_buffer.get_char(1, 0).unwrap().ascii_character, b'B');
    }

    #[test]
    fn test_new_line() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.column_position = BUFFER_WIDTH - 1;
        mock_buffer.write_byte(b'A');
        mock_buffer.write_byte(b'\n');
        let char_a = mock_buffer.get_char(0, BUFFER_WIDTH - 1).unwrap();
        assert_eq!(char_a.ascii_character, b'A');
        assert_eq!(mock_buffer.row_position, 1);
        assert_eq!(mock_buffer.column_position, 0);
    }

    #[test]
    fn test_scrolling() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        for r in 0..BUFFER_HEIGHT {
            for c in 0..BUFFER_WIDTH {
                mock_buffer.write_byte(b'A');
            }
        }
        mock_buffer.write_byte(b'B');
        assert_eq!(mock_buffer.get_char(0, 0).unwrap().ascii_character, b'A');
        assert_eq!(
            mock_buffer
                .get_char(BUFFER_HEIGHT - 1, 0)
                .unwrap()
                .ascii_character,
            b'B'
        );
        for c in 1..BUFFER_WIDTH {
            assert_eq!(
                mock_buffer
                    .get_char(BUFFER_HEIGHT - 1, c)
                    .unwrap()
                    .ascii_character,
                b' '
            );
        }
    }

    #[test]
    fn test_clear_row() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.write_string("Line 1\nLine 2");
        mock_buffer.clear_row(0);
        let char_at_0_0 = mock_buffer.get_char(0, 0).unwrap();
        assert_eq!(char_at_0_0.ascii_character, b' ');
        let char_at_1_0 = mock_buffer.get_char(1, 0).unwrap();
        assert_eq!(char_at_1_0.ascii_character, b'L');
    }

    #[test]
    fn test_clear_screen() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.write_string("Some text");
        mock_buffer.row_position = 5;
        mock_buffer.column_position = 10;
        mock_buffer.clear_screen();
        assert_eq!(mock_buffer.row_position, 0);
        assert_eq!(mock_buffer.column_position, 0);
        for r in 0..BUFFER_HEIGHT {
            for c in 0..BUFFER_WIDTH {
                let char = mock_buffer.get_char(r, c).unwrap();
                assert_eq!(char.ascii_character, b' ');
            }
        }
    }

    #[test]
    fn test_write_string() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.write_string("Hello\nWorld");
        let char_h = mock_buffer.get_char(0, 0).unwrap();
        assert_eq!(char_h.ascii_character, b'H');
        let char_w = mock_buffer.get_char(1, 0).unwrap();
        assert_eq!(char_w.ascii_character, b'W');
    }

    #[test]
    fn test_write_string_non_ascii() {
        let mut mock_buffer = MockVgaBuffer::new(BUFFER_WIDTH, BUFFER_HEIGHT);
        mock_buffer.write_string("Hello€");
        let char_h = mock_buffer.get_char(0, 0).unwrap();
        assert_eq!(char_h.ascii_character, b'H');
        let char_euro = mock_buffer.get_char(0, 5).unwrap();
        assert_eq!(char_euro.ascii_character, 0xfe);
    }
}
