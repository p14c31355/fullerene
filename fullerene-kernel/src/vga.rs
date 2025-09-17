// fullerene-kernel/src/vga.rs

use core::fmt::Write;
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
#[allow(dead_code)]
enum Color {
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
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

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
            color_code: ColorCode::new(Color::White, Color::Black),
        }
    }

    /// Writes a single byte to the buffer.
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = self.row_position;
                let col = self.column_position;

                self.buffer[row][col] = ScreenChar {
                    ascii_character: byte,
                    color_code: self.color_code,
                };
                self.column_position += 1;
            }
        }
    }

    /// Writes a string to the buffer.
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Printable ASCII byte or newline
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // Not part of printable ASCII range, display a solid block
                _ => self.write_byte(0xfe),
            }
        }
    }

    /// Moves the cursor to the next line.
    fn new_line(&mut self) {
        self.column_position = 0;
        if self.row_position < BUFFER_HEIGHT - 1 {
            self.row_position += 1;
        } else {
            // Shift all lines up
            for row in 1..BUFFER_HEIGHT {
                for col in 0..BUFFER_WIDTH {
                    self.buffer[row - 1][col] = self.buffer[row][col];
                }
            }
            // Clear the last line
            self.clear_row(BUFFER_HEIGHT - 1);
        }
    }

    /// Clears a specific row in the buffer.
    fn clear_row(&mut self, row: usize) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer[row][col] = blank_char;
        }
    }

    /// Clears the entire screen and resets the cursor position.
    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.row_position = 0;
        self.update_cursor();
    }

    /// Updates the hardware cursor position.
    fn update_cursor(&self) {
        let pos = self.row_position * BUFFER_WIDTH + self.column_position;
        unsafe {
            let mut command_port = Port::new(0x3D4);
            let mut data_port = Port::new(0x3D5);

            command_port.write(0x0F_u8);
            data_port.write((pos & 0xFF) as u8);
            command_port.write(0x0E_u8);
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

// Global singleton for the VGA buffer writer
static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

/// Logs a message to the VGA screen.
pub fn log(msg: &str) {
    if let Some(vga) = VGA_BUFFER.get() {
        let mut writer = vga.lock();
        writer.write_string(msg);
        writer.write_string("\n");
        writer.update_cursor();
    }
}

pub fn panic_log(info: &core::panic::PanicInfo) {
    if let Some(vga) = VGA_BUFFER.get() {
        let mut writer = vga.lock();
        let _ = writer.write_str("KERNEL PANIC!\n");
        if let Some(location) = info.location() {
            let _ = write!(
                writer,
                "  at {}:{}:{}
",
                location.file(),
                location.line(),
                location.column()
            );
        }
        let msg = info.message();
        let _ = write!(writer, "  {}\n", msg);
        writer.update_cursor();
    }
}

/// Initializes the VGA screen.
pub fn vga_init() {
    VGA_BUFFER.call_once(|| Mutex::new(VgaBuffer::new()));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen(); // Clear screen on boot
    writer.color_code = ColorCode::new(Color::LightGreen, Color::Black);
    writer.write_string("Hello QEMU by fullerene!\n");
    writer.color_code = ColorCode::new(Color::White, Color::Black);
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
}
