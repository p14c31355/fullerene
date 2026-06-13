//! InterruptContext — IDT, APIC, handler registry.
//!
//! Aggregates the interrupt-related subsystems that were previously
//! scattered across `interrupts/` submodules.

use spin::Mutex;

/// IDT (Interrupt Descriptor Table) context.
pub struct IdtContext {
    /// Whether the IDT has been loaded.
    pub loaded: bool,
    /// Number of exception vectors registered.
    pub exception_handlers: usize,
}

impl IdtContext {
    pub const fn new() -> Self {
        Self {
            loaded: false,
            exception_handlers: 0,
        }
    }
}

/// APIC context (I/O APIC, Local APIC).
pub struct ApicContext {
    /// Whether the APIC has been initialised.
    pub initialized: bool,
    /// Whether the I/O APIC is in use (vs legacy 8259 PIC).
    pub io_apic_active: bool,
}

impl ApicContext {
    pub const fn new() -> Self {
        Self {
            initialized: false,
            io_apic_active: false,
        }
    }
}

/// Registered interrupt handler count (keyboard, mouse, timer, etc.)
pub struct HandlerRegistryContext {
    /// Number of hardware interrupt handlers registered.
    pub hardware_handlers: usize,
    /// Number of software interrupt / syscall handlers registered.
    pub software_handlers: usize,
}

impl HandlerRegistryContext {
    pub const fn new() -> Self {
        Self {
            hardware_handlers: 0,
            software_handlers: 0,
        }
    }
}

/// Aggregated interrupt context.
pub struct InterruptContext {
    pub idt: IdtContext,
    pub apic: ApicContext,
    pub handlers: HandlerRegistryContext,
}

// InterruptContext lives behind a Mutex.
unsafe impl Send for InterruptContext {}
unsafe impl Sync for InterruptContext {}

impl InterruptContext {
    pub const fn new() -> Self {
        Self {
            idt: IdtContext::new(),
            apic: ApicContext::new(),
            handlers: HandlerRegistryContext::new(),
        }
    }

    /// True when the interrupt subsystem is fully initialised.
    pub fn is_ready(&self) -> bool {
        self.idt.loaded && self.apic.initialized
    }
}

// ── Global singleton ──────────────────────────────────────────
static INTR: Mutex<Option<InterruptContext>> = Mutex::new(None);

pub fn init_interrupt() {
    *INTR.lock() = Some(InterruptContext::new());
}

pub fn get_interrupt() -> &'static Mutex<Option<InterruptContext>> {
    &INTR
}

pub fn with_interrupt_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut InterruptContext) -> R,
{
    INTR.lock().as_mut().map(f)
}

pub fn with_interrupt<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&InterruptContext) -> R,
{
    INTR.lock().as_ref().map(f)
}