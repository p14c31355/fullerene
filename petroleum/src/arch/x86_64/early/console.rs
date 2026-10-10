//! # Early Boot Console
//!
//! Minimal console output for the bootloader phase.
//!
//! ## Capabilities
//!
//! - **Serial** (COM1) — primary output channel, always available.
//! - **VGA text mode** (0xB8000) — optional, useful when a monitor is connected.
//!
//! ## Non-goals
//!
//! - Framebuffer rendering (belongs in `graphics::framebuffer`, not early)
//! - Scrolling with complex buffer management
//! - Colour schemes beyond the basic VGA palette
//!
//! ## Contract
//!
//! This console is for boot-phase logging only.
//! The runtime kernel should use `graphics::PRIMARY_RENDERER` instead.

use core::fmt::{self, Write};
use sealant::{FramebufferRegion, Permissions};
use spin::Mutex;

// ── Serial port constants ──────────────────────────────────────────────
const COM1_DATA: u16 = 0x3F8;
#[cfg(not(any(feature = "std", test)))]
const COM1_LINE_STATUS: u16 = 0x3FD;

/// Write raw bytes to the serial port (blocking, with timeout).
unsafe fn write_serial_raw(bytes: &[u8]) {
    #[cfg(not(any(feature = "std", test)))]
    unsafe {
        for &b in bytes {
            let mut timeout = 1_000_000;
            while (read_serial_status(COM1_LINE_STATUS) & 0x20) == 0 && timeout > 0 {
                timeout -= 1;
            }
            core::arch::asm!(
                "out dx, al",
                in("dx") COM1_DATA,
                in("al") b,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
    #[cfg(any(feature = "std", test))]
    let _ = bytes;
}

#[cfg(not(any(feature = "std", test)))]
#[inline(always)]
unsafe fn read_serial_status(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

// ── VGA text buffer ────────────────────────────────────────────────────
const VGA_ADDRESS: usize = 0xB8000;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

/// VGA text-mode writer used by the early console.
struct VgaTextWriter {
    framebuffer: usize,
    row: usize,
    col: usize,
    color: u8, // high nibble = bg, low nibble = fg
}

impl VgaTextWriter {
    const fn new() -> Self {
        Self {
            framebuffer: VGA_ADDRESS,
            row: 0,
            col: 0,
            color: 0x0F, // white on black
        }
    }

    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.col = 0;
                self.row += 1;
                if self.row >= VGA_HEIGHT {
                    self.scroll();
                }
            }
            b'\r' => self.col = 0,
            b'\t' => self.col = (self.col + 8) & !7,
            c => {
                if self.col >= VGA_WIDTH {
                    self.col = 0;
                    self.row += 1;
                    if self.row >= VGA_HEIGHT {
                        self.scroll();
                    }
                }
                let idx = self.row * VGA_WIDTH + self.col;
                let region = self.region();
                let _ = region.write_volatile_at(
                    idx * core::mem::size_of::<u16>(),
                    (self.color as u16) << 8 | c as u16,
                );
                self.col += 1;
            }
        }
    }

    fn scroll(&mut self) {
        let region = self.region();
        for row in 1..VGA_HEIGHT {
            for col in 0..VGA_WIDTH {
                let src = row * VGA_WIDTH + col;
                let dst = (row - 1) * VGA_WIDTH + col;
                let val = region
                    .read_volatile_at::<u16>(src * core::mem::size_of::<u16>())
                    .unwrap_or(0);
                let _ = region.write_volatile_at(dst * core::mem::size_of::<u16>(), val);
            }
        }
        let blank = (self.color as u16) << 8 | b' ' as u16;
        for col in 0..VGA_WIDTH {
            let idx = (VGA_HEIGHT - 1) * VGA_WIDTH + col;
            let _ = region.write_volatile_at(idx * core::mem::size_of::<u16>(), blank);
        }
        self.row = VGA_HEIGHT - 1;
        self.col = 0;
    }

    fn region(&self) -> FramebufferRegion<'static> {
        unsafe {
            FramebufferRegion::from_address(
                self.framebuffer,
                VGA_WIDTH * VGA_HEIGHT * core::mem::size_of::<u16>(),
                Permissions::READ_WRITE,
            )
            .expect("invalid VGA framebuffer region")
        }
    }
}

impl Write for VgaTextWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            self.write_byte(b);
        }
        Ok(())
    }
}

// ── EarlyConsole (serial + VGA) ────────────────────────────────────────

/// Boot-phase console that writes to serial and VGA text mode.
pub struct EarlyConsole {
    vga: Mutex<VgaTextWriter>,
    serial_initialized: Mutex<bool>,
}

impl EarlyConsole {
    const fn new() -> Self {
        Self {
            vga: Mutex::new(VgaTextWriter::new()),
            serial_initialized: Mutex::new(false),
        }
    }

    /// Initialise the serial port (COM1, 115200 8N1).
    /// Safe to call multiple times (idempotent).
    pub fn init_serial(&self) {
        let mut init = self.serial_initialized.lock();
        if *init {
            return;
        }
        unsafe {
            let config: &[(u16, u8)] = &[
                (COM1_DATA + 3, 0x80),
                (COM1_DATA, 0x01),
                (COM1_DATA + 1, 0x00),
                (COM1_DATA + 3, 0x03),
                (COM1_DATA + 2, 0xC7),
                (COM1_DATA + 4, 0x0B),
            ];
            for &(port, value) in config {
                core::arch::asm!(
                    "out dx, al",
                    in("dx") port,
                    in("al") value,
                    options(nomem, nostack, preserves_flags),
                );
            }
        }
        *init = true;
    }

    /// Write a formatted string to serial and VGA.
    pub fn write_fmt(&self, args: fmt::Arguments<'_>) {
        // Serial output via a trivial fmt::Write impl
        struct SerialWriter;
        impl fmt::Write for SerialWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                unsafe { write_serial_raw(s.as_bytes()) }
                Ok(())
            }
        }
        let _ = core::fmt::write(&mut SerialWriter, args);

        // VGA output
        let mut vga = self.vga.lock();
        let _ = core::fmt::write(&mut *vga, args);
    }
}

/// Global early console instance for bootloader-phase logging.
pub static EARLY_CONSOLE: EarlyConsole = EarlyConsole::new();

/// Convenience macro for early boot logging.
///
/// Usage: `early_println!("Hello {:#x}", value);`
#[macro_export]
macro_rules! early_println {
    ($($arg:tt)*) => {
        $crate::early::console::EARLY_CONSOLE.write_fmt(format_args!($($arg)*));
    };
}
