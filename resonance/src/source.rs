use crate::event::Event;

// ---------------------------------------------------------------------------
// EventSource trait
// ---------------------------------------------------------------------------

/// Trait for event sources — entities that produce external events.
///
/// # Design
///
/// Any hardware or software component that generates events implements this
/// trait:
///
/// - PS/2 mouse / keyboard
/// - USB HID
/// - VirtIO input
/// - Hardware timer (→ `TimerEvent`)
/// - Network
///
/// `poll()` is called by the dispatch loop to collect events from all
/// registered sources.
///
/// # Example
///
/// ```ignore
/// struct MouseDriver { ... }
///
/// impl EventSource for MouseDriver {
///     fn poll(&mut self) -> Option<Event> {
///         // Read hardware, return an event if available
///     }
/// }
/// ```
pub trait EventSource {
    /// Polls the source for a single event.
    ///
    /// Returns `Some(Event)` if an event is available, or `None` if the
    /// source has no pending events.
    fn poll(&mut self) -> Option<Event>;
}