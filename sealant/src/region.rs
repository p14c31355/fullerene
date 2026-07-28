use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ptr::NonNull;

use crate::{PointerError, RegionError};

/// Permissions attached to a memory region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions(u8);

impl Permissions {
    /// No access.
    pub const NONE: Self = Self(0);
    /// Read access.
    pub const READ: Self = Self(1 << 0);
    /// Write access.
    pub const WRITE: Self = Self(1 << 1);
    /// Execute access.
    pub const EXECUTE: Self = Self(1 << 2);
    /// Read and write access.
    pub const READ_WRITE: Self = Self(Self::READ.0 | Self::WRITE.0);

    /// Return whether all permissions in `required` are present.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Combine two permission sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The semantic kind of a region.  The kind selects the operations available
/// on the resulting capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionKind {
    /// Ordinary mapped RAM.
    Ram,
    /// Device registers; access must be volatile.
    Mmio,
    /// Memory owned by a user address space.
    User,
    /// Memory participating in device DMA.
    Dma,
    /// A kernel-owned range.
    Kernel,
    /// A framebuffer range.
    Framebuffer,
}

/// A checked description of a contiguous virtual address range.
///
/// This is metadata, not an OS page-table query.  Constructing it from a raw
/// address is therefore `unsafe`; the caller must guarantee that the range is
/// mapped as described for the lifetime carried by the returned value.
#[derive(Clone, Debug)]
pub struct MemoryRegion<'a> {
    start: usize,
    len: usize,
    permissions: Permissions,
    kind: RegionKind,
    _lifetime: PhantomData<&'a [u8]>,
}

impl<'a> MemoryRegion<'a> {
    fn new(
        start: usize,
        len: usize,
        permissions: Permissions,
        kind: RegionKind,
    ) -> Result<Self, RegionError> {
        start.checked_add(len).ok_or(RegionError::AddressOverflow)?;
        Ok(Self {
            start,
            len,
            permissions,
            kind,
            _lifetime: PhantomData,
        })
    }

    /// Create a RAM region backed by a shared Rust slice.
    pub fn from_slice<T>(slice: &'a [T]) -> Result<Self, RegionError> {
        Self::new(
            slice.as_ptr() as usize,
            slice
                .len()
                .checked_mul(size_of::<T>())
                .ok_or(RegionError::AddressOverflow)?,
            Permissions::READ,
            RegionKind::Ram,
        )
    }

    /// Create a RAM region backed by a mutable Rust slice.
    pub fn from_mut_slice<T>(slice: &'a mut [T]) -> Result<Self, RegionError> {
        Self::new(
            slice.as_mut_ptr() as usize,
            slice
                .len()
                .checked_mul(size_of::<T>())
                .ok_or(RegionError::AddressOverflow)?,
            Permissions::READ_WRITE,
            RegionKind::Ram,
        )
    }

    /// Create a region from a raw mapped address range.
    ///
    /// # Safety
    ///
    /// `start..start + len` must be a valid, continuously mapped range for
    /// the lifetime `'a`, with the declared permissions and memory kind.  The
    /// caller must also ensure that the mapping remains present and that any
    /// typed reads use initialized values with valid bit patterns.
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
        kind: RegionKind,
    ) -> Result<Self, RegionError> {
        Self::new(start, len, permissions, kind)
    }

    /// Start address of the region.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Length in bytes.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the region contains no bytes.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exclusive end address, which is known not to overflow.
    pub const fn end(&self) -> usize {
        self.start + self.len
    }

    /// Permissions attached to the region.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Semantic kind of the region.
    pub const fn kind(&self) -> RegionKind {
        self.kind
    }

    fn contains(&self, address: usize, len: usize) -> bool {
        let Some(end) = address.checked_add(len) else {
            return false;
        };
        address >= self.start && end <= self.end()
    }

    fn check<T>(
        &self,
        ptr: *const T,
        len: usize,
        permission: Permissions,
    ) -> Result<NonNull<T>, PointerError> {
        if ptr.is_null() {
            return Err(PointerError::Null);
        }
        let address = ptr as usize;
        if address % align_of::<T>() != 0 {
            return Err(PointerError::Unaligned);
        }
        if !self.permissions.contains(permission) {
            return Err(PointerError::PermissionDenied);
        }
        if !self.contains(address, len) {
            return if address.checked_add(len).is_none() {
                Err(PointerError::AddressOverflow)
            } else {
                Err(PointerError::OutOfBounds)
            };
        }
        // Nullness was checked above, so this conversion cannot fail.
        Ok(unsafe { NonNull::new_unchecked(ptr as *mut T) })
    }

    fn check_slice<T>(
        &self,
        ptr: *const T,
        count: usize,
        permission: Permissions,
    ) -> Result<NonNull<T>, PointerError> {
        let len = count
            .checked_mul(size_of::<T>())
            .ok_or(PointerError::LengthOverflow)?;
        self.check(ptr, len, permission)
    }
}

