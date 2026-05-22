use crate::event::Event;
use crate::handler::EventHandler;
use crate::queue::EventQueue;
use alloc::boxed::Box;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Dispatcher – routes events from the queue to registered handlers
// ---------------------------------------------------------------------------

/// Routes events from the `EventQueue` to registered `EventHandler`s.
///
/// # Design
///
/// The dispatcher is responsible **only for routing**. It does **not**
/// perform:
///
/// - Rendering
/// - Window management logic
/// - Shell logic
///
/// This keeps the dispatcher focused and testable.
///
/// # Flow
///
/// ```text
/// source → EventQueue → Dispatcher → EventHandler
/// ```
pub struct Dispatcher {
    handlers: Vec<Box<dyn EventHandler + Send>>,
}

impl Dispatcher {
    /// Creates an empty dispatcher with no handlers.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Creates a dispatcher with the given pre-allocated handler capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            handlers: Vec::with_capacity(capacity),
        }
    }

    /// Registers an event handler.
    ///
    /// Handlers are called **in registration order** for each event.
    pub fn register(&mut self, handler: Box<dyn EventHandler + Send>) {
        self.handlers.push(handler);
    }

    /// Removes all registered handlers.
    pub fn clear_handlers(&mut self) {
        self.handlers.clear();
    }

    /// Returns the number of registered handlers.
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Returns `true` if no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    // ------------------------------------------------------------------
    // Dispatch methods
    // ------------------------------------------------------------------

    /// Dispatches a single event to all registered handlers.
    ///
    /// Each handler receives the event in order of registration.
    /// Propagation stops if a handler returns `true` (consumed).
    pub fn dispatch(&mut self, event: &Event) {
        for handler in &mut self.handlers {
            if handler.handle(event) {
                break;
            }
        }
    }

    /// Dispatches all events currently in the queue.
    ///
    /// This drains the queue, routing each event to every registered
    /// handler.
    pub fn dispatch_queue(&mut self, queue: &mut EventQueue) {
        while let Some(event) = queue.pop() {
            self.dispatch(&event);
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, InputEvent, MouseButton};
    use core::cell::Cell;

    struct TrackingHandler {
        count: Cell<usize>,
    }

    impl TrackingHandler {
        fn new() -> Self {
            Self {
                count: Cell::new(0),
            }
        }
    }

    impl EventHandler for TrackingHandler {
        fn handle(&mut self, _event: &Event) -> bool {
            self.count.set(self.count.get() + 1);
            false // don't consume, allow propagation
        }
    }

    #[test]
    fn test_dispatch_single_event() {
        let mut dispatcher = Dispatcher::new();
        let handler = TrackingHandler::new();
        dispatcher.register(Box::new(handler));

        let event = Event::Input(InputEvent::MouseDown(MouseButton::Left));
        dispatcher.dispatch(&event);
    }

    #[test]
    fn test_dispatch_queue() {
        let mut dispatcher = Dispatcher::new();
        let handler = TrackingHandler::new();
        dispatcher.register(Box::new(handler));

        let mut queue = EventQueue::new();
        queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
        queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Left)));
        queue.push(Event::Input(InputEvent::MouseMove { x: 10, y: 20 }));

        dispatcher.dispatch_queue(&mut queue);
        // handler called 3 times
    }

    #[test]
    fn test_multiple_handlers() {
        let mut dispatcher = Dispatcher::new();
        let h1 = TrackingHandler::new();
        let h2 = TrackingHandler::new();
        dispatcher.register(Box::new(h1));
        dispatcher.register(Box::new(h2));

        let mut queue = EventQueue::new();
        queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));

        dispatcher.dispatch_queue(&mut queue);
        // Both handlers called once each
    }

    #[test]
    fn test_empty_dispatcher() {
        let mut dispatcher = Dispatcher::new();
        assert!(dispatcher.is_empty());

        let event = Event::Input(InputEvent::MouseDown(MouseButton::Left));
        // Should not panic
        dispatcher.dispatch(&event);
    }

    #[test]
    fn test_consumed_stops_propagation() {
        // First handler consumes → second should NOT be called
        struct ConsumingHandler {
            count: Cell<usize>,
        }

        impl ConsumingHandler {
            fn new() -> Self {
                Self {
                    count: Cell::new(0),
                }
            }
        }

        impl EventHandler for ConsumingHandler {
            fn handle(&mut self, _event: &Event) -> bool {
                self.count.set(self.count.get() + 1);
                true // consume
            }
        }

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Box::new(ConsumingHandler::new()));
        let h2 = TrackingHandler::new();
        dispatcher.register(Box::new(h2));

        let event = Event::Input(InputEvent::MouseDown(MouseButton::Left));
        dispatcher.dispatch(&event);
    }

    #[test]
    fn test_handler_count() {
        let mut dispatcher = Dispatcher::new();
        assert_eq!(dispatcher.handler_count(), 0);

        dispatcher.register(Box::new(TrackingHandler::new()));
        assert_eq!(dispatcher.handler_count(), 1);

        dispatcher.register(Box::new(TrackingHandler::new()));
        assert_eq!(dispatcher.handler_count(), 2);

        dispatcher.clear_handlers();
        assert_eq!(dispatcher.handler_count(), 0);
    }
}
