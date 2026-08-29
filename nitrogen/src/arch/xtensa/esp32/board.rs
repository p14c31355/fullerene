//! Board wiring profiles.

pub const DISPLAY_WIDTH: u16 = 320;
pub const DISPLAY_HEIGHT: u16 = 240;

#[derive(Clone, Copy, Debug)]
pub struct LcdPinout {
    pub spi_host: u8,
    pub sclk: u8,
    pub mosi: u8,
    pub dc: u8,
    pub cs: u8,
    pub rst: u8,
    pub backlight: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct TouchPinout {
    pub clk: u8,
    pub mosi: u8,
    pub miso: u8,
    pub cs: u8,
    pub irq: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct SdPinout {
    pub clk: u8,
    pub cmd: u8,
    pub data0: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct BoardProfile {
    pub lcd: LcdPinout,
    pub touch: TouchPinout,
    pub sd: SdPinout,
}

impl BoardProfile {
    pub const fn xh32s() -> Self {
        Self {
            lcd: LcdPinout {
                spi_host: 2,
                sclk: 14,
                mosi: 13,
                dc: 2,
                cs: 15,
                rst: 12,
                backlight: 21,
            },
            touch: TouchPinout {
                clk: 25,
                mosi: 32,
                miso: 36,
                cs: 33,
                irq: 39,
            },
            sd: SdPinout {
                clk: 18,
                cmd: 23,
                data0: 19,
            },
        }
    }
}
