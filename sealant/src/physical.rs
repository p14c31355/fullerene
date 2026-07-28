use core::marker::PhantomData;

/// A physical address.  It is an address value, not a dereferenceable pointer.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PhysicalAddress(usize);

impl PhysicalAddress {
    /// Construct a physical address from its integer representation.
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    /// Return the integer representation of this address.
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Return the address after `offset`, or `None` on overflow.
    pub const fn checked_add(self, offset: usize) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(address) => Some(Self(address)),
            None => None,
        }
    }
}

/// A typed physical address which cannot be dereferenced by this crate.
///
/// A page-table or VMM component should convert this token into a virtual
/// capability after establishing an appropriate mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysPtr<T> {
    address: PhysicalAddress,
    _type: PhantomData<fn() -> T>,
}

impl<T> PhysPtr<T> {
    /// Create a typed physical-address token.
    pub const fn new(address: PhysicalAddress) -> Self {
        Self {
            address,
            _type: PhantomData,
        }
    }

    /// Return the physical address.
    pub const fn address(self) -> PhysicalAddress {
        self.address
    }

    /// Reinterpret the token as another element type without dereferencing it.
    pub const fn cast<U>(self) -> PhysPtr<U> {
        PhysPtr::new(self.address)
    }
}
