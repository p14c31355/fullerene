#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub trait ClockSource {
    fn now_ticks(&self) -> u64;

    fn deadline_after(&self, delta: u64) -> Deadline {
        Deadline::new(self.now_ticks().saturating_add(delta))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Deadline(u64);

impl Deadline {
    pub const fn new(ticks: u64) -> Self {
        Self(ticks)
    }
    pub const fn ticks(&self) -> u64 {
        self.0
    }
    pub fn from_now(clock: &impl ClockSource, delta: u64) -> Self {
        Self(clock.now_ticks().saturating_add(delta))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerMode {
    OneShot,
    Repeating { interval_ticks: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TimerId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    pub deadline: Deadline,
    pub id: TimerId,
    pub mode: TimerMode,
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.deadline
            .cmp(&other.deadline)
            .then_with(|| self.id.cmp(&other.id))
    }
}

#[derive(Default)]
pub struct ChronoLine {
    timers: Vec<Timer>,
    now: u64,
}

impl ChronoLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, deadline: Deadline, id: TimerId) {
        self.register_with_mode(deadline, id, TimerMode::OneShot);
    }

    pub fn register_with_mode(&mut self, deadline: Deadline, id: TimerId, mode: TimerMode) {
        let timer = Timer { deadline, id, mode };
        let index = self.timers.binary_search(&timer).unwrap_or_else(|e| e);
        self.timers.insert(index, timer);
    }

    pub fn advance(&mut self, delta: u64) {
        self.now = self.now.saturating_add(delta);
    }

    /// Set the current time to the given absolute tick value.
    ///
    /// Note: This only moves time forward. If the clock source resets
    /// (e.g. wraps to 0), call `reset_now(0)` before resuming `tick()`.
    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
    }

    /// Reset the current time to `now`, even if it moves backwards.
    /// Use when the underlying clock source has wrapped around.
    pub fn reset_now(&mut self, now: u64) {
        self.now = now;
    }

    pub fn pop_expired(&mut self) -> Option<Timer> {
        if self.first_expired() {
            let timer = self.timers.remove(0);
            if let TimerMode::Repeating { interval_ticks } = timer.mode {
                let new_deadline = Deadline::new(self.now.saturating_add(interval_ticks));
                self.register_with_mode(new_deadline, timer.id, timer.mode);
            }
            Some(timer)
        } else {
            None
        }
    }

    pub fn has_expired(&self) -> bool {
        self.first_expired()
    }

    fn first_expired(&self) -> bool {
        self.timers
            .first()
            .is_some_and(|t| t.deadline.ticks() <= self.now)
    }

    pub fn len(&self) -> usize {
        self.timers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }
    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn cancel(&mut self, id: TimerId) -> bool {
        let len = self.timers.len();
        self.timers.retain(|t| t.id != id);
        self.timers.len() < len
    }

    pub fn next_deadline(&self) -> Option<Deadline> {
        self.timers.first().map(|t| t.deadline)
    }

    pub fn clear(&mut self) {
        self.timers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[derive(Default)]
    pub struct FakeClock {
        now: Cell<u64>,
    }

    impl FakeClock {
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
        let clock = FakeClock::default();

        cl.register(Deadline::from_now(&clock, 10), TimerId(1));
        cl.register(Deadline::from_now(&clock, 5), TimerId(2));
        assert_eq!(cl.len(), 2);

        cl.tick(3);
        assert!(cl.pop_expired().is_none());

        cl.tick(5);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(2));
        assert_eq!(cl.len(), 1);

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
        let t1 = Timer {
            deadline: Deadline(100),
            id: TimerId(1),
            mode: TimerMode::OneShot,
        };
        let t2 = Timer {
            deadline: Deadline(50),
            id: TimerId(2),
            mode: TimerMode::OneShot,
        };
        assert!(t2 < t1);

        let mut cl = ChronoLine::new();
        cl.register(Deadline(100), TimerId(1));
        cl.register(Deadline(50), TimerId(2));
        cl.tick(200);
        let first = cl.pop_expired().unwrap();
        assert_eq!(first.id, TimerId(2));
    }

    #[test]
    fn test_multiple_expired_in_order() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1));
        cl.register(Deadline(5), TimerId(2));
        cl.register(Deadline(20), TimerId(3));
        cl.tick(20);

        assert_eq!(cl.pop_expired().unwrap().id, TimerId(2));
        assert_eq!(cl.pop_expired().unwrap().id, TimerId(1));
        assert_eq!(cl.pop_expired().unwrap().id, TimerId(3));
        assert!(cl.is_empty());
    }

    #[test]
    fn test_repeating_timer() {
        let mut cl = ChronoLine::new();
        let interval = 10;

        cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating {
                interval_ticks: interval,
            },
        );
        assert_eq!(cl.len(), 1);

        cl.tick(10);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(1));
        assert_eq!(cl.len(), 1);

        cl.tick(20);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(1));
        assert_eq!(cl.len(), 1);

        assert!(cl.cancel(TimerId(1)));
        assert!(cl.is_empty());
    }

    #[test]
    fn test_repeating_oneshot_mixed() {
        let mut cl = ChronoLine::new();

        cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 10 },
        );
        cl.register(Deadline::new(15), TimerId(2));

        cl.tick(20);

        let t1 = cl.pop_expired().unwrap();
        assert_eq!(t1.id, TimerId(1));
        let t2 = cl.pop_expired().unwrap();
        assert_eq!(t2.id, TimerId(2));

        assert_eq!(cl.len(), 1);
        assert_eq!(cl.next_deadline(), Some(Deadline(30)));

        cl.tick(30);
        let t1_again = cl.pop_expired().unwrap();
        assert_eq!(t1_again.id, TimerId(1));
        assert_eq!(cl.len(), 1);
    }
}
