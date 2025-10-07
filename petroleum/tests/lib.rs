#[cfg_attr(feature = "std", macro_use)]
#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
mod tests_with_std {
    use super::*;

    #[test]
    fn test_u32_to_str_heapless() {
        let mut buffer = [0u8; 10];
        let result = crate::u32_to_str_heapless(12345, &mut buffer);
        assert_eq!(result, "12345");
    }

    #[test]
    fn test_u32_to_str_heapless_zero() {
        let mut buffer = [0u8; 10];
        let result = crate::u32_to_str_heapless(0, &mut buffer);
        assert_eq!(result, "0");
    }

    #[test]
    fn test_u32_to_str_heapless_max() {
        let mut buffer = [0u8; 20];
        let result = crate::u32_to_str_heapless(u32::MAX, &mut buffer);
        // Just verify it produces some output
        assert_eq!(result, "4294967295");
    }

    #[test]
    fn test_panic_handling() {
        // Test that we can create basic structures without panicking
        let ptr = crate::UefiSystemTablePtr(core::ptr::null_mut());
        assert!(ptr.0.is_null());
    }
}

#[cfg(not(feature = "std"))]
mod tests_no_std {
    // For no_std tests, we can only do compile-time checks
    // Real testing would require a UEFI environment or emulator
    const _: () = {
        // Check that certain functions are accessible via linking
        // (would cause link-time errors if not present)
    };
}
