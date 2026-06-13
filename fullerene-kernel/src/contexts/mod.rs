//! Unified Context System — replaces scattered statics with named structs.
//!
//! Architecture:
//! ```text
//! KernelContext (S)
//!  ├ boot: BootContext (A)
//!  ├ memory: MemoryContext (A) → {physical, virtual, dma, heap}
//!  ├ pci: PciContext
//!  ├ framebuffer: FramebufferContext
//!  ├ input: InputContext
//!  ├ window: WindowContext
//!  ├ audio: AudioContext (B)
//!  └ event: EventContext
//!
//! DeviceContext (S) — separate from kernel, holds driver state
//!  ├ pci: PciContext
//!  ├ ahci / nvme / hda / gpu
//!
//! DisplayContext (B)
//!  ├ framebuffer: FramebufferContext
//!  ├ windows: WindowContext
//!  └ cursor: CursorContext
//!
//! ProcessContext (C)
//!  ├ scheduler: SchedulerContext
//!  ├ tasks: TaskManagerContext
//!  └ process_table: ProcessTableContext
//!
//! InterruptContext (C)
//!  ├ idt: IdtContext
//!  ├ apic: ApicContext
//!  └ handlers: HandlerRegistryContext
//! ```

pub mod audio;
pub mod boot;
pub mod device;
pub mod display;
pub mod event;
pub mod framebuffer;
pub mod input;
pub mod interrupt;
pub mod kernel;
pub mod memory; // directory module
pub mod pci;
pub mod process;
pub mod window;

pub use audio::AudioContext;
pub use boot::BootContext;
pub use device::DeviceContext;
pub use display::DisplayContext;
pub use event::EventContext;
pub use framebuffer::FramebufferContext;
pub use input::InputContext;
pub use interrupt::InterruptContext;
pub use kernel::KernelContext;
pub use memory::MemoryContext;
pub use pci::PciContext;
pub use process::ProcessContext;
pub use window::WindowContext;
