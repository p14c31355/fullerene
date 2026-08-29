//! SPI LCD backend for Lattice.

use super::{
    board::{BoardProfile, DISPLAY_HEIGHT, DISPLAY_WIDTH},
    gpio,
    spi::{SpiController, SpiError},
};

fn delay_ms(milliseconds: u32) {
    for _ in 0..milliseconds * 80_000 {
        core::hint::spin_loop();
    }
}

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
        let profile = BoardProfile::xh32s();
        self.controller.configure(&profile)?;
        gpio::set_output_low(profile.lcd.rst);
        delay_ms(20);
        gpio::set_output_high(profile.lcd.rst);
        delay_ms(120);

        self.command(0x01)?; // software reset
        delay_ms(120);
        self.command(0x3A)?;
        self.data(&[0x55])?;
        self.command(0x36)?;
        self.data(&[0x48])?;
        self.command(0x11)?; // sleep out
        delay_ms(120);
        self.command(0x29)?; // display on
        self.draw_test_pattern()?;
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

    fn command(&self, command: u8) -> Result<(), LcdError> {
        self.controller.write_command(command)?;
        Ok(())
    }

    fn data(&self, data: &[u8]) -> Result<(), LcdError> {
        self.controller.write_data(data)?;
        Ok(())
    }

    /// Fill the visible panel with four bands so the SPI wiring can be
    /// verified without allocating the full Lattice surface.
    fn draw_test_pattern(&self) -> Result<(), LcdError> {
        self.command(0x2A)?;
        self.data(&[
            0,
            0,
            (DISPLAY_WIDTH - 1 >> 8) as u8,
            DISPLAY_WIDTH as u8 - 1,
        ])?;
        self.command(0x2B)?;
        self.data(&[
            0,
            0,
            (DISPLAY_HEIGHT - 1 >> 8) as u8,
            DISPLAY_HEIGHT as u8 - 1,
        ])?;
        self.command(0x2C)?;

        let mut row = [0u8; DISPLAY_WIDTH as usize * 2];
        for y in 0..DISPLAY_HEIGHT {
            let color = if y < 80 {
                0xf8_00
            } else if y < 160 {
                0x07_e0
            } else {
                0x00_1f
            };
            for (index, bytes) in row.chunks_exact_mut(2).enumerate() {
                let shade: u16 = if (index / 40) & 1 == 0 { color } else { 0xffff };
                bytes.copy_from_slice(&shade.to_be_bytes());
            }
            self.controller.transfer(&row)?;
        }
        Ok(())
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
