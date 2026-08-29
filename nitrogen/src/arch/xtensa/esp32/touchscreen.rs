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

    /// Read the touch controller once. The XPT2046 PENIRQ line is active low;
    /// use it as the cheap idle filter before starting the SPI conversion.
    pub fn read(&mut self) -> Option<TouchSample> {
        if gpio::input(self.irq) != Some(false) {
            return Some(TouchSample {
                x: 0,
                y: 0,
                pressed: false,
            });
        }
        self.select();
        let z1 = self.read_adc(0xb1);
        let z2 = self.read_adc(0xc1);
        let pressure = z1.saturating_add(4_095).saturating_sub(z2);
        if pressure < 300 {
            self.deselect();
            return Some(TouchSample {
                x: 0,
                y: 0,
                pressed: false,
            });
        }

        // The first X conversion after the pressure sequence is noisy.
        let _ = self.read_adc(0x91);
        let x_first = self.read_adc(0xd1);
        let y_first = self.read_adc(0x91);
        let x_second = self.read_adc(0xd1);
        let y_second = self.read_adc(0x91);
        let _ = self.read_adc(0xd0); // final Y conversion and power down
        let _ = self.read_adc(0x00);
        self.deselect();

        let x = ((u32::from(x_first) + u32::from(x_second)) / 2) as u16;
        let y = ((u32::from(y_first) + u32::from(y_second)) / 2) as u16;
        Some(TouchSample {
            x,
            y,
            pressed: true,
        })
    }

    pub fn map_to_screen(&self, sample: TouchSample, width: u16, height: u16) -> (u16, u16) {
        self.calibration.map_to_screen(sample, width, height)
    }

    fn read_adc(&mut self, command: u8) -> u16 {
        self.shift_byte(command);
        let mut value = 0u16;
        for bit in (0..12).rev() {
            gpio::set_output_high(self.clk);
            clock_delay();
            if gpio::input(self.miso) == Some(true) {
                value |= 1 << bit;
            }
            gpio::set_output_low(self.clk);
            clock_delay();
        }
        value
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
            clock_delay();
            gpio::set_output_high(self.clk);
            clock_delay();
            value <<= 1;
            if gpio::input(self.miso) == Some(true) {
                value |= 1;
            }
            gpio::set_output_low(self.clk);
            clock_delay();
        }
        value
    }
}

/// Keep the software SPI clock within the XPT2046's conservative timing
/// budget.  The LCD tolerates the much faster display bit-bang path, but the
/// touch controller is specified for a roughly 2.5 MHz clock.
#[inline(never)]
fn clock_delay() {
    for _ in 0..16 {
        core::hint::spin_loop();
    }
}
