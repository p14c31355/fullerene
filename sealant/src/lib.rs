#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

//! Explicit capability types for checked access to low-level memory.
//!
//! `sealant` deliberately does not turn an arbitrary raw pointer into a safe
//! reference.  A [`MemoryRegion`] only describes a range and its permissions;
//! the code which knows that the range is mapped and remains valid must create
//! that description.  Pointer construction then checks nullness, alignment,
//! overflow, bounds, permissions, and the kind of memory being accessed.
//!
//! The actual operations which read or write untyped memory remain `unsafe`.
//! This is intentional: a range check cannot prove initialization, pointer
//! provenance, concurrent access, or that an MMU mapping will not disappear.
//! The API makes those assumptions visible at the narrowest possible scope.

mod error;
mod physical;
mod region;

pub use error::{PointerError, RegionError};
pub use physical::{PhysPtr, PhysicalAddress};
pub use region::{
    CheckedMut, DmaPtr, DmaRegion, DmaWritePtr, ExclusiveRamRegion, MemoryRegion, MmioPtr,
    MmioRegion, MmioWritePtr, Permissions, RamPtr, RamRegion, RamWritePtr, RegionKind, SlicePtr,
    UserPtr, UserPtrMut, UserRegion,
};
