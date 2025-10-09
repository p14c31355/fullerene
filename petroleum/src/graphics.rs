use core::fmt::{self, Write};
use core::marker::{Send, Sync};
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

/// Generic port writer struct to reduce unsafe block repetition and improve type safety
pub struct PortWriter<T> {
    port: Port<T>,
}

impl<T> PortWriter<T> {
    pub fn new(port_addr: u16) -> Self {
        Self {
            port: Port::new(port_addr),
        }
    }

    pub unsafe fn write(&mut self, value: T)
    where
        T: x86_64::instructions::port::PortWrite,
    {
        unsafe {
            self.port.write(value);
        }
    }

    pub unsafe fn read(&mut self) -> T
    where
        T: x86_64::instructions::port::PortRead,
    {
        unsafe { self.port.read() }
    }
}

// Specialized VGA port operations
pub struct VgaPortOps {
    index_writer: PortWriter<u8>,
    data_writer: PortWriter<u8>,
}

impl VgaPortOps {
    pub fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_writer: PortWriter::new(index_port),
            data_writer: PortWriter::new(data_port),
        }
    }

    pub fn write_register(&mut self, index: u8, value: u8) {
        unsafe {
            self.index_writer.write(index);
            self.data_writer.write(value);
        }
    }

    pub fn write_sequence(&mut self, configs: &[RegisterConfig]) {
        for reg in configs {
            self.write_register(reg.index, reg.value);
        }
    }
}

// Enhanced macro for writing port sequences with automatic port management
#[macro_export]
macro_rules! write_port_sequence {
    ($($config:expr, $index_port:expr, $data_port:expr);*$(;)?) => {{
        $(
            let mut vga_ports = VgaPortOps::new($index_port, $data_port);
            vga_ports.write_sequence($config);
        )*
    }};
}

// Simplified macro for single register writes
// VGA register configurations using structs for data-driven setup
#[derive(Debug, Clone, Copy)]
pub struct RegisterConfig {
    pub index: u8,
    pub value: u8,
}

// VGA Sequencer registers configuration for mode 13h (320x200, 256 colors)
// These control the timing and memory access for the VGA sequencer
pub const SEQUENCER_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x03,
    }, // Reset register - synchronous reset
    RegisterConfig {
        index: 0x01,
        value: 0x01,
    }, // Clocking mode register - 8/9 dot clocks, use 9th bit in text mode (not applicable in graphics mode)
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Map mask register - enable all planes (15)
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Character map select register - select character sets (not used in graphics mode)
    RegisterConfig {
        index: 0x04,
        value: 0x0E,
    }, // Memory mode register - enable ext memory, enable odd/even in chain 4, use chain 4 addressing mode
];

// VGA CRTC (Cathode Ray Tube Controller) registers configuration for mode 13h
// These control horizontal/vertical synchronization, display size, and timing
pub const CRTC_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x5F,
    }, // Horizontal total (horizontal display end)
    RegisterConfig {
        index: 0x01,
        value: 0x4F,
    }, // Horizontal display enable end
    RegisterConfig {
        index: 0x02,
        value: 0x50,
    }, // Start horizontal blanking
    RegisterConfig {
        index: 0x03,
        value: 0x82,
    }, // End horizontal blanking
    RegisterConfig {
        index: 0x04,
        value: 0x54,
    }, // Start horizontal retrace pulse
    RegisterConfig {
        index: 0x05,
        value: 0x80,
    }, // End horizontal retrace
    RegisterConfig {
        index: 0x06,
        value: 0xBF,
    }, // Vertical total (vertical display end)
    RegisterConfig {
        index: 0x07,
        value: 0x1F,
    }, // Overflow
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Preset row scan
    RegisterConfig {
        index: 0x09,
        value: 0x41,
    }, // Maximum scan line
    RegisterConfig {
        index: 0x10,
        value: 0x9C,
    }, // Start vertical retrace
    RegisterConfig {
        index: 0x11,
        value: 0x8E,
    }, // End vertical retrace
    RegisterConfig {
        index: 0x12,
        value: 0x8F,
    }, // Vertical display enable end
    RegisterConfig {
        index: 0x13,
        value: 0x28,
    }, // Offset (line offset/logical width - number of bytes per scan line)
    RegisterConfig {
        index: 0x14,
        value: 0x40,
    }, // Underline location
    RegisterConfig {
        index: 0x15,
        value: 0x96,
    }, // Start vertical blanking
    RegisterConfig {
        index: 0x16,
        value: 0xB9,
    }, // End vertical blanking
    RegisterConfig {
        index: 0x17,
        value: 0xA3,
    }, // CRTC mode control
];

// VGA Graphics Controller registers configuration for mode 13h
// These control how graphics memory is mapped and accessed
pub const GRAPHICS_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Set/reset register - reset all bits
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Enable set/reset register - disable
    RegisterConfig {
        index: 0x02,
        value: 0x00,
    }, // Color compare register - compare mode
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Data rotate register - no rotate
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Read plane select register - select plane 0
    RegisterConfig {
        index: 0x05,
        value: 0x40,
    }, // Graphics mode register - chain odd/even planes, read mode 0, write mode 0, read plane 0
    RegisterConfig {
        index: 0x06,
        value: 0x05,
    }, // Miscellaneous register - memory map mode A0000-AFFFF (64KB), alphanumerics/text mode disabled, chain odd/even planes disabled
    RegisterConfig {
        index: 0x07,
        value: 0x0F,
    }, // Color don't care register - care about all bits
    RegisterConfig {
        index: 0x08,
        value: 0xFF,
    }, // Bit mask register - enable all bits
];

// VGA port addresses
pub struct VgaPorts;

