//! PS/2 shims: the ANE has no PS/2 controller.
//!
//! Same names as the x86 tree so the shared desktop input code compiles;
//! every call reports "no input".

pub mod keyboard;
pub mod mouse;
