//! ESP32 SDMMC transport boundary.
//!
//! This module owns only the card-host mechanism. Genome owns filesystem
//! semantics and converts a successfully probed host into its block contract.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdMmcError {
    NotPresent,
    NotInitialized,
    CommandFailed,
    BufferTooSmall { required: usize, provided: usize },
}

pub struct SdMmcHost {
    initialized: bool,
    sectors: u64,
}

impl SdMmcHost {
    pub const fn new() -> Self {
        Self {
            initialized: false,
            sectors: 0,
        }
    }

    /// Probe the physical card. Return an explicit error until the ESP32
    /// SDMMC host is implemented; never report a card that was not detected.
    pub fn detect(&mut self) -> Result<(), SdMmcError> {
        self.initialized = false;
        self.sectors = 0;
        Err(SdMmcError::NotPresent)
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn sector_count(&self) -> u64 {
        self.sectors
    }

    pub fn read_sectors_raw(
        &mut self,
        _lba: u64,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), SdMmcError> {
        let required = usize::from(count) * 512;
        if buf.len() < required {
            return Err(SdMmcError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        if !self.initialized {
            return Err(SdMmcError::NotInitialized);
        }
        Err(SdMmcError::CommandFailed)
    }

    pub fn write_sectors_raw(
        &mut self,
        _lba: u64,
        count: u16,
        buf: &[u8],
    ) -> Result<(), SdMmcError> {
        let required = usize::from(count) * 512;
        if buf.len() < required {
            return Err(SdMmcError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        if !self.initialized {
            return Err(SdMmcError::NotInitialized);
        }
        Err(SdMmcError::CommandFailed)
    }
}

impl Default for SdMmcHost {
    fn default() -> Self {
        Self::new()
    }
}