impl VgaPorts {
    pub const MISC_OUTPUT: u16 = 0x3C2;
    pub const CRTC_INDEX: u16 = 0x3D4;
    pub const CRTC_DATA: u16 = 0x3D5;
    pub const STATUS: u16 = 0x3DA;
    pub const ATTRIBUTE_INDEX: u16 = 0x3C0;
    pub const DAC_INDEX: u16 = 0x3C8;
    pub const DAC_DATA: u16 = 0x3C9;
    pub const GRAPHICS_INDEX: u16 = 0x3CE;
    pub const GRAPHICS_DATA: u16 = 0x3CF;
    pub const SEQUENCER_INDEX: u16 = 0x3C4;
    pub const SEQUENCER_DATA: u16 = 0x3C5;
}

// VGA Attribute Controller registers configuration for mode 13h
// These control color mapping and screen display attributes
pub const ATTRIBUTE_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Palette register 0 (red|green|blue|intensity)
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Palette register 1
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Palette register 2
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Palette register 3
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Palette register 4
    RegisterConfig {
        index: 0x05,
        value: 0x00,
    }, // Palette register 5
    RegisterConfig {
        index: 0x06,
        value: 0x00,
    }, // Palette register 6
    RegisterConfig {
        index: 0x07,
        value: 0x00,
    }, // Palette register 7
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Palette register 8
    RegisterConfig {
        index: 0x09,
        value: 0x00,
    }, // Palette register 9
    RegisterConfig {
        index: 0x0A,
        value: 0x00,
    }, // Palette register A
    RegisterConfig {
        index: 0x0B,
        value: 0x00,
    }, // Palette register B
    RegisterConfig {
        index: 0x0C,
        value: 0x00,
    }, // Palette register C
    RegisterConfig {
        index: 0x0D,
        value: 0x00,
    }, // Palette register D
    RegisterConfig {
        index: 0x0E,
        value: 0x00,
    }, // Palette register E
    RegisterConfig {
        index: 0x0F,
        value: 0x00,
    }, // Palette register F
    RegisterConfig {
        index: 0x10,
        value: 0x41,
    }, // Attr mode control register - enable 256-color mode, enable graphics mode
    RegisterConfig {
        index: 0x11,
        value: 0x00,
    }, // Overscan color register - border color (black)
    RegisterConfig {
        index: 0x12,
        value: 0x0F,
    }, // Color plane enable register - enable all planes
    RegisterConfig {
        index: 0x13,
        value: 0x00,
    }, // Horizontal pixel panning register - no panning
    RegisterConfig {
        index: 0x14,
        value: 0x00,
    }, // Color select register
];

// Helper function to write a palette value in grayscale
pub fn write_palette_grayscale(val: u8) {
    unsafe {
        let mut dac_data_port: Port<u8> = Port::new(VgaPorts::DAC_DATA);
        for _ in 0..3 {
            // RGB
            dac_data_port.write(val);
        }
    }
}

/// Configures the Miscellaneous Output Register.
pub fn setup_misc_output() {
    unsafe {
        let mut misc_output_port = Port::new(VgaPorts::MISC_OUTPUT);
        misc_output_port.write(0x63u8); // Value for enabling VGA in 320x200x256 mode
    }
}

/// Configures the VGA registers using the new macro
pub fn setup_registers_from_configs() {
    write_port_sequence!(
        SEQUENCER_CONFIG, VgaPorts::SEQUENCER_INDEX, VgaPorts::SEQUENCER_DATA;
        CRTC_CONFIG, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA;
        GRAPHICS_CONFIG, VgaPorts::GRAPHICS_INDEX, VgaPorts::GRAPHICS_DATA
    );
}

// VGA Attribute Controller registers configuration for mode 13h
// These control color mapping and screen display attributes

/// Helper function to write to attribute registers with special sequence
pub fn write_attribute_registers() {
    unsafe {
        let mut status_port = Port::<u8>::new(VgaPorts::STATUS);
        let mut index_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);
        let mut data_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);

        let _ = status_port.read(); // Reset flip-flop

        for reg in ATTRIBUTE_CONFIG {
            index_port.write(reg.index);
            data_port.write(reg.value);
        }

        index_port.write(0x20); // Enable video output
    }
}

/// Configures the VGA Attribute Controller registers.
pub fn setup_attribute_controller() {
    write_attribute_registers();
}

/// Sets up a simple grayscale palette for the 256-color mode.
pub fn setup_palette() {
    unsafe {
        let mut dac_index_port = Port::<u8>::new(VgaPorts::DAC_INDEX);
        dac_index_port.write(0x00); // Start at color index 0
    }

    for i in 0..256 {
        let val = (i * 63 / 255) as u8;
        write_palette_grayscale(val);
    }
}

// Initializes VGA graphics mode 13h (320x200, 256 colors).
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga_graphics() {
    setup_misc_output();
    setup_registers_from_configs(); // Consolidated setup for sequencer, crtc, graphics
    setup_attribute_controller();
    setup_palette();
}

#[macro_export]
macro_rules! write_vga_register {
    ($index_port:expr, $data_port:expr, $index:expr, $data:expr) => {{
        let mut vga_ports = VgaPortOps::new($index_port, $data_port);
        vga_ports.write_register($index, $data);
    }};
}

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
        for col in 0..self.get_width() {
            self.set_char_at(row, col, blank_char);
        }
    }

    fn clear_screen(&mut self) {
        for row in 0..self.get_height() {
            self.clear_row(row);
        }
        self.set_position(0, 0);
    }

    fn scroll_up(&mut self) {
        for row in 1..self.get_height() {
            for col in 0..self.get_width() {
                let src = self.get_char_at(row, col);
                self.set_char_at(row - 1, col, src);
            }
        }
        self.clear_row(self.get_height() - 1);
    }
}
