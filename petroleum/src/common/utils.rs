use spin::Mutex;

pub const COM1_DATA_PORT: u16 = 0x3F8;
pub const COM1_STATUS_PORT: u16 = 0x3FD;

/// Calculate offset address in loops (base + i * 4096)
pub fn calculate_offset_address(base: u64, i: u64) -> u64 {
    base + (i * 4096)
}

/// Calculates the pointer to the i-th descriptor.
///
/// # Safety
///
/// The caller must ensure that the resulting pointer is within the bounds of the allocated object.
pub unsafe fn calculate_descriptor_ptr(ptr: *const u8, index: usize, size: usize) -> *const u8 {
    unsafe { ptr.add(index * size) }
}

/// Calculates the end address of a memory region given start address and page count.
pub fn calculate_region_end(start: u64, pages: u64) -> u64 {
    start + (pages * 4096)
}

/// Calculates the pointer to metadata appended at the end of a buffer.
///
/// # Safety
///
/// The caller must ensure that the resulting pointer is within the bounds of the allocated object.
pub unsafe fn calculate_metadata_ptr(
    base: *const u8,
    total_size: usize,
    metadata_size: usize,
) -> *const u8 {
    unsafe { base.add(total_size - metadata_size) }
}

/// Calculates the number of pages needed to cover a given size, rounding up.
pub fn calculate_pages(size: usize) -> u64 {
    size.div_ceil(4096) as u64
}

/// Sign-extend a 48-bit virtual address to 64 bits.
///
/// On x86_64, virtual addresses are 48-bit sign-extended to 64 bits.
/// This function performs the sign extension: if bit 47 is 1, the upper 16 bits
/// are set to 1; otherwise they are set to 0.
#[inline]
pub fn sign_extend_virt_addr(addr: u64) -> u64 {
    if (addr & (1 << 47)) != 0 {
        addr | 0xFFFF_0000_0000_0000
    } else {
        addr & 0x0000_FFFF_FFFF_FFFF
    }
}

/// Force reset a Mutex lock state to 0.
///
/// # Safety
/// This is a highly unsafe operation that should only be used during early boot
/// to handle cases where .bss is not cleared.
pub unsafe fn reset_mutex_lock<T>(mutex: &Mutex<T>) {
    unsafe {
        // A spin::Mutex has the lock state (AtomicBool or similar) at the beginning of the struct.
        // Use addr_of! to get the address without creating a reference, avoiding
        // invalid_reference_casting lint.
        // SAFETY: The caller guarantees this is a static Mutex that hasn't been locked yet,
        // and writing 0 to the lock byte is safe during early boot initialization.
        let lock_ptr = core::ptr::addr_of!(*mutex).cast::<u32>().cast_mut();
        core::ptr::write_volatile(lock_ptr, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_offset_address() {
        assert_eq!(calculate_offset_address(0x1000, 0), 0x1000);
        assert_eq!(calculate_offset_address(0x1000, 1), 0x2000);
        assert_eq!(calculate_offset_address(0x1000, 2), 0x3000);
        assert_eq!(calculate_offset_address(0, 10), 10 * 4096);
    }

    #[test]
    fn test_calculate_pages() {
        assert_eq!(calculate_pages(0), 0);
        assert_eq!(calculate_pages(1), 1);
        assert_eq!(calculate_pages(4096), 1);
        assert_eq!(calculate_pages(4097), 2);
        assert_eq!(calculate_pages(8192), 2);
    }
}