/// Region restricted to ordinary RAM access.
#[derive(Debug)]
pub struct RamRegion<'a>(MemoryRegion<'a>);

impl<'a> RamRegion<'a> {
    /// Create a read-only RAM region from a shared slice.
    pub fn from_slice<T>(slice: &'a [T]) -> Result<Self, RegionError> {
        Ok(Self(MemoryRegion::from_slice(slice)?))
    }

    /// Create an accessible RAM region from a raw mapped range.
    ///
    /// # Safety
    ///
    /// See [`MemoryRegion::from_raw_parts`].
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self(unsafe {
            MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::Ram)?
        }))
    }

    /// Create a read capability for one value.
    pub fn check_read<T>(&self, ptr: *const T) -> Result<RamPtr<'_, T>, PointerError> {
        Ok(RamPtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::READ)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Create a write capability for one value.
    pub fn check_write<T>(&self, ptr: *mut T) -> Result<RamWritePtr<'_, T>, PointerError> {
        Ok(RamWritePtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::WRITE)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Check a contiguous array of values.
    pub fn check_slice<T>(
        &self,
        ptr: *const T,
        count: usize,
    ) -> Result<SlicePtr<'_, T>, PointerError> {
        Ok(SlicePtr {
            ptr: self.0.check_slice(ptr, count, Permissions::READ)?,
            len: count,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Return the underlying region metadata.
    pub const fn region(&self) -> &MemoryRegion<'a> {
        &self.0
    }
}

/// A RAM region with an exclusive borrow of its represented memory.
#[derive(Debug)]
pub struct ExclusiveRamRegion<'a> {
    region: RamRegion<'a>,
    _exclusive: PhantomData<&'a mut [u8]>,
}

impl<'a> ExclusiveRamRegion<'a> {
    /// Create an exclusive RAM region from a mutable Rust slice.
    pub fn from_mut_slice<T>(slice: &'a mut [T]) -> Result<Self, RegionError> {
        Ok(Self {
            region: RamRegion(MemoryRegion::from_mut_slice(slice)?),
            _exclusive: PhantomData,
        })
    }

    /// Create an exclusive RAM region from a raw mapped range.
    ///
    /// # Safety
    ///
    /// The caller must additionally guarantee that no other access, mutable
    /// or shared, to the represented bytes can occur while this value exists.
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self {
            region: RamRegion(unsafe {
                MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::Ram)?
            }),
            _exclusive: PhantomData,
        })
    }

    /// Check a mutable value capability.  The mutable borrow ensures that two
    /// capabilities cannot be created from this region at the same time.
    pub fn check_mut<T>(&mut self, ptr: *mut T) -> Result<CheckedMut<'_, T>, PointerError> {
        Ok(CheckedMut {
            ptr: self
                .region
                .0
                .check(ptr, size_of::<T>(), Permissions::WRITE)?,
            _borrow: PhantomData,
        })
    }

    /// Return the read-only view of the region.
    pub const fn region(&self) -> &RamRegion<'a> {
        &self.region
    }
}

/// A checked read capability for ordinary RAM.
#[derive(Debug)]
pub struct RamPtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> RamPtr<'_, T> {
    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    /// Read the value at the checked address.
    ///
    /// # Safety
    ///
    /// The pointed-to bytes must remain mapped and contain a valid initialized
    /// `T`; no conflicting access may occur during the read.
    pub unsafe fn read(&self) -> T
    where
        T: Copy,
    {
        unsafe { self.ptr.as_ptr().read() }
    }

    /// Re-check the immutable capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::READ)
            .map(|_| ())
    }
}

/// A checked write capability for ordinary RAM.
#[derive(Debug)]
pub struct RamWritePtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> RamWritePtr<'_, T> {
    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Write a value at the checked address.
    ///
    /// # Safety
    ///
    /// No conflicting access may occur during the write, and the mapping must
    /// remain writable for the duration of the operation.
    pub unsafe fn write(&self, value: T) {
        unsafe { self.ptr.as_ptr().write(value) }
    }

    /// Re-check the mutable capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::WRITE)
            .map(|_| ())
    }
}

/// A mutable capability whose borrow is confined to a closure.
#[derive(Debug)]
pub struct CheckedMut<'r, T> {
    ptr: NonNull<T>,
    _borrow: PhantomData<&'r mut T>,
}

