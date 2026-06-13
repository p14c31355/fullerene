//! KernelContext — top-level aggregation of all subsystem contexts.
//!
//! Replaces scattered `with_*()` calls with a single `kernel.audio.play()`
//! style access, similar to `petroleum::assembly::Assembly` philosophy.
//!
//! # Access
//!
//! ```ignore
//! kernel_context().with(|k| {
//!     k.audio.play(...);
//!     k.window.create(...);
//!     k.memory.allocate(...);
//! });
//! ```

use super::audio::AudioContext;
use super::boot::BootContext;
use super::event::EventContext;
use super::framebuffer::FramebufferContext;
use super::input::InputContext;
use super::memory::MemoryContext;
use super::pci::PciContext;
use super::window::WindowContext;

use spin::Mutex;

/// Top-level kernel context holding every subsystem.
///
/// When this struct exists, all `with_audio()`, `with_memory()`, …
/// calls can be replaced by `kernel.audio`, `kernel.memory`, …
pub struct KernelContext {
    pub boot: BootContext,
    pub memory: MemoryContext,
    pub pci: PciContext,
    pub framebuffer: FramebufferContext,
    pub input: InputContext,
    pub window: WindowContext,
    pub audio: AudioContext,
    pub event: EventContext,
}

// KernelContext is only stored behind a Mutex; interior mutability is
// already provided by each sub-context.  We just need Send+Sync for
// the static holder.
unsafe impl Send for KernelContext {}
unsafe impl Sync for KernelContext {}

impl KernelContext {
    /// Create a fresh KernelContext with every sub-context in its "uninitialised" state.
    ///
    /// Individual sub-contexts must still be initialised via their respective
    /// `init_*` helpers (e.g. `init_audio()`, `init_memory()`) before use.
    pub fn new() -> Self {
        Self {
            boot: BootContext::empty(),
            memory: MemoryContext::new(),
            pci: PciContext::new(),
            framebuffer: FramebufferContext::new(),
            input: InputContext::new(),
            window: WindowContext::new(),
            audio: AudioContext::new(),
            event: EventContext::new(),
        }
    }

    /// Convenience: probe + scan all discoverable hardware.
    pub fn discover_devices(&mut self) {
        // PCI scan already populates self.pci.devices
        self.audio.probe();
    }

    /// True when the framebuffer (GOP or VGA) is ready for output.
    pub fn display_ready(&self) -> bool {
        self.framebuffer.is_available()
    }

    /// True when the memory manager has been set up.
    pub fn memory_ready(&self) -> bool {
        self.memory.is_ready()
    }
}

// ── Global singleton ──────────────────────────────────────────
static KERNEL: Mutex<Option<KernelContext>> = Mutex::new(None);

/// Initialise the global KernelContext.
///
/// Called from `init_common()` instead of the 8 individual `init_*()`
/// calls.  After this, all sub-contexts are accessible via
/// `kernel_context().with(…)`.
pub fn init_kernel() {
    // Check if already initialized (idempotent)
    if KERNEL.lock().is_some() {
        return;
    }
    let mut k = KernelContext::new();
    // PCI scan is mandatory early.
    let _ = k.pci.scan();
    *KERNEL.lock() = Some(k);
}

/// Get a direct reference to the global `Mutex<Option<KernelContext>>`.
pub fn get_kernel() -> &'static Mutex<Option<KernelContext>> {
    &KERNEL
}

/// Execute a closure over the KernelContext.
pub fn with_kernel<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&KernelContext) -> R,
{
    KERNEL.lock().as_ref().map(f)
}

/// Execute a mutable closure over the KernelContext.
pub fn with_kernel_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut KernelContext) -> R,
{
    KERNEL.lock().as_mut().map(f)
}
