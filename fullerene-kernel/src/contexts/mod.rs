//! Unified Context System — replaces scattered statics with named structs.
//!
//! All contexts use `define_context!` macro for zero-boilerplate singleton management.
//! KernelContext bundles all sub-contexts for single-lock access.

#[macro_use]
pub mod macros;

pub mod audio;
pub mod boot;
pub mod event;
pub mod framebuffer;
pub mod input;
pub mod kernel;
pub mod memory;
pub mod pci;
pub mod window;

pub use audio::AudioContext;
pub use boot::BootContext;
pub use event::EventContext;
pub use framebuffer::FramebufferContext;
pub use input::InputContext;
pub use kernel::KernelContext;
pub use memory::MemoryContext;
pub use pci::PciContext;
pub use window::WindowContext;
