//! Unified Context System — replaces scattered statics with named structs.
pub mod audio;
pub mod boot;
pub mod framebuffer;
pub mod input;
pub mod memory;
pub mod pci;
pub mod window;

pub use audio::AudioContext;
pub use boot::BootContext;
pub use framebuffer::FramebufferContext;
pub use input::InputContext;
pub use memory::MemoryContext;
pub use pci::PciContext;
pub use window::WindowContext;
