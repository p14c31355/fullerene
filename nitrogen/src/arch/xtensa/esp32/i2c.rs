//! Shared-bit I2C mechanism used by the touch controller during bring-up.

pub struct I2cBus {
    sda: u8,
    scl: u8,
}

impl I2cBus {
    pub const fn new(sda: u8, scl: u8) -> Self {
        Self { sda, scl }
    }
}
