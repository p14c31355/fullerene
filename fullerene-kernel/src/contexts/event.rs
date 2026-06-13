//! EventContext — centralized event bus bridging kernel contexts.
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use resonance::{Dispatcher, Event, EventHandler, EventQueue};

const MAX_EVENTS: usize = 256;

pub struct EventContext {
    pub queue: EventQueue, pub dispatcher: Dispatcher, pub system_queue: VecDeque<Event>,
}

impl EventContext {
    pub fn new() -> Self { Self { queue:EventQueue::with_capacity(MAX_EVENTS),dispatcher:Dispatcher::new(),system_queue:VecDeque::with_capacity(64) } }
    pub fn register_handler(&mut self, handler: Box<dyn EventHandler+Send>) { self.dispatcher.register(handler); }
    pub fn push(&mut self, event: Event) { self.queue.push(event); }
    pub fn push_system(&mut self, event: Event) { self.system_queue.push_back(event); }
    pub fn process(&mut self) { self.dispatcher.dispatch_queue(&mut self.queue); while let Some(event)=self.system_queue.pop_front() { self.dispatcher.dispatch(&event); } }
    pub fn has_pending(&self) -> bool { !self.queue.is_empty()||!self.system_queue.is_empty() }
}

crate::define_context!(EventContext, event, EVENT_CTX);