//! Unified Context System — replaces scattered statics with named structs.
//!
//! Every subsystem context lives inside [`KernelContext`], the single source of
//! truth for kernel state; all access goes through `with_kernel(…)`.
//!
//! Exception: `vfs` is self-owned. `contexts/vfs.rs` holds its own singleton
//! (`VFS_CONTEXT` static + `init_vfs()`/`with_vfs()`), so both kernel roots —
//! the x86/UEFI one and the aarch64 one — compile and use the same owner
//! without dragging the kernel lock through every VFS call.
//!
//! # Adding a new context
//!
//! 1. Create `contexts/<name>.rs` with a `pub struct <Name>Context { … }`
//! 2. Add `pub mod <name>;` and `pub use <name>::<Name>Context;` below
//! 3. Add a `pub <name>: <Name>Context` field to `KernelContext` in `kernel.rs`
//!
//! The `define_context!` macro in `macros.rs` is kept for reference but
//! is no longer the recommended pattern — new code should use
//! `KernelContext` aggregation.

#[macro_use]
pub mod macros;

pub mod audio;
pub mod boot;
pub mod event;
pub mod framebuffer;
pub mod gui;
pub mod input;
pub mod kernel;
pub mod memory;
pub mod pci;
pub mod settings;
pub mod settings_persist;
pub mod shell;
pub mod vfs;
pub mod window;

pub use audio::AudioContext;
pub use boot::BootContext;
pub use event::EventContext;
pub use framebuffer::FramebufferContext;
pub use gui::GuiContext;
pub use input::InputContext;
pub use kernel::KernelContext;
pub use memory::MemoryContext;
pub use pci::PciContext;
pub use shell::ShellContext;
pub use vfs::VfsContext;
pub use window::WindowContext;
