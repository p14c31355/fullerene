use core::fmt;

/// Errors returned while describing a memory region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError {
    /// The range's end address would overflow `usize`.
    AddressOverflow,
    /// A region with these parameters cannot be represented.
    InvalidRange,
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressOverflow => f.write_str("memory region address overflow"),
            Self::InvalidRange => f.write_str("invalid memory region"),
        }
    }
}

impl core::error::Error for RegionError {}

/// Errors returned when converting a raw pointer into a checked capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerError {
    /// The supplied pointer was null.
    Null,
    /// The pointer is not aligned for the requested type.
    Unaligned,
    /// The pointer plus the requested size overflowed `usize`.
    AddressOverflow,
    /// The requested bytes are outside the region.
    OutOfBounds,
    /// The region does not grant the requested operation.
    PermissionDenied,
    /// The operation is not valid for this kind of region.
    WrongRegionKind,
    /// A slice length multiplied by the element size overflowed `usize`.
    LengthOverflow,
}

impl fmt::Display for PointerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null pointer"),
            Self::Unaligned => f.write_str("unaligned pointer"),
            Self::AddressOverflow => f.write_str("pointer address overflow"),
            Self::OutOfBounds => f.write_str("pointer is outside the memory region"),
            Self::PermissionDenied => f.write_str("memory permission denied"),
            Self::WrongRegionKind => f.write_str("wrong memory region kind"),
            Self::LengthOverflow => f.write_str("slice length overflow"),
        }
    }
}

impl core::error::Error for PointerError {}
