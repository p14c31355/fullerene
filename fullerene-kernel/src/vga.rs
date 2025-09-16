// fullerene/fullerene-kernel/src/vga.rs
use spin::Mutex;
use spin::once::Once;
use x86_64::instructions::port::Port;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
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

pub struct VgaBuffer {
    buffer: &'static mut [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
    column_position: usize,
    row_position: usize,
    color_code: ColorCode,
}

impl VgaBuffer {
    pub fn new() -> VgaBuffer {
        VgaBuffer {
            buffer: unsafe { &mut *(0xb8000 as *mut _) },
            column_position: 0,
            row_position: 0,
            color_code: ColorCode::new(Color::White, Color::Black),
        }
    }

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

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // printable ASCII byte or newline
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // not part of printable ASCII range
                _ => self.write_byte(0xfe),
            }
        }
    }

    pub fn new_line(&mut self) {
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

    fn clear_row(&mut self, row: usize) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer[row][col] = blank_char;
        }
    }

    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.row_position = 0;
    }
}

unsafe impl Send for VgaBuffer {}
unsafe impl Sync for VgaBuffer {}

// Replace SERIAL static with VGA_BUFFER static
static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

fn vga_hardware_init() {
    unsafe {
        // Set VGA text mode (80x25, 16 colors)
        // This is a simplified approach; a full VGA mode set is more complex.
        // For text mode, often the BIOS has already set it up.
        // Here, we primarily ensure the cursor is visible and configured.

        // Cursor Start Register (0x0A) - Set scanline start for cursor
        let mut cursor_start_port = Port::new(0x3D4);
        let mut cursor_end_port = Port::new(0x3D5);

        cursor_start_port.write(0x0A as u8); // Cursor Start Register
        cursor_end_port.write(0x0E as u8); // Cursor End Register (scanline 14, 15)

        // Cursor Location High Register (0x0E) and Low Register (0x0F)
        // Set cursor to (0,0)
        cursor_start_port.write(0x0E as u8); // Cursor Location High Register
        cursor_end_port.write(0x00 as u8); // High byte of cursor offset
        cursor_start_port.write(0x0F as u8); // Cursor Location Low Register
        cursor_end_port.write(0x00 as u8); // Low byte of cursor offset
    }
}

pub fn log(msg: &str) {
    
    if let Some(vga) = VGA_BUFFER.get() {
        let mut writer = vga.lock();
        writer.write_string(msg);
        writer.write_string("\n");
    }
}

pub fn vga_init() {
    vga_hardware_init(); // Initialize VGA hardware
    VGA_BUFFER.call_once(|| Mutex::new(VgaBuffer::new()));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen(); // Clear screen on boot
    writer.color_code = ColorCode::new(Color::LightGreen, Color::Black);
    writer.write_string("Hello QEMU by fullerene!\n");
    writer.color_code = ColorCode::new(Color::White, Color::Black);
    writer.write_string("This is output directly to VGA.\n");
}
