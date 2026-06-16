use crate::event::Event;
use alloc::collections::VecDeque;
use core::ops::{Deref, DerefMut};

#[derive(Clone, Debug, Default)]
pub struct EventQueue(VecDeque<Event>);

impl Deref for EventQueue {
    type Target = VecDeque<Event>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for EventQueue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self(VecDeque::with_capacity(capacity))
    }
    pub fn pop(&mut self) -> Option<Event> {
        self.0.pop_front()
    }
    pub fn push(&mut self, event: Event) {
        self.0.push_back(event);
    }
    pub fn drain_all(&mut self) -> alloc::collections::vec_deque::Drain<'_, Event> {
        self.0.drain(..)
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
        let count = eq.drain(..).count();
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
