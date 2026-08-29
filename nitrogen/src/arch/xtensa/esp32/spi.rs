//! Bit-banged ESP32 SPI for the LCD bring-up path.

use super::{board::BoardProfile, gpio};

pub struct SpiController {
    sclk: u8,
    mosi: u8,
    dc: u8,
    cs: u8,
    rst: u8,
}

impl SpiController {
    pub const fn new() -> Self {
        Self {
            sclk: 0,
            mosi: 0,
            dc: 0,
            cs: 0,
            rst: 0,
        }
    }

    /// Configure the board's SPI LCD pins for software control.
    pub fn configure(&mut self, profile: &BoardProfile) -> Result<(), SpiError> {
        self.sclk = profile.lcd.sclk;
        self.mosi = profile.lcd.mosi;
        self.dc = profile.lcd.dc;
        self.cs = profile.lcd.cs;
        self.rst = profile.lcd.rst;

        for pin in [self.sclk, self.mosi, self.dc, self.cs, self.rst] {
            gpio::enable_output(pin);
            gpio::set_output_high(pin);
        }
        gpio::set_output_high(self.sclk);
        Ok(())
    }

    fn select(&self) {
        gpio::set_output_low(self.cs);
    }

    fn deselect(&self) {
        gpio::set_output_high(self.cs);
    }

    fn shift_byte(&self, byte: u8) {
        for bit in (0..8).rev() {
            if byte & (1 << bit) != 0 {
                gpio::set_output_high(self.mosi);
            } else {
                gpio::set_output_low(self.mosi);
            }
            gpio::set_output_high(self.sclk);
            gpio::set_output_low(self.sclk);
        }
    }

    pub fn write_command(&self, command: u8) -> Result<(), SpiError> {
        self.select();
        gpio::set_output_low(self.dc);
        self.shift_byte(command);
        gpio::set_output_high(self.dc);
        self.deselect();
        Ok(())
    }

    pub fn write_data(&self, data: &[u8]) -> Result<(), SpiError> {
        self.select();
        gpio::set_output_high(self.dc);
        for byte in data {
            self.shift_byte(*byte);
        }
        self.deselect();
        Ok(())
    }

    pub fn transfer(&self, data: &[u8]) -> Result<(), SpiError> {
        self.write_data(data)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiError {
    Busy,
    Timeout,
}
