//! User Space Memory Validation Functions
//!
//! This module provides functions for validating user space memory access and managing inter-process memory operations.

use super::*;

/// Helper functions for user space memory validation
pub mod user_space {
    use super::*;



    /// Map a user page for kernel access
    pub fn map_user_page(
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            manager.page_table_manager.map_page(
                virtual_addr,
                physical_addr,
                flags,
                &mut manager.frame_allocator,
            )
        } else {
            Err(SystemError::InternalError)
        }
    }


}

// Re-export functions for easier access
pub use user_space::map_user_page;
pub use petroleum::{is_user_address, validate_user_buffer};
