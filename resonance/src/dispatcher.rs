use crate::event::Event;
use crate::handler::EventHandler;
use crate::queue::EventQueue;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// Routes events from the `EventQueue` to registered `EventHandler`s.
///
/// The dispatcher is responsible **only for routing**.
/// It does not perform rendering, WM logic, or shell logic.
///
/// Flow: `source → EventQueue → Dispatcher → EventHandler`
#[derive(Default)]
pub struct Dispatcher {
    handlers: Vec<Box<dyn EventHandler + Send>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            handlers: Vec::with_capacity(capacity),
        }
    }

    pub fn register(&mut self, handler: Box<dyn EventHandler + Send>) {
        self.handlers.push(handler);
    }

    pub fn clear_handlers(&mut self) {
        self.handlers.clear();
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    pub fn dispatch(&mut self, event: &Event) {
        for handler in &mut self.handlers {
            if handler.handle(event) {
                break;
            }
        }
    }

    pub fn dispatch_queue(&mut self, queue: &mut EventQueue) {
        while let Some(event) = queue.pop() {
            self.dispatch(&event);
        }
    }
}

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
            false
        }
    }

    #[test]
    fn test_dispatch_single_event() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Box::new(TrackingHandler::new()));
        dispatcher.dispatch(&Event::Input(InputEvent::MouseDown(MouseButton::Left)));
    }

    #[test]
    fn test_dispatch_queue() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Box::new(TrackingHandler::new()));

        let mut queue = EventQueue::new();
        queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
        queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Left)));
        queue.push(Event::Input(InputEvent::MouseMove { x: 10, y: 20 }));
        dispatcher.dispatch_queue(&mut queue);
    }

    #[test]
    fn test_multiple_handlers() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Box::new(TrackingHandler::new()));
        dispatcher.register(Box::new(TrackingHandler::new()));

        let mut queue = EventQueue::new();
        queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
        dispatcher.dispatch_queue(&mut queue);
    }

    #[test]
    fn test_empty_dispatcher() {
        let mut dispatcher = Dispatcher::new();
        assert!(dispatcher.is_empty());
        dispatcher.dispatch(&Event::Input(InputEvent::MouseDown(MouseButton::Left)));
    }

    #[test]
    fn test_consumed_stops_propagation() {
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
                true
            }
        }

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Box::new(ConsumingHandler::new()));
        dispatcher.register(Box::new(TrackingHandler::new()));
        dispatcher.dispatch(&Event::Input(InputEvent::MouseDown(MouseButton::Left)));
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
