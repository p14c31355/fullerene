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

const LCD_ROW_BYTES: usize = DISPLAY_WIDTH as usize * 2;

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
        gpio::enable_output(profile.lcd.backlight);
        gpio::set_output_low(profile.lcd.backlight);
        gpio::set_output_low(profile.lcd.rst);
        delay_ms(20);
        gpio::set_output_high(profile.lcd.rst);
        delay_ms(120);

        self.command(0x01)?; // software reset
        delay_ms(120);
        self.command(0x3A)?;
        self.data(&[0x55])?;
        self.command(0x36)?;
        // ILI9341 native memory is 240x320.  MV selects the 320x240
        // landscape address order; BGR matches the panel wiring.
        self.data(&[0x28])?;
        self.command(0x11)?; // sleep out
        delay_ms(120);
        self.command(0x29)?; // display on
        // Leave initialization black. Lattice composes and presents the first
        // desktop frame as soon as the RGB565 surface is allocated.
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
            Some((dx, dy, dirty_width, dirty_height)) => {
                let left = dx.min(x);
                let top = dy.min(y);
                let right = dx.saturating_add(dirty_width).max(x2);
                let bottom = dy.saturating_add(dirty_height).max(y2);
                (
                    left,
                    top,
                    right.saturating_sub(left),
                    bottom.saturating_sub(top),
                )
            }
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

    fn set_window(&self, x: u16, y: u16, width: u16, height: u16) -> Result<(), LcdError> {
        let x_end = x + width - 1;
        let y_end = y + height - 1;
        self.command(0x2A)?;
        self.data(&[(x >> 8) as u8, x as u8, (x_end >> 8) as u8, x_end as u8])?;
        self.command(0x2B)?;
        self.data(&[(y >> 8) as u8, y as u8, (y_end >> 8) as u8, y_end as u8])?;
        self.command(0x2C)
    }

    /// Present the clipped rows from the Lattice RGB565 surface. Pixels are
    /// sent big-endian, as required by the controller's RAM-write protocol.
    pub fn flush(&mut self, pixels: &[u16]) -> Result<(), LcdError> {
        let Some((x, y, width, height)) = self.dirty else {
            return Ok(());
        };
        self.dirty = None;
        if width == 0 || height == 0 || x >= DISPLAY_WIDTH || y >= DISPLAY_HEIGHT {
            return Ok(());
        }
        let width = width.min(DISPLAY_WIDTH - x);
        let height = height.min(DISPLAY_HEIGHT - y);
        self.set_window(x, y, width, height)?;
        let mut row = [0u8; LCD_ROW_BYTES];
        for row_index in 0..u32::from(height) {
            let surface_y = u32::from(y) + row_index;
            let start = surface_y * u32::from(DISPLAY_WIDTH) + u32::from(x);
            let source = pixels
                .get(start as usize..start as usize + usize::from(width))
                .ok_or(LcdError::OutOfRange)?;
            for (destination, pixel) in row.chunks_exact_mut(2).zip(source) {
                destination.copy_from_slice(&pixel.to_be_bytes());
            }
            self.controller.write_data(&row[..usize::from(width) * 2])?;
        }
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
