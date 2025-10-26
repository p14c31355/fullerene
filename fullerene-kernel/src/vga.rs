use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations, handle_write_byte, port_write,
    update_vga_cursor,
};
use spin::{Mutex, Once};

// VGA port constants
const VGA_MISC_WRITE: u16 = 0x3C2;
const VGA_SEQ_INDEX: u16 = 0x3C4;
const VGA_SEQ_DATA: u16 = 0x3C5;
const VGA_CRTC_INDEX: u16 = 0x3D4;
const VGA_CRTC_DATA: u16 = 0x3D5;
const VGA_GC_INDEX: u16 = 0x3CE;
const VGA_GC_DATA: u16 = 0x3CF;
const VGA_AC_INDEX: u16 = 0x3C0;
const VGA_AC_WRITE: u16 = 0x3C1;

// VGA register values for 80x25 text mode
const MISC_REGISTER_VALUE: u8 = 0x67;
const SEQUENCER_RESET: u8 = 0x00;
const SEQUENCER_RESET_VALUE: u8 = 0x03;
const SEQUENCER_CLOCKING_MODE: u8 = 0x01;
const SEQUENCER_CLOCKING_MODE_VALUE: u8 = 0x00;
const SEQUENCER_PLANE_MASK: u8 = 0x02;
const SEQUENCER_PLANE_MASK_VALUE: u8 = 0x03;
const SEQUENCER_CHARACTER_MAP: u8 = 0x03;
const SEQUENCER_CHARACTER_MAP_VALUE: u8 = 0x00;
const SEQUENCER_MEMORY_MODE: u8 = 0x04;
const SEQUENCER_MEMORY_MODE_VALUE: u8 = 0x02;
const CRTC_HORIZ_DISPLAY_END: u8 = 0x00;
const CRTC_HORIZ_DISPLAY_END_VALUE: u8 = 0x5f;
const CRTC_HORIZ_DISPLAY_ENABLE_END: u8 = 0x01;
const CRTC_HORIZ_DISPLAY_ENABLE_END_VALUE: u8 = 0x4f;
const CRTC_HORIZ_RETRACE_START: u8 = 0x02;
const CRTC_HORIZ_RETRACE_START_VALUE: u8 = 0x50;
const CRTC_HORIZ_RETRACE_END: u8 = 0x03;
const CRTC_HORIZ_RETRACE_END_VALUE: u8 = 0x82;
const CRTC_HORIZ_RETRACE_END2: u8 = 0x04;
const CRTC_HORIZ_RETRACE_END2_VALUE: u8 = 0x55;
const CRTC_HORIZ_BLANK_START: u8 = 0x05;
const CRTC_HORIZ_BLANK_START_VALUE: u8 = 0x81;
const CRTC_HORIZ_BLANK_END: u8 = 0x06;
const CRTC_HORIZ_BLANK_END_VALUE: u8 = 0xbf;
const CRTC_VERTICAL_SYNC_START: u8 = 0x07;
const CRTC_VERTICAL_SYNC_START_VALUE: u8 = 0x1f;
const CRTC_VERTICAL_SYNC_END: u8 = 0x08;
const CRTC_VERTICAL_SYNC_END_VALUE: u8 = 0x00;
const CRTC_VERTICAL_SYNC_END_LOW: u8 = 0x09;
const CRTC_VERTICAL_SYNC_END_LOW_VALUE: u8 = 0x4f;
const CRTC_VERTICAL_SYNC_START2: u8 = 0x10;
const CRTC_VERTICAL_SYNC_START2_VALUE: u8 = 0x9c;
const CRTC_VERTICAL_SYNC_END2: u8 = 0x11;
const CRTC_VERTICAL_SYNC_END2_VALUE: u8 = 0x8e;
const CRTC_VERTICAL_DISPLAY_ENABLE_END: u8 = 0x12;
const CRTC_VERTICAL_DISPLAY_ENABLE_END_VALUE: u8 = 0x8f;
const CRTC_OFFSET: u8 = 0x13;
const CRTC_OFFSET_VALUE: u8 = 0x28;
const CRTC_UNDERLINE_LOCATION: u8 = 0x14;
const CRTC_UNDERLINE_LOCATION_VALUE: u8 = 0x1f;
const CRTC_VERTICAL_BLANK_START: u8 = 0x15;
const CRTC_VERTICAL_BLANK_START_VALUE: u8 = 0x96;
const CRTC_VERTICAL_BLANK_END: u8 = 0x16;
const CRTC_VERTICAL_BLANK_END_VALUE: u8 = 0xb9;
const CRTC_MODE_CONTROL: u8 = 0x17;
const CRTC_MODE_CONTROL_VALUE: u8 = 0xa3;
const GRAPHICS_SET_RESET: u8 = 0x00;
const GRAPHICS_SET_RESET_VALUE: u8 = 0x00;
const GRAPHICS_ENABLE_SET_RESET: u8 = 0x01;
const GRAPHICS_ENABLE_SET_RESET_VALUE: u8 = 0x00;
const GRAPHICS_COLOR_COMPARE: u8 = 0x02;
const GRAPHICS_COLOR_COMPARE_VALUE: u8 = 0x00;
const GRAPHICS_DATA_ROTATE: u8 = 0x03;
const GRAPHICS_DATA_ROTATE_VALUE: u8 = 0x00;
const GRAPHICS_READ_MAP_SELECT: u8 = 0x04;
const GRAPHICS_READ_MAP_SELECT_VALUE: u8 = 0x00;
const GRAPHICS_GRAPHICS_MODE: u8 = 0x05;
const GRAPHICS_GRAPHICS_MODE_VALUE: u8 = 0x10;
const GRAPHICS_MEMORY_MAP_SELECT: u8 = 0x06;
const GRAPHICS_MEMORY_MAP_SELECT_VALUE: u8 = 0x0e;
const GRAPHICS_COLOR_DONT_CARE: u8 = 0x07;
const GRAPHICS_COLOR_DONT_CARE_VALUE: u8 = 0x00;
const GRAPHICS_BIT_MASK: u8 = 0x08;
const GRAPHICS_BIT_MASK_VALUE: u8 = 0xff;
const ATTRIBUTE_MODE_CONTROL: u8 = 0x10;
const ATTRIBUTE_MODE_CONTROL_VALUE: u8 = 0x0c;
const ATTRIBUTE_OVERSCAN_COLOR: u8 = 0x11;
const ATTRIBUTE_OVERSCAN_COLOR_VALUE: u8 = 0x00;
const ATTRIBUTE_COLOR_PLANE_ENABLE: u8 = 0x12;
const ATTRIBUTE_COLOR_PLANE_ENABLE_VALUE: u8 = 0x0f;
const ATTRIBUTE_PIXEL_PANNING: u8 = 0x13;
const ATTRIBUTE_PIXEL_PANNING_VALUE: u8 = 0x08;
const ATTRIBUTE_COLOR_SELECT: u8 = 0x14;
const ATTRIBUTE_COLOR_SELECT_VALUE: u8 = 0x00;

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

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
    use petroleum::graphics::ports::{HardwarePorts, RegisterConfig, VgaPortOps};
    use petroleum::hardware::ports::*;
    use petroleum::port_write;
    use x86_64::instructions::port::*;

    // Write misc register
    port_write!(VGA_MISC_WRITE, MISC_REGISTER_VALUE);

    // Sequencer registers
    let sequencer_configs = [
        RegisterConfig { index: SEQUENCER_RESET, value: SEQUENCER_RESET_VALUE },
        RegisterConfig { index: SEQUENCER_CLOCKING_MODE, value: SEQUENCER_CLOCKING_MODE_VALUE },
        RegisterConfig { index: SEQUENCER_PLANE_MASK, value: SEQUENCER_PLANE_MASK_VALUE },
        RegisterConfig { index: SEQUENCER_CHARACTER_MAP, value: SEQUENCER_CHARACTER_MAP_VALUE },
        RegisterConfig { index: SEQUENCER_MEMORY_MODE, value: SEQUENCER_MEMORY_MODE_VALUE },
    ];
    let mut sequencer_ops = VgaPortOps::new(HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA);
    sequencer_ops.write_sequence(&sequencer_configs);

    // CRTC registers for 80x25 text mode
    let crtc_configs = [
        RegisterConfig { index: CRTC_HORIZ_DISPLAY_END, value: CRTC_HORIZ_DISPLAY_END_VALUE },
        RegisterConfig { index: CRTC_HORIZ_DISPLAY_ENABLE_END, value: CRTC_HORIZ_DISPLAY_ENABLE_END_VALUE },
        RegisterConfig { index: CRTC_HORIZ_RETRACE_START, value: CRTC_HORIZ_RETRACE_START_VALUE },
        RegisterConfig { index: CRTC_HORIZ_RETRACE_END, value: CRTC_HORIZ_RETRACE_END_VALUE },
        RegisterConfig { index: CRTC_HORIZ_RETRACE_END2, value: CRTC_HORIZ_RETRACE_END2_VALUE },
        RegisterConfig { index: CRTC_HORIZ_BLANK_START, value: CRTC_HORIZ_BLANK_START_VALUE },
        RegisterConfig { index: CRTC_HORIZ_BLANK_END, value: CRTC_HORIZ_BLANK_END_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_SYNC_START, value: CRTC_VERTICAL_SYNC_START_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_SYNC_END, value: CRTC_VERTICAL_SYNC_END_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_SYNC_END_LOW, value: CRTC_VERTICAL_SYNC_END_LOW_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_SYNC_START2, value: CRTC_VERTICAL_SYNC_START2_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_SYNC_END2, value: CRTC_VERTICAL_SYNC_END2_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_DISPLAY_ENABLE_END, value: CRTC_VERTICAL_DISPLAY_ENABLE_END_VALUE },
        RegisterConfig { index: CRTC_OFFSET, value: CRTC_OFFSET_VALUE },
        RegisterConfig { index: CRTC_UNDERLINE_LOCATION, value: CRTC_UNDERLINE_LOCATION_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_BLANK_START, value: CRTC_VERTICAL_BLANK_START_VALUE },
        RegisterConfig { index: CRTC_VERTICAL_BLANK_END, value: CRTC_VERTICAL_BLANK_END_VALUE },
        RegisterConfig { index: CRTC_MODE_CONTROL, value: CRTC_MODE_CONTROL_VALUE },
    ];
    let mut crtc_ops = VgaPortOps::new(HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA);
    crtc_ops.write_sequence(&crtc_configs);

    // Enable screen
    let mut sequencer_ops_enable = VgaPortOps::new(HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA);
    sequencer_ops_enable.write_register(SEQUENCER_CLOCKING_MODE, SEQUENCER_CLOCKING_MODE_VALUE);

    // Graphics controller
    let graphics_configs = [
        RegisterConfig { index: GRAPHICS_SET_RESET, value: GRAPHICS_SET_RESET_VALUE },
        RegisterConfig { index: GRAPHICS_ENABLE_SET_RESET, value: GRAPHICS_ENABLE_SET_RESET_VALUE },
        RegisterConfig { index: GRAPHICS_COLOR_COMPARE, value: GRAPHICS_COLOR_COMPARE_VALUE },
        RegisterConfig { index: GRAPHICS_DATA_ROTATE, value: GRAPHICS_DATA_ROTATE_VALUE },
        RegisterConfig { index: GRAPHICS_READ_MAP_SELECT, value: GRAPHICS_READ_MAP_SELECT_VALUE },
        RegisterConfig { index: GRAPHICS_GRAPHICS_MODE, value: GRAPHICS_GRAPHICS_MODE_VALUE },
        RegisterConfig { index: GRAPHICS_MEMORY_MAP_SELECT, value: GRAPHICS_MEMORY_MAP_SELECT_VALUE },
        RegisterConfig { index: GRAPHICS_COLOR_DONT_CARE, value: GRAPHICS_COLOR_DONT_CARE_VALUE },
        RegisterConfig { index: GRAPHICS_BIT_MASK, value: GRAPHICS_BIT_MASK_VALUE },
    ];
    let mut graphics_ops = VgaPortOps::new(HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA);
    graphics_ops.write_sequence(&graphics_configs);

    // Attribute controller (text mode)
    let mut vga_input_status_1: PortReadOnly<u8> = PortReadOnly::new(0x3DA);
    let mut vga_ac_port: Port<u8> = Port::new(VGA_AC_INDEX); // 0x3C0 is used for both index and data

    let mut write_ac_reg = |index: u8, value: u8| {
        unsafe {
            vga_input_status_1.read(); // Reset flip-flop
            vga_ac_port.write(index);
            vga_ac_port.write(value);
        }
    };

    // Set palette registers (0-15) and other registers
    for i in 0..16 {
        write_ac_reg(i, i);
    }
    write_ac_reg(ATTRIBUTE_MODE_CONTROL, ATTRIBUTE_MODE_CONTROL_VALUE);
    write_ac_reg(ATTRIBUTE_OVERSCAN_COLOR, ATTRIBUTE_OVERSCAN_COLOR_VALUE);
    write_ac_reg(
        ATTRIBUTE_COLOR_PLANE_ENABLE,
        ATTRIBUTE_COLOR_PLANE_ENABLE_VALUE,
    );
    write_ac_reg(ATTRIBUTE_PIXEL_PANNING, ATTRIBUTE_PIXEL_PANNING_VALUE);
    write_ac_reg(ATTRIBUTE_COLOR_SELECT, ATTRIBUTE_COLOR_SELECT_VALUE);

    // Finally, enable video by writing 0x20 to the index port (with palette access enabled)
    unsafe {
        vga_input_status_1.read();
        vga_ac_port.write(0x20);
    }

    // DAC registers (optional for text mode, simplified)
    // Skip DAC initialization for now as it's not strictly necessary for text mode

    writer.write_string("Hello QEMU by FullereneOS!\n");
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
    // Force display refresh by reading status register
    let _: u8 = petroleum::port_read_u8!(petroleum::graphics::ports::HardwarePorts::STATUS);
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
