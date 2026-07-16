use core::fmt;

use crate::driver_context::DriverContextError;

/// Hardware-driver failures exposed across the mechanism/policy boundary.
///
/// Drivers should prefer this enum over string errors so callers can preserve
/// retry, resource, and device-failure semantics when translating to an ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// No matching hardware device is present.
    DeviceNotFound,
    /// The device has not reached an operational state yet.
    NotReady,
    /// A driver argument or hardware descriptor is invalid.
    InvalidArgument,
    /// A required driver-owned allocation failed.
    OutOfMemory,
    /// A memory-mapped register range could not be mapped.
    MmioMappingFailed,
    /// A DMA buffer could not be mapped for the device.
    DmaMappingFailed,
    /// The device did not complete an operation before its deadline.
    TimedOut,
    /// The device cannot accept the operation while busy.
    Busy,
    /// The device or driver does not support the requested operation.
    NotSupported,
    /// A generic device I/O transaction failed.
    Io,
    /// The device returned an invalid protocol response.
    Protocol,
    /// The device reported a fatal internal fault.
    DeviceFault,
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DeviceNotFound => "device not found",
            Self::NotReady => "device not ready",
            Self::InvalidArgument => "invalid driver argument",
            Self::OutOfMemory => "driver allocation failed",
            Self::MmioMappingFailed => "MMIO mapping failed",
            Self::DmaMappingFailed => "DMA mapping failed",
            Self::TimedOut => "driver operation timed out",
            Self::Busy => "device busy",
            Self::NotSupported => "driver operation not supported",
            Self::Io => "device I/O error",
            Self::Protocol => "device protocol error",
            Self::DeviceFault => "device fault",
        })
    }
}

impl From<DriverContextError> for DriverError {
    fn from(error: DriverContextError) -> Self {
        match error {
            DriverContextError::OutOfMemory => Self::OutOfMemory,
            DriverContextError::MmioMappingFailed => Self::MmioMappingFailed,
            DriverContextError::InvalidArgument => Self::InvalidArgument,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_context_errors_preserve_their_category() {
        assert_eq!(
            DriverError::from(DriverContextError::OutOfMemory),
            DriverError::OutOfMemory
        );
        assert_eq!(
            DriverError::from(DriverContextError::MmioMappingFailed),
            DriverError::MmioMappingFailed
        );
        assert_eq!(
            DriverError::from(DriverContextError::InvalidArgument),
            DriverError::InvalidArgument
        );
    }
}
