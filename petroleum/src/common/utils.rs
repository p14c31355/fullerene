/// Calculate offset address in loops (base + i * 4096)
pub fn calculate_offset_address(base: u64, i: u64) -> u64 {
    base + (i * 4096)
}

/// Calculates the number of pages needed for a buffer including metadata overhead.
pub fn calculate_pages_for_buffer(buffer_size: usize) -> usize {
    (buffer_size + core::mem::size_of::<usize>())
        .div_ceil(4096)
        .max(1)
}

/// Calculates the data pointer offset from the physical address (skipping metadata).
pub fn calculate_map_data_ptr(phys_addr: usize) -> usize {
    phys_addr + core::mem::size_of::<usize>()
}

/// Calculates the offset where the configuration should be appended to the memory map.
pub fn calculate_config_offset(map_size: usize) -> usize {
    core::mem::size_of::<usize>() + map_size
}

/// Checks if adding a configuration block exceeds the allocated buffer size.
pub fn check_buffer_overflow(phys_addr: usize, config_offset: usize, config_size: usize, buffer_size: usize) -> bool {
    let total_capacity = buffer_size + core::mem::size_of::<usize>();
    (config_offset + config_size) <= total_capacity
}

/// Calculates the pointer to the i-th descriptor.
pub fn calculate_descriptor_ptr(ptr: *const u8, index: usize, size: usize) -> *const u8 {
    unsafe { ptr.add(index * size) }
}

/// Calculates the end address of a memory region given start address and page count.
pub fn calculate_region_end(start: u64, pages: u64) -> u64 {
    start + (pages * 4096)
}

/// Calculates the pointer to metadata appended at the end of a buffer.
pub fn calculate_metadata_ptr(base: *const u8, total_size: usize, metadata_size: usize) -> *const u8 {
    unsafe { base.add(total_size - metadata_size) }
}

/// Calculates the number of pages needed to cover a given size, rounding up.
pub fn calculate_pages(size: usize) -> u64 {
    ((size + 4095) / 4096) as u64
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
    fn test_calculate_pages_for_buffer() {
        // 128KB + 8 bytes = 131080 bytes. 131080 / 4096 = 32.0019 -> 33 pages
        assert_eq!(calculate_pages_for_buffer(128 * 1024), 33);
        // Very small buffer
        assert_eq!(calculate_pages_for_buffer(0), 1);
    }

    #[test]
    fn test_calculate_map_data_ptr() {
        assert_eq!(calculate_map_data_ptr(0x1000), 0x1000 + core::mem::size_of::<usize>());
    }

    #[test]
    fn test_calculate_config_offset() {
        assert_eq!(calculate_config_offset(1024), core::mem::size_of::<usize>() + 1024);
    }

    #[test]
    fn test_check_buffer_overflow() {
        let phys = 0x1000;
        let buffer_size = 128 * 1024;
        let metadata = core::mem::size_of::<usize>();
        
        // Case 1: Fits
        let offset = 100;
        let size = 100;
        assert!(check_buffer_overflow(phys, offset, size, buffer_size));

        // Case 2: Overflows
        let offset = buffer_size + metadata + 1;
        let size = 1;
        assert!(!check_buffer_overflow(phys, offset, size, buffer_size));
    }

    #[test]
    fn test_calculate_descriptor_ptr() {
        let ptr = 0x1000 as *const u8;
        let size = 40;
        assert_eq!(calculate_descriptor_ptr(ptr, 0, size), 0x1000 as *const u8);
        assert_eq!(calculate_descriptor_ptr(ptr, 1, size), 0x1028 as *const u8);
        assert_eq!(calculate_descriptor_ptr(ptr, 2, size), 0x1050 as *const u8);
    }

    #[test]
    fn test_calculate_region_end() {
        assert_eq!(calculate_region_end(0x1000, 1), 0x2000);
        assert_eq!(calculate_region_end(0x1000, 0), 0x1000);
        assert_eq!(calculate_region_end(0, 10), 10 * 4096);
    }

    #[test]
    fn test_calculate_metadata_ptr() {
        let base = 0x1000 as *const u8;
        let size = 100;
        let meta_size = 20;
        assert_eq!(calculate_metadata_ptr(base, size, meta_size), (0x1000 + 80) as *const u8);
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
