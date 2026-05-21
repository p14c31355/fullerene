use crate::event::Event;

// ---------------------------------------------------------------------------
// EventHandler trait
// ---------------------------------------------------------------------------

/// Trait for handling events dispatched from the `Resonance` event system.
///
/// # Design
///
/// Any subsystem that needs to receive events implements this trait:
///
/// - `Lattice` (compositor / WM)
/// - `Nozzle` (terminal / shell)
/// - Future applications
///
/// # Important
///
/// Handlers should **not** perform long-running work synchronously. If
/// expensive processing is needed, the handler should enqueue work elsewhere
/// and return quickly.
pub trait EventHandler {
    /// Handle a single event.
    ///
    /// The event is passed by shared reference — events are **immutable**
    /// throughout their lifecycle.
    fn handle(&mut self, event: &Event) -> bool;
}