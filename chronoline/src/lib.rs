#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::cmp::Ordering;

// ---------------------------------------------------------------------------
// ClockSource – clock source abstraction
// ---------------------------------------------------------------------------

/// Trait for abstracting hardware timers (PIT, HPET, APIC timer, TSC deadline, etc.).
pub trait ClockSource {
    /// Returns the current tick count.
    fn now_ticks(&self) -> u64;
}

// ---------------------------------------------------------------------------
// Deadline – tick-count wrapper
// ---------------------------------------------------------------------------

/// A thin wrapper around a tick count.
/// Derives `Ord` so it can be used directly for timer sorting.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Deadline(u64);

impl Deadline {
    /// Creates a `Deadline` from a raw tick value.
    pub const fn new(ticks: u64) -> Self {
        Self(ticks)
    }

    /// Returns the inner tick value.
    pub fn ticks(&self) -> u64 {
        self.0
    }

    /// Creates a `Deadline` that fires `delta` ticks from the clock's current time.
    pub fn from_now(clock: &impl ClockSource, delta: u64) -> Self {
        Self(clock.now_ticks().saturating_add(delta))
    }
}

// ---------------------------------------------------------------------------
// TimerId – timer identifier
// ---------------------------------------------------------------------------

/// Uniquely identifies a timer event.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TimerId(pub u64);

// ---------------------------------------------------------------------------
// Timer – timer event
// ---------------------------------------------------------------------------

/// A single timer event.
///
/// Holds no callback; the scheduler calls `pop_expired()` and dispatches
/// the event on its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timer {
    pub deadline: Deadline,
    pub id: TimerId,
}

// ---------------------------------------------------------------------------
// ChronoLine – time management primitive
// ---------------------------------------------------------------------------

/// Manages timer events — a time management primitive, **not** a scheduler.
///
/// # Design
///
/// - **Knows nothing about schedulers** – `ChronoLine` is a primitive used
///   *by* a scheduler, not the other way around.
/// - Internally holds `Vec<Timer>` sorted by deadline (ascending).
///   `pop_expired()` removes from the front (v1: linear scan).
/// - `#![no_std]` + `alloc` compatible.
///
/// # Future evolution
///
/// - v2: `BinaryHeap<Reverse<Timer>>`
/// - v3: Timer wheel
/// - v4: Hierarchical timing wheel
pub struct ChronoLine {
    timers: Vec<Timer>,
    now: u64,
}

impl ChronoLine {
    /// Creates an empty `ChronoLine`.
    pub fn new() -> Self {
        Self {
            timers: Vec::new(),
            now: 0,
        }
    }

    /// Registers a timer event.
    ///
    /// After insertion the list is sorted by deadline (full sort in v1).
    pub fn register(&mut self, deadline: Deadline, id: TimerId) {
        let timer = Timer { deadline, id };
        let index = self.timers.binary_search(&timer).unwrap_or_else(|e| e);
        self.timers.insert(index, timer);
    }

    /// Advances the internal clock to `now`.
    pub fn tick(&mut self, now: u64) {
        if now > self.now {
            self.now = now;
        }
    }

    /// Pops the next expired timer, if any.
    ///
    /// Because timers are sorted by deadline, only the first element needs
    /// to be checked.
    pub fn pop_expired(&mut self) -> Option<Timer> {
        if self
            .timers
            .first()
            .is_some_and(|t| t.deadline.ticks() <= self.now)
        {
            Some(self.timers.remove(0))
        } else {
            None
        }
    }

    /// Returns `true` if at least one timer has expired (without removing it).
    pub fn has_expired(&self) -> bool {
        self.timers
            .first()
            .is_some_and(|t| t.deadline.ticks() <= self.now)
    }

    /// Returns the number of registered timers.
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    /// Returns `true` if no timers are registered.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// Returns the current tick value.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Cancels all timers matching the given `TimerId`.
    ///
    /// Returns `true` if at least one timer was removed.
    pub fn cancel(&mut self, id: TimerId) -> bool {
        let len = self.timers.len();
        self.timers.retain(|t| t.id != id);
        self.timers.len() < len
    }

    /// Returns the next deadline, or `None` if no timers are registered.
    pub fn next_deadline(&self) -> Option<Deadline> {
        self.timers.first().map(|t| t.deadline)
    }

    /// Removes all timers.
    pub fn clear(&mut self) {
        self.timers.clear();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    /// A fake clock for testing — time can be advanced arbitrarily with `advance()`.
    pub struct FakeClock {
        now: Cell<u64>,
    }

    impl FakeClock {
        pub fn new() -> Self {
            Self {
                now: Cell::new(0),
            }
        }

        #[allow(dead_code)]
        pub fn advance(&self, delta: u64) {
            self.now.set(self.now.get() + delta);
        }
    }

    impl ClockSource for FakeClock {
        fn now_ticks(&self) -> u64 {
            self.now.get()
        }
    }

    #[test]
    fn test_register_and_expire() {
        let mut cl = ChronoLine::new();
        let clock = FakeClock::new();

        cl.register(Deadline::from_now(&clock, 10), TimerId(1));
        cl.register(Deadline::from_now(&clock, 5), TimerId(2));
        assert_eq!(cl.len(), 2);

        // Nothing expired yet
        cl.tick(3);
        assert!(cl.pop_expired().is_none());

        // Past TimerId(2)'s deadline (=5)
        cl.tick(5);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(2));
        assert_eq!(cl.len(), 1);

        // Past TimerId(1)'s deadline (=10)
        cl.tick(10);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(1));
        assert!(cl.is_empty());
    }

    #[test]
    fn test_cancel() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1));
        cl.register(Deadline(20), TimerId(2));

        assert!(cl.cancel(TimerId(1)));
        assert_eq!(cl.len(), 1);

        // Cancelling a non-existent ID returns false
        assert!(!cl.cancel(TimerId(3)));
    }

    #[test]
    fn test_next_deadline() {
        let mut cl = ChronoLine::new();
        assert!(cl.next_deadline().is_none());

        cl.register(Deadline(100), TimerId(1));
        assert_eq!(cl.next_deadline(), Some(Deadline(100)));
    }

    #[test]
    fn test_has_expired() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1));
        assert!(!cl.has_expired());

        cl.tick(10);
        assert!(cl.has_expired());
    }

    #[test]
    fn test_clear() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1));
        cl.register(Deadline(20), TimerId(2));
        assert_eq!(cl.len(), 2);

        cl.clear();
        assert!(cl.is_empty());
    }

    #[test]
    fn test_ordering() {
        // Verify Timer sorts by deadline ascending
        let t1 = Timer {
            deadline: Deadline(100),
            id: TimerId(1),
        };
        let t2 = Timer {
            deadline: Deadline(50),
            id: TimerId(2),
        };

        // Deadline(50) < Deadline(100) → t2 < t1
        assert!(t2 < t1);

        // Sorted order should be [t2, t1]
        let mut cl = ChronoLine::new();
        cl.register(Deadline(100), TimerId(1));
        cl.register(Deadline(50), TimerId(2));

        cl.tick(200);
        let first = cl.pop_expired().unwrap();
        assert_eq!(first.id, TimerId(2)); // TimerId(2) expires first
    }

    #[test]
    fn test_multiple_expired_in_order() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1));
        cl.register(Deadline(5), TimerId(2));
        cl.register(Deadline(20), TimerId(3));

        cl.tick(20);

        // Must pop in deadline order
        assert_eq!(cl.pop_expired().unwrap().id, TimerId(2));
        assert_eq!(cl.pop_expired().unwrap().id, TimerId(1));
        assert_eq!(cl.pop_expired().unwrap().id, TimerId(3));
        assert!(cl.is_empty());
    }
}