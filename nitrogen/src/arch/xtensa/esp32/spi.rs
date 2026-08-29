//! ESP32 SPI2 controller mechanism.

const SPI2_BASE: usize = 0x3ff6_4000;

pub struct SpiController;

impl SpiController {
    pub const fn new() -> Self {
        Self
    }

    pub fn configure(&self) -> Result<(), SpiError> {
        unsafe {
            (SPI2_BASE as *mut u32).write_volatile(0);
            ((SPI2_BASE + 0x18) as *mut u32).write_volatile(0x0080_0000);
        }
        Ok(())
    }

    pub fn transfer(&self, data: &[u8]) -> Result<(), SpiError> {
        let _ = data;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiError {
    Busy,
    Timeout,
}
