//! UEFI memory map processing.

pub mod descriptor;
pub mod processor;
pub mod validator;

// Re-export commonly used items for backward compatibility
pub use descriptor::*;
pub use processor::*;
pub use validator::MemoryDescriptorValidator;