impl<T> CheckedMut<'_, T> {
    /// Operate on the checked value without allowing the reference to escape
    /// the capability's borrow.
    pub fn with_mut<R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        // `ExclusiveRamRegion::check_mut` establishes alignment, bounds,
        // write permission, and exclusive access before this value exists.
        f(unsafe { self.ptr.as_mut() })
    }

    /// Return the raw pointer for APIs which must remain explicitly unsafe.
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }
}

/// A checked contiguous RAM slice capability.
#[derive(Debug)]
pub struct SlicePtr<'a, T> {
    ptr: NonNull<T>,
    len: usize,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> SlicePtr<'_, T> {
    /// Number of elements in the checked slice.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the checked slice is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return the raw start pointer.
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    /// Read one element after checking its index.
    ///
    /// # Safety
    ///
    /// The selected element must contain an initialized, valid `T` and may
    /// not be concurrently accessed.
    pub unsafe fn read_at(&self, index: usize) -> Result<T, PointerError>
    where
        T: Copy,
    {
        if index >= self.len {
            return Err(PointerError::OutOfBounds);
        }
        self.region.check(
            unsafe { self.ptr.as_ptr().add(index) },
            size_of::<T>(),
            Permissions::READ,
        )?;
        Ok(unsafe { self.ptr.as_ptr().add(index).read() })
    }

    /// Re-check the whole slice against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check_slice(self.as_ptr(), self.len, Permissions::READ)
            .map(|_| ())
    }
}

/// Region restricted to volatile device-register access.
#[derive(Clone, Debug)]
pub struct MmioRegion<'a>(MemoryRegion<'a>);

/// Primitive register values for which every bit pattern is a valid value.
///
/// This is sealed so a downstream crate cannot accidentally make a type with
/// invalid bit patterns readable through a safe volatile load.
pub trait VolatileRead: Copy + sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

macro_rules! impl_volatile_read {
    ($($ty:ty),* $(,)?) => {
        $(
            impl sealed::Sealed for $ty {}
            impl VolatileRead for $ty {}
        )*
    };
}

impl_volatile_read!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

impl<'a> MmioRegion<'a> {
    /// Create a device-register region from a mapped address range.
    ///
    /// # Safety
    ///
    /// The range must be a live MMIO mapping for `'a`; the caller is also
    /// responsible for the device's register width and access protocol.
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self(unsafe {
            MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::Mmio)?
        }))
    }

    /// Create a device-register region from an address value.
    ///
    /// This is the address-based counterpart of [`Self::from_raw_parts`].
    /// Keeping addresses as integers prevents driver structs from storing or
    /// incrementing raw pointers.
    ///
    /// # Safety
    ///
    /// The address range must be a live MMIO mapping for `'a`.
    pub unsafe fn from_address(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        unsafe { Self::from_raw_parts(start, len, permissions) }
    }

    /// Check a volatile read at a byte offset within this region.
    pub fn check_read_at<T>(&self, offset: usize) -> Result<MmioPtr<'_, T>, PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        self.check_read(address as *const T)
    }

    /// Check a volatile write at a byte offset within this region.
    pub fn check_write_at<T>(&self, offset: usize) -> Result<MmioWritePtr<'_, T>, PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        self.check_write(address as *mut T)
    }

    /// Read a primitive register value at a byte offset.
    pub fn read_volatile_at<T: VolatileRead>(&self, offset: usize) -> Result<T, PointerError> {
        self.check_read_at(offset).map(|ptr| ptr.read_volatile())
    }

    /// Write a value to a register at a byte offset.
    pub fn write_volatile_at<T: Copy>(&self, offset: usize, value: T) -> Result<(), PointerError> {
        self.check_write_at(offset).map(|ptr| {
            ptr.write_volatile(value);
        })
    }

    /// Check a volatile read capability.
    pub fn check_read<T>(&self, ptr: *const T) -> Result<MmioPtr<'_, T>, PointerError> {
        Ok(MmioPtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::READ)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Check a volatile write capability.
    pub fn check_write<T>(&self, ptr: *mut T) -> Result<MmioWritePtr<'_, T>, PointerError> {
        Ok(MmioWritePtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::WRITE)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Return the underlying region metadata.
    pub const fn region(&self) -> &MemoryRegion<'a> {
        &self.0
    }
}

/// Region restricted to volatile framebuffer access.
#[derive(Clone, Debug)]
pub struct FramebufferRegion<'a>(MemoryRegion<'a>);

