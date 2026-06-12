//! Unified Context System for Fullerene Kernel
//!
//! This module consolidates scattered global/static variables into named
//! context structs.  Each context owns its data and exposes methods that
//! operate on it, reducing the number of globals each function touches
//! and lowering cognitive load for both humans and LLMs.
//!
//! # Design principle (from Assembly.rs)
//!
//! ```text
//! Context → 処理 （関数はコンテキストを受け取るだけ）
//! ```
//!
//! # Contexts
//!
//! | Context              | Wraps                                              |
//! |----------------------|----------------------------------------------------|
//! | [`BootContext`]      | framebuffer info, memory map, RSDP, kernel args     |
//! | [`FramebufferContext`] | framebuffer base, width, height, stride, format    |
//! | [`PciContext`]       | PCI device list, find by class/vendor               |
//! | [`InputContext`]     | unified input event queue (keyboard + mouse)         |
//! | [`WindowContext`]    | windows, cursor, focus, z-order                     |
//! | [`AudioContext`]     | HDA controller state, DMA buffers                   |
//! | [`MemoryContext`]    | page table, physical/virtual allocators             |

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