//! Paging-related macros.

/// Map a region with debug logging and error handling.
///
/// # Usage
/// ```ignore
/// safe_map!(mapper, virt => phys, size, flags);
/// ```
///
/// Expands to a mapping call with optional debug output and
/// error logging to serial.
#[macro_export]
macro_rules! safe_map {
    ($mapper:expr, $virt:expr => $phys:expr, $size:expr, $flags:expr) => {{
        #[cfg(feature = "debug_pf")]
        $crate::serial_println!(
            "map: {:x} -> {:x} ({} KiB, flags={:x})",
            $virt,
            $phys,
            $size / 1024,
            $flags
        );
        $mapper
            .map_region(
                $crate::page_table::types::CanonicalVirtAddr::new($virt)
                    .expect("non-canonical virtual address"),
                $phys,
                $size,
            )
            .with_flags($flags)
            .huge_if_possible()
            .apply()
            .map_err(|e| {
                #[cfg(feature = "debug_pf")]
                $crate::serial_println!("map FAILED: {:?} (virt={:x} phys={:x})", e, $virt, $phys);
                e
            })
    }};
}

/// Map a region with explicit 4 KiB pages (no huge page optimization).
#[macro_export]
macro_rules! safe_map_4k {
    ($mapper:expr, $virt:expr => $phys:expr, $size:expr, $flags:expr) => {{
        #[cfg(feature = "debug_pf")]
        $crate::serial_println!(
            "map_4k: {:x} -> {:x} ({} KiB)",
            $virt,
            $phys,
            $size / 1024
        );
        $mapper
            .map_region(
                $crate::page_table::types::CanonicalVirtAddr::new($virt)
                    .expect("non-canonical virtual address"),
                $phys,
                $size,
            )
            .with_flags($flags)
            .apply()
    }};
}

/// Assert that an address is aligned to the given boundary.
#[macro_export]
macro_rules! assert_aligned {
    ($addr:expr, $align:expr) => {
        debug_assert!(
            $addr % $align == 0,
            "address 0x{:x} not aligned to 0x{:x}",
            $addr,
            $align
        );
    };
}

/// Assert that an address is canonical.
#[macro_export]
macro_rules! assert_canonical {
    ($addr:expr) => {
        debug_assert!(
            $crate::page_table::types::CanonicalVirtAddr::new($addr).is_some(),
            "address 0x{:x} is not canonical",
            $addr
        );
    };
}

// Re-export for use within the crate
pub use assert_aligned;
pub use assert_canonical;
pub use safe_map;
pub use safe_map_4k;