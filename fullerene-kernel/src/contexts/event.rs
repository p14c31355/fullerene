//! EventContext — centralized event bus bridging kernel contexts.
//!
//! Re-exports `resonance` event types and provides a kernel-global
//! event queue + dispatcher.  InputContext routes raw PS/2 events
//! through here instead of duplicating polling in solvent.
//!
//! # Flow
//!
//! ```text
//! PS/2 IRQ → InputContext.poll() → EventContext.queue
//!                                   ↓
//!                         Dispatcher.dispatch()
//!                          ├→ WmEventHandler  (solvent)
//!                          ├→ ShellEventHandler
//!                          └→ AudioEventHandler (future)
//! ```
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use resonance::{Dispatcher, Event, EventHandler, EventQueue};
use spin::Mutex;

/// Maximum number of pending events before oldest are dropped.
const MAX_EVENTS: usize = 256;

/// Kernel-global event context.
pub struct EventContext {
    pub queue: EventQueue,
    pub dispatcher: Dispatcher,
    /// Pending system-level events that cross-cut contexts.
    pub system_queue: VecDeque<Event>,
}

impl EventContext {
    pub fn new() -> Self {
        Self {
            queue: EventQueue::with_capacity(MAX_EVENTS),
            dispatcher: Dispatcher::new(),
            system_queue: VecDeque::with_capacity(64),
        }
    }

    pub fn register_handler(&mut self, handler: Box<dyn EventHandler + Send>) {
        self.dispatcher.register(handler);
    }

    /// Push an event into the queue for dispatch.
    pub fn push(&mut self, event: Event) {
        self.queue.push(event);
    }

    /// Push a system-level event (cross-cutting).
    pub fn push_system(&mut self, event: Event) {
        self.system_queue.push_back(event);
    }

    /// Drain and dispatch all pending events.
    pub fn process(&mut self) {
        self.dispatcher.dispatch_queue(&mut self.queue);
        while let Some(event) = self.system_queue.pop_front() {
            self.dispatcher.dispatch(&event);
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty() || !self.system_queue.is_empty()
    }
}

static EVENT_CTX: Mutex<Option<EventContext>> = Mutex::new(None);

pub fn init_event() {
    *EVENT_CTX.lock() = Some(EventContext::new());
}

pub fn get_event() -> &'static Mutex<Option<EventContext>> {
    &EVENT_CTX
}

pub fn with_event_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut EventContext) -> R,
{
    EVENT_CTX.lock().as_mut().map(f)
}

pub fn with_event<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&EventContext) -> R,
{
    EVENT_CTX.lock().as_ref().map(f)
}