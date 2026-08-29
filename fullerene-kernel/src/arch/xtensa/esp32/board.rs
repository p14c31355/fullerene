//! ESP32-2432S028/XH-32S board profile.

/// Wiring values are profile defaults pending physical continuity probing.
/// They are isolated here; SoC and driver code never hard-code these pins.
pub const DISPLAY_WIDTH: u16 = 320;
pub const DISPLAY_HEIGHT: u16 = 240;
pub const LCD_SPI_HOST: u8 = 2;
pub const LCD_SCLK: u8 = 14;
pub const LCD_MOSI: u8 = 13;
pub const LCD_DC: u8 = 2;
pub const LCD_CS: u8 = 15;
pub const LCD_RST: u8 = 12;
pub const LCD_BACKLIGHT: u8 = 21;
pub const TOUCH_I2C_SDA: u8 = 33;
pub const TOUCH_I2C_SCL: u8 = 32;
pub const SD_CLK: u8 = 18;
pub const SD_CMD: u8 = 23;
pub const SD_DATA0: u8 = 19;
