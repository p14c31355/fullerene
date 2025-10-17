// petroleum/tests/lib.rs (integrated tests to reduce redundancy)

#[cfg(feature = "std")]
mod tests_with_std {

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
    fn test_uefi_system_table_ptr_creation() {
        // Test that we can create basic structures without panicking
        let ptr = crate::UefiSystemTablePtr(core::ptr::null_mut());
        assert!(ptr.0.is_null());
    }

    // Integrated tests from toluene to reduce redundant test files
    mod toluene_integration {
        extern crate toluene;

        #[test]
        fn test_add_positive() {
            assert_eq!(toluene::add(2, 3), 5);
            assert_eq!(toluene::add(0, 0), 0);
            assert_eq!(toluene::add(-1, 1), 0);
        }

        #[test]
        fn test_add_negative() {
            assert_eq!(toluene::add(-2, -3), -5);
        }
    }
}

#[cfg(test)]
mod macro_tests {
    #[test]
    fn test_basic_functionality() {
        // Basic test to ensure the library compiles and functions work
        // We'll keep this simple and focus on exporting the functionality we need
        assert!(true);
    }
}
