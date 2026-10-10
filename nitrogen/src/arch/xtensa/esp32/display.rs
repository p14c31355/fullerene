//! ILI9341 panel driver over the ESP32 HSPI peripheral.
//!
//! The panel is driven by hardware SPI (see `super::spi`), and every transfer
//! is a clipped rectangle handed in by the caller. This module deliberately
//! holds no dirty state of its own: the compositor owns the knowledge of what
//! changed, and a single copy of that knowledge is what the runtime reads
//! (dirty tracking that lives in two places drifts).
//!
//! `flush_paced` is the cooperative form: it hands control back to the caller
//! between 64-byte chunks so the input path keeps sampling while a large
//! transfer is in flight.

use super::{
    board::{BoardProfile, DISPLAY_HEIGHT, DISPLAY_WIDTH},
    gpio,
    spi::{SpiController, SpiError},
};
use crate::geometry::Rect;

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

/// A clipped region of the panel, in panel coordinates.
///
/// The rectangle arithmetic (clip/union/size) is the same one the compositor
/// uses, so it comes from the shared geometry module rather than being
/// reimplemented here.
pub type LcdRect = Rect;

/// The whole panel as a rectangle.
pub const FULL_FRAME: LcdRect = Rect::new(0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT);

pub struct SpiLcd {
    controller: SpiController,
}

impl SpiLcd {
    pub const fn new() -> Self {
        Self {
            controller: SpiController::new(),
        }
    }

    pub fn init(&mut self) -> Result<(), LcdError> {
        self.init_at(super::spi::DEFAULT_SPI_HZ)
    }

    /// Bring up the panel at a specific HSPI clock.
    pub fn init_at(&mut self, hz: u32) -> Result<(), LcdError> {
        let profile = BoardProfile::xh32s();
        self.controller.configure_at(&profile, hz)?;
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

    /// The configured shift clock, for the latency report.
    pub fn clock_hz(&self) -> u32 {
        self.controller.clock_hz()
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (DISPLAY_WIDTH, DISPLAY_HEIGHT)
    }

    fn command(&mut self, command: u8) -> Result<(), LcdError> {
        self.controller.write_command(command)?;
        Ok(())
    }

    fn data(&mut self, data: &[u8]) -> Result<(), LcdError> {
        self.controller.write_data(data)?;
        Ok(())
    }

    fn set_window(&mut self, rect: LcdRect) -> Result<(), LcdError> {
        let x_end = rect.x + rect.width - 1;
        let y_end = rect.y + rect.height - 1;
        self.command(0x2A)?;
        self.data(&[
            (rect.x >> 8) as u8,
            rect.x as u8,
            (x_end >> 8) as u8,
            x_end as u8,
        ])?;
        self.command(0x2B)?;
        self.data(&[(rect.y >> 8) as u8, rect.y as u8, (y_end >> 8) as u8, y_end as u8])?;
        self.command(0x2C)
    }

    /// Present one clipped region of the Lattice RGB565 surface.
    ///
    /// Pixels are sent most-significant byte first, as the controller's
    /// RAM-write protocol requires.
    pub fn flush(&mut self, pixels: &[u16], rect: LcdRect) -> Result<(), LcdError> {
        self.flush_inner(pixels, rect, None)
    }

    /// Present one clipped region, yielding to `pace` between SPI chunks.
    ///
    /// The runtime passes a closure that keeps polling the panel, so a flush
    /// never blocks input for more than one 64-byte chunk.
    pub fn flush_paced(
        &mut self,
        pixels: &[u16],
        rect: LcdRect,
        pace: &mut dyn FnMut(),
    ) -> Result<(), LcdError> {
        self.flush_inner(pixels, rect, Some(pace))
    }

    fn flush_inner(
        &mut self,
        pixels: &[u16],
        rect: LcdRect,
        mut pace: Option<&mut dyn FnMut()>,
    ) -> Result<(), LcdError> {
        let rect = clip(rect);
        let Some(rect) = rect else {
            return Ok(());
        };
        self.set_window(rect)?;
        let mut row = [0u8; LCD_ROW_BYTES];
        for row_index in 0..u32::from(rect.height) {
            let surface_y = u32::from(rect.y) + row_index;
            let start = surface_y * u32::from(DISPLAY_WIDTH) + u32::from(rect.x);
            let source = pixels
                .get(start as usize..start as usize + usize::from(rect.width))
                .ok_or(LcdError::OutOfRange)?;
            let length = usize::from(rect.width) * 2;
            for (destination, pixel) in row[..length].chunks_exact_mut(2).zip(source) {
                destination.copy_from_slice(&pixel.to_be_bytes());
            }
            match pace.as_mut() {
                Some(callback) => {
                    self.controller.write_data_paced(&row[..length], *callback)?;
                }
                None => {
                    self.controller.write_data(&row[..length])?;
                }
            }
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

/// Clip a rectangle to the panel, returning `None` when nothing is visible.
fn clip(rect: LcdRect) -> Option<LcdRect> {
    rect.clipped(DISPLAY_WIDTH, DISPLAY_HEIGHT)
}
