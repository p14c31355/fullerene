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
    use crate::*;
    use crate::common::macros::*;
    use crate::common::logging::SystemError;

    #[test]
    fn test_logging_macros() {
        // Test that macros compile correctly
        log::error!("Test error");
        log::warn!("Test warning");
        log::info!("Test info");
        log::debug!("Test debug");
        log::trace!("Test trace");
    }

    #[test]
    fn test_utility_macros() {
        // Test ensure macro
        let result: Result<(), &crate::SystemError> = (|| {
            ensure!(true, &SystemError::InvalidArgument);
            Ok(())
        })();
        assert!(result.is_ok());

        // Test ensure_with_msg macro
        let result: Result<(), &crate::SystemError> = (|| {
            ensure_with_msg!(false, &SystemError::InvalidArgument, "Test message");
            Ok(())
        })();
        assert!(result.is_err());

        // Test option_to_result macro
        let some_value = Some(42);
        let none_value: Option<i32> = None;

        assert_eq!(
            option_to_result!(some_value, &SystemError::FileNotFound),
            Ok(42)
        );
        assert!(matches!(
            option_to_result!(none_value, &SystemError::FileNotFound),
            Err(crate::SystemError::FileNotFound)
        ));
    }
}
