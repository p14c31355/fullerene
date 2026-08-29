//! SPI LCD backend for Lattice.

use super::{
    board::{BoardProfile, DISPLAY_HEIGHT, DISPLAY_WIDTH},
    gpio,
    spi::{SpiController, SpiError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LcdError {
    Spi(SpiError),
    OutOfRange,
}

impl From<SpiError> for LcdError {
    fn from(value: SpiError) -> Self {
        Self::Spi(value)
    }
}

pub struct SpiLcd {
    controller: SpiController,
    dirty: Option<(u16, u16, u16, u16)>,
}

impl SpiLcd {
    pub const fn new() -> Self {
        Self {
            controller: SpiController::new(),
            dirty: None,
        }
    }

    pub fn init(&mut self) -> Result<(), LcdError> {
        self.controller.configure()?;
        self.mark_full_dirty();
        Ok(())
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (DISPLAY_WIDTH, DISPLAY_HEIGHT)
    }

    pub fn mark_full_dirty(&mut self) {
        self.dirty = Some((0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT));
    }

    pub fn mark_dirty(&mut self, x: u16, y: u16, width: u16, height: u16) {
        let x2 = x.saturating_add(width);
        let y2 = y.saturating_add(height);
        self.dirty = Some(match self.dirty {
            Some((dx, dy, dw, dh)) => (
                dx.min(x),
                dy.min(y),
                dx.max(x2).saturating_sub(dx),
                dy.max(y2).saturating_sub(dy),
            ),
            None => (x, y, width, height),
        });
    }

    pub fn flush(&mut self, pixels: &[u16]) -> Result<(), LcdError> {
        let _ = pixels;
        self.dirty = None;
        Ok(())
    }

    pub fn set_backlight(&self, on: bool) {
        let profile = BoardProfile::xh32s();
        if on {
            gpio::set_output_high(profile.lcd.backlight);
        } else {
            gpio::set_output_low(profile.lcd.backlight);
        }
    }
}
