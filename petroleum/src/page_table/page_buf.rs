//! Page-backed buffer — physically allocated, direct-mapped pixel/byte storage.
//!
//! [`PageBuf<T>`] allocates contiguous physical pages from the frame allocator
//! and exposes them as a `&[T]` / `&mut [T]` slice through the kernel's direct
//! physical mapping (`physical_to_virtual`).  No page-table manipulation is
//! needed because physical memory is pre-mapped in the higher-half direct-map
//! region at boot.
//!
//! On drop the physical frames are returned to the frame allocator, completely
//! bypassing the kernel heap (linked_list_allocator).  This is the primary
//! mechanism for keeping large allocations (back buffer, image decode buffers)
//! out of the 36 MiB kernel heap.

// ── Real implementation (kernel, no_std) ─────────────────────

#[cfg(not(any(feature = "std", test)))]
mod inner {
    use core::marker::PhantomData;
    use core::mem;

    pub struct PageBuf<T> {
        phys_start: u64,
        pages: usize,
        len: usize,
        _phantom: PhantomData<T>,
    }

    impl<T> PageBuf<T> {
        /// Allocate a buffer for `len` elements, zeroed.
        ///
        /// Returns `None` if the frame allocator cannot satisfy the request.
        ///
        /// # Safety
        ///
        /// `T` must be OK to represent as all-zero bytes (i.e. no
        /// `NonNull` / reference fields that would be invalid as null).
        pub unsafe fn alloc_zeroed_for_len(len: usize) -> Option<Self> {
            let bytes = len.checked_mul(mem::size_of::<T>())?;
            let pages = bytes.div_ceil(4096);
            if pages == 0 {
                return Some(Self {
                    phys_start: 0,
                    pages: 0,
                    len,
                    _phantom: PhantomData,
                });
            }
            let phys = crate::page_table::constants::with_frame_allocator(|fa| {
                fa.allocate_contiguous_frames(pages)
            })
            .ok()?;
            let virt = crate::common::memory::physical_to_virtual(phys as usize);
            unsafe { core::ptr::write_bytes(virt as *mut u8, 0, pages * 4096) };
            Some(Self {
                phys_start: phys,
                pages,
                len,
                _phantom: PhantomData,
            })
        }

        #[inline]
        pub fn as_slice(&self) -> &[T] {
            if self.pages == 0 {
                return &[];
            }
            let virt = crate::common::memory::physical_to_virtual(self.phys_start as usize);
            unsafe { core::slice::from_raw_parts(virt as *const T, self.len) }
        }

        #[inline]
        pub fn as_mut_slice(&mut self) -> &mut [T] {
            if self.pages == 0 {
                return &mut [];
            }
            let virt = crate::common::memory::physical_to_virtual(self.phys_start as usize);
            unsafe { core::slice::from_raw_parts_mut(virt as *mut T, self.len) }
        }

        #[inline]
        pub fn len(&self) -> usize {
            self.len
        }

        #[inline]
        pub fn capacity(&self) -> usize {
            self.pages * 4096 / mem::size_of::<T>()
        }
    }

    impl<T> Drop for PageBuf<T> {
        fn drop(&mut self) {
            if self.pages > 0 {
                let _ = crate::page_table::constants::with_frame_allocator(|fa| {
                    fa.free_contiguous_frames(self.phys_start, self.pages)
                });
            }
        }
    }
}

// ── Test / std implementation ────────────────────────────────

#[cfg(any(feature = "std", test))]
mod inner {
    use alloc::vec::Vec;

    pub struct PageBuf<T> {
        vec: Vec<T>,
    }

    impl<T> PageBuf<T> {
        /// Allocate a buffer for `len` elements, zeroed.
        ///
        /// # Safety
        ///
        /// `T` must be OK to represent as all-zero bytes.
        pub unsafe fn alloc_zeroed_for_len(len: usize) -> Option<Self> {
            let byte_len = len.checked_mul(core::mem::size_of::<T>())?;
            let mut vec: Vec<T> = Vec::with_capacity(len);
            unsafe {
                core::ptr::write_bytes(vec.as_mut_ptr(), 0, byte_len);
                vec.set_len(len);
            }
            Some(Self { vec })
        }

        #[inline]
        pub fn as_slice(&self) -> &[T] {
            &self.vec
        }

        #[inline]
        pub fn as_mut_slice(&mut self) -> &mut [T] {
            &mut self.vec
        }

        #[inline]
        pub fn len(&self) -> usize {
            self.vec.len()
        }

        #[inline]
        pub fn capacity(&self) -> usize {
            self.vec.capacity()
        }
    }
}

pub use inner::PageBuf;

// ── Common trait impls (available in both modes) ──────────────

impl<T> core::ops::Deref for PageBuf<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> core::ops::DerefMut for PageBuf<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