impl<'a> FramebufferRegion<'a> {
    /// Create a framebuffer region from a mapped virtual address range.
    ///
    /// # Safety
    ///
    /// The range must be mapped to the framebuffer for `'a`, and its size
    /// must cover every access made through the returned region.
    pub unsafe fn from_address(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self(unsafe {
            MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::Framebuffer)?
        }))
    }

    /// Write one framebuffer value using a volatile store.
    pub fn write_volatile_at<T: Copy>(&self, offset: usize, value: T) -> Result<(), PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        let ptr = self
            .0
            .check(address as *mut T, size_of::<T>(), Permissions::WRITE)?;
        unsafe { ptr.as_ptr().write_volatile(value) };
        Ok(())
    }

    /// Read one primitive framebuffer value using a volatile load.
    pub fn read_volatile_at<T: VolatileRead>(&self, offset: usize) -> Result<T, PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        let ptr = self
            .0
            .check(address as *const T, size_of::<T>(), Permissions::READ)?;
        Ok(unsafe { ptr.as_ptr().read_volatile() })
    }

    /// Return the mapped framebuffer base address as an integer.
    pub const fn start(&self) -> usize {
        self.0.start
    }

    /// Return the mapped framebuffer size in bytes.
    pub const fn len(&self) -> usize {
        self.0.len
    }

    /// Return whether the mapped framebuffer contains no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow the framebuffer as a mutable slice for a tightly audited bulk
    /// operation.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the region is exclusively owned and
    /// contains initialized `T` values for the requested length.
    pub unsafe fn as_mut_slice<T>(&mut self, len: usize) -> Result<&mut [T], PointerError> {
        let bytes = len
            .checked_mul(size_of::<T>())
            .ok_or(PointerError::LengthOverflow)?;
        let ptr = self
            .0
            .check(self.0.start as *mut T, bytes, Permissions::WRITE)?;
        Ok(unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) })
    }
}

/// A checked volatile MMIO read capability.
#[derive(Debug)]
pub struct MmioPtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> MmioPtr<'_, T> {
    /// Read the device register using a volatile load.
    ///
    /// # Safety
    ///
    /// The device must permit a volatile read of `T` at this register, and the
    /// mapping must remain live for the operation.
    pub fn read_volatile(&self) -> T
    where
        T: VolatileRead,
    {
        unsafe { self.ptr.as_ptr().read_volatile() }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::READ)
            .map(|_| ())
    }
}

/// A checked volatile MMIO write capability.
#[derive(Debug)]
pub struct MmioWritePtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> MmioWritePtr<'_, T> {
    /// Write the device register using a volatile store.
    ///
    /// # Safety
    ///
    /// The device must permit a volatile write of `T` at this register, and
    /// the mapping must remain live for the operation.
    pub fn write_volatile(&self, value: T) {
        unsafe { self.ptr.as_ptr().write_volatile(value) }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::WRITE)
            .map(|_| ())
    }
}

/// Region restricted to explicit user-memory copy operations.
#[derive(Clone, Debug)]
pub struct UserRegion<'a>(MemoryRegion<'a>);

impl<'a> UserRegion<'a> {
    /// Create a user-memory region from an already validated address-space
    /// mapping.  Page-table validation belongs in the caller.
    ///
    /// # Safety
    ///
    /// The range must be mapped in the user address space with the declared
    /// permissions for `'a`, and remain mapped while capabilities are used.
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self(unsafe {
            MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::User)?
        }))
    }

    /// Create a user-memory region from an address value.
    ///
    /// # Safety
    ///
    /// The address range must already have been validated by the owning
    /// address-space manager and remain mapped for `'a`.
    pub unsafe fn from_address(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        unsafe { Self::from_raw_parts(start, len, permissions) }
    }

    /// Copy bytes from the user region into kernel-owned storage.
    ///
    /// # Safety
    ///
    /// The user mapping must remain present and stable during the copy.
    pub unsafe fn copy_from_at(
        &self,
        offset: usize,
        destination: &mut [u8],
    ) -> Result<(), PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        let ptr = self
            .0
            .check(address as *const u8, destination.len(), Permissions::READ)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                destination.as_mut_ptr(),
                destination.len(),
            )
        };
        Ok(())
    }

    /// Copy kernel-owned bytes into the user region.
    ///
    /// # Safety
    ///
    /// The user mapping must remain present and writable during the copy, and
    /// no conflicting access may occur.
    pub unsafe fn copy_to_at(&self, offset: usize, source: &[u8]) -> Result<(), PointerError> {
        let address = self
            .0
            .start
            .checked_add(offset)
            .ok_or(PointerError::AddressOverflow)?;
        let ptr = self
            .0
            .check(address as *mut u8, source.len(), Permissions::WRITE)?;
        unsafe { core::ptr::copy_nonoverlapping(source.as_ptr(), ptr.as_ptr(), source.len()) };
        Ok(())
    }

    /// Check a user-space read capability.
    pub fn check_read<T>(&self, ptr: *const T) -> Result<UserPtr<'_, T>, PointerError> {
        Ok(UserPtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::READ)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Check a user-space write capability.
    pub fn check_write<T>(&self, ptr: *mut T) -> Result<UserPtrMut<'_, T>, PointerError> {
        Ok(UserPtrMut {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::WRITE)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }
}

/// A checked user-space read capability.
#[derive(Debug)]
pub struct UserPtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> UserPtr<'_, T> {
    /// Copy a value from user memory into kernel-owned memory.
    ///
    /// # Safety
    ///
    /// The user mapping must remain present and stable during the copy, and
    /// the bytes must contain an initialized, valid `T`.
    pub unsafe fn copy_from_user(&self) -> T
    where
        T: Copy,
    {
        unsafe { self.ptr.as_ptr().read() }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::READ)
            .map(|_| ())
    }
}

/// A checked user-space write capability.
#[derive(Debug)]
pub struct UserPtrMut<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> UserPtrMut<'_, T> {
    /// Copy a kernel-owned value into user memory.
    ///
    /// # Safety
    ///
    /// The user mapping must remain present and writable during the copy, and
    /// no conflicting access may occur.
    pub unsafe fn copy_to_user(&self, value: T) {
        unsafe { self.ptr.as_ptr().write(value) }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::WRITE)
            .map(|_| ())
    }
}

/// Region for DMA memory.  DMA synchronization is intentionally not guessed
/// by this crate; the platform's DMA owner must establish the appropriate
/// cache and ownership transition before using the raw operation methods.
#[derive(Debug)]
pub struct DmaRegion<'a>(MemoryRegion<'a>);

impl<'a> DmaRegion<'a> {
    /// Create a DMA region from a platform-validated mapping.
    ///
    /// # Safety
    ///
    /// The caller must guarantee the mapping and its DMA ownership rules.
    pub unsafe fn from_raw_parts(
        start: usize,
        len: usize,
        permissions: Permissions,
    ) -> Result<Self, RegionError> {
        Ok(Self(unsafe {
            MemoryRegion::from_raw_parts(start, len, permissions, RegionKind::Dma)?
        }))
    }

    /// Check a DMA read capability.
    pub fn check_read<T>(&self, ptr: *const T) -> Result<DmaPtr<'_, T>, PointerError> {
        Ok(DmaPtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::READ)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }

    /// Check a DMA write capability.
    pub fn check_write<T>(&self, ptr: *mut T) -> Result<DmaWritePtr<'_, T>, PointerError> {
        Ok(DmaWritePtr {
            ptr: self.0.check(ptr, size_of::<T>(), Permissions::WRITE)?,
            region: self.0.clone(),
            _lifetime: PhantomData,
        })
    }
}

/// A checked DMA read capability.
#[derive(Debug)]
pub struct DmaPtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> DmaPtr<'_, T> {
    /// Read DMA memory after the caller has completed the platform's required
    /// device-to-CPU synchronization.
    ///
    /// # Safety
    ///
    /// The mapping, initialization, synchronization, and ownership transition
    /// must all be valid for this access.
    pub unsafe fn read(&self) -> T
    where
        T: Copy,
    {
        unsafe { self.ptr.as_ptr().read() }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::READ)
            .map(|_| ())
    }
}

/// A checked DMA write capability.
#[derive(Debug)]
pub struct DmaWritePtr<'a, T> {
    ptr: NonNull<T>,
    region: MemoryRegion<'a>,
    _lifetime: PhantomData<&'a T>,
}

impl<T> DmaWritePtr<'_, T> {
    /// Write DMA memory after the caller has completed the platform's required
    /// CPU-to-device synchronization.
    ///
    /// # Safety
    ///
    /// The mapping, synchronization, and ownership transition must all be
    /// valid for this access.
    pub unsafe fn write(&self, value: T) {
        unsafe { self.ptr.as_ptr().write(value) }
    }

    /// Return the raw pointer for a narrowly scoped unsafe operation.
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Re-check the capability against its region metadata.
    pub fn validate(&self) -> Result<(), PointerError> {
        self.region
            .check(self.as_ptr(), size_of::<T>(), Permissions::WRITE)
            .map(|_| ())
    }
}
