//! XPT2046 resistive-touch backend for the ESP32 carrier board.
//!
//! The controller uses a separate SPI transport from the LCD. Coordinates are
//! normalized by calibration, but the absolute calibration is still provisional
//! until the user performs a four-corner touch calibration.

use super::{board::BoardProfile, gpio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub min_x: u16,
    pub max_x: u16,
    pub min_y: u16,
    pub max_y: u16,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub swap_xy: bool,
}

impl Calibration {
    /// Convert a calibrated raw point to screen coordinates.
    ///
    /// The mapping clamps before division, avoiding 16-bit overflow and making
    /// noisy edge samples stay within the panel.
    pub fn map_to_screen(&self, sample: TouchSample, width: u16, height: u16) -> (u16, u16) {
        let clamped = TouchSample {
            x: sample.x.clamp(self.min_x, self.max_x),
            y: sample.y.clamp(self.min_y, self.max_y),
            pressed: sample.pressed,
        };
        let raw_x = if self.swap_xy { clamped.y } else { clamped.x };
        let raw_y = if self.swap_xy { clamped.x } else { clamped.y };
        let span_x = u32::from(self.max_x - self.min_x).max(1);
        let span_y = u32::from(self.max_y - self.min_y).max(1);
        let mapped_x = (u32::from(raw_x - self.min_x) * u32::from(width.saturating_sub(1))
            / span_x)
            .min(u32::from(width.saturating_sub(1)));
        let mapped_y = (u32::from(raw_y - self.min_y) * u32::from(height.saturating_sub(1))
            / span_y)
            .min(u32::from(height.saturating_sub(1)));
        let screen_x = if self.mirror_x {
            u32::from(width.saturating_sub(1)).saturating_sub(mapped_x)
        } else {
            mapped_x
        };
        let screen_y = if self.mirror_y {
            u32::from(height.saturating_sub(1)).saturating_sub(mapped_y)
        } else {
            mapped_y
        };
        (screen_x as u16, screen_y as u16)
    }
}

pub struct Xpt2046Touch {
    clk: u8,
    mosi: u8,
    miso: u8,
    cs: u8,
    irq: u8,
    calibration: Calibration,
}

impl Xpt2046Touch {
    pub fn new(profile: BoardProfile) -> Self {
        let pins = profile.touch;
        gpio::enable_output(pins.clk);
        gpio::enable_output(pins.mosi);
        gpio::enable_output(pins.cs);
        gpio::enable_input(pins.miso);
        gpio::enable_input_pullup(pins.irq);
        // XPT2046 uses SPI mode 0, so the first command bit must begin with
        // a real low-to-high clock edge.
        gpio::set_output_low(pins.clk);
        gpio::set_output_high(pins.cs);
        gpio::set_output_high(pins.mosi);
        Self {
            clk: pins.clk,
            mosi: pins.mosi,
            miso: pins.miso,
            cs: pins.cs,
            irq: pins.irq,
            calibration: Calibration {
                min_x: 280,
                max_x: 3_860,
                min_y: 340,
                max_y: 3_860,
                mirror_x: true,
                mirror_y: false,
                swap_xy: false,
            },
        }
    }

    pub fn set_calibration(&mut self, calibration: Calibration) {
        self.calibration = calibration;
    }

    /// Read the touch controller once. The controller is polled even when IRQ
    /// is high: this keeps bring-up usable on board variants with a missing or
    /// incorrectly pulled IRQ trace. Pressure remains the authoritative gate.
    pub fn read(&mut self) -> Option<TouchSample> {
        let pressure = self.read_channel(0xb0);
        if pressure < 40 {
            return Some(TouchSample {
                x: 0,
                y: 0,
                pressed: false,
            });
        }
        let x = self.read_channel(0xd0);
        let y = self.read_channel(0x90);
        Some(TouchSample {
            x,
            y,
            pressed: true,
        })
    }

    pub fn map_to_screen(&self, sample: TouchSample, width: u16, height: u16) -> (u16, u16) {
        self.calibration.map_to_screen(sample, width, height)
    }

    fn read_channel(&mut self, command: u8) -> u16 {
        self.select();
        self.shift_byte(command);
        let high = self.shift_byte(0);
        let low = self.shift_byte(0);
        self.deselect();
        ((u16::from(high) << 8) | u16::from(low)) >> 4
    }

    fn select(&mut self) {
        gpio::set_output_low(self.cs);
    }

    fn deselect(&mut self) {
        gpio::set_output_high(self.cs);
    }

    fn shift_byte(&mut self, byte: u8) -> u8 {
        let mut value = 0;
        for bit in (0..8).rev() {
            if byte & (1 << bit) != 0 {
                gpio::set_output_high(self.mosi);
            } else {
                gpio::set_output_low(self.mosi);
            }
            gpio::set_output_high(self.clk);
            value <<= 1;
            if gpio::input(self.miso) == Some(true) {
                value |= 1;
            }
            gpio::set_output_low(self.clk);
        }
        value
    }
}
