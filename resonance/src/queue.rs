use alloc::collections::VecDeque;
use crate::event::Event;

// ---------------------------------------------------------------------------
// EventQueue – single-queue v0 implementation
// ---------------------------------------------------------------------------

/// A simple, typed event queue backed by `VecDeque<Event>`.
///
/// # Design
///
/// - v0: single FIFO queue
/// - v1: priority queue (`input > redraw > background`)
/// - v2: targeted events (`Event` carries a `WindowId`)
/// - v3: subscriptions
/// - v4: async wake integration
///
/// Events are **immutable** — they flow through the system as
/// `create → queue → consume → drop`.
#[derive(Clone, Debug)]
pub struct EventQueue {
    queue: VecDeque<Event>,
}

impl EventQueue {
    /// Creates an empty event queue.
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Creates an event queue with the given pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
        }
    }

    /// Pushes an event to the **back** of the queue (FIFO).
    pub fn push(&mut self, event: Event) {
        self.queue.push_back(event);
    }

    /// Pushes an event to the **front** of the queue (urgent / high-priority).
    ///
    /// Useful for high-priority events (e.g. input) in v1+.
    pub fn push_front(&mut self, event: Event) {
        self.queue.push_front(event);
    }

    /// Pops an event from the **front** of the queue.
    ///
    /// Returns `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<Event> {
        self.queue.pop_front()
    }

    /// Returns the number of events currently in the queue.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Returns `true` if the queue contains no events.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Drains all events from the queue and returns them as a `Vec`.
    pub fn drain_all(&mut self) -> alloc::vec::Vec<Event> {
        self.queue.drain(..).collect()
    }

    /// Clears all events from the queue.
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Iterator support: drain the queue
// ---------------------------------------------------------------------------

/// An iterator that drains the `EventQueue`.
pub struct IntoIter(alloc::collections::vec_deque::IntoIter<Event>);

impl Iterator for IntoIter {
    type Item = Event;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

impl IntoIterator for EventQueue {
    type Item = Event;
    type IntoIter = IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter(self.queue.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, InputEvent, MouseButton};

    #[test]
    fn test_push_and_pop() {
        let mut eq = EventQueue::new();
        assert!(eq.is_empty());

        let ev = Event::Input(InputEvent::MouseDown(MouseButton::Left));
        eq.push(ev.clone());
        assert_eq!(eq.len(), 1);

        assert_eq!(eq.pop(), Some(ev));
        assert!(eq.is_empty());
    }

    #[test]
    fn test_push_front() {
        let mut eq = EventQueue::new();
        let ev1 = Event::Input(InputEvent::MouseDown(MouseButton::Left));
        let ev2 = Event::Input(InputEvent::MouseUp(MouseButton::Left));

        eq.push(ev1);
        eq.push_front(ev2.clone());

        // ev2 was pushed front, so it should come out first
        assert_eq!(eq.pop(), Some(ev2));
    }

    #[test]
    fn test_drain_all() {
        let mut eq = EventQueue::with_capacity(4);
        eq.push(Event::Input(InputEvent::MouseMove { x: 10, y: 20 }));
        eq.push(Event::Input(InputEvent::MouseDown(MouseButton::Right)));
        assert_eq!(eq.drain_all().len(), 2);
        assert!(eq.is_empty());
    }

    #[test]
    fn test_into_iter() {
        let mut eq = EventQueue::new();
        eq.push(Event::Input(InputEvent::MouseMove { x: 1, y: 2 }));
        eq.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));

        let count = eq.into_iter().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_clear() {
        let mut eq = EventQueue::new();
        eq.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
        eq.clear();
        assert!(eq.is_empty());
    }
}