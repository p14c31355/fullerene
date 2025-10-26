//! System error types and conversions - DEPRECATED
//!
//! This module is deprecated. Use `petroleum::common::logging::SystemError` instead.
//! This file is kept for backward compatibility during migration.

// Re-export from petroleum for backward compatibility
pub use petroleum::common::logging::{SystemError, SystemResult};

// Explicit From implementations
