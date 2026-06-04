use crate::event::Event;
use alloc::collections::VecDeque;

#[derive(Clone, Debug, Default)]
pub struct EventQueue {
    queue: VecDeque<Event>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, event: Event) {
        self.queue.push_back(event);
    }

    pub fn push_front(&mut self, event: Event) {
        self.queue.push_front(event);
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn drain_all(&mut self) -> alloc::collections::vec_deque::Drain<'_, Event> {
        self.queue.drain(..)
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

impl IntoIterator for EventQueue {
    type Item = Event;
    type IntoIter = alloc::collections::vec_deque::IntoIter<Event>;

    fn into_iter(self) -> Self::IntoIter {
        self.queue.into_iter()
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
        assert_eq!(eq.into_iter().count(), 2);
    }

    #[test]
    fn test_clear() {
        let mut eq = EventQueue::new();
        eq.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
        eq.clear();
        assert!(eq.is_empty());
    }
}