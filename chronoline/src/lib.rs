#![no_std]

extern crate alloc;

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// ClockSource – clock source abstraction
// ---------------------------------------------------------------------------

/// Trait for abstracting hardware timers (PIT, HPET, APIC timer, TSC deadline, etc.).
pub trait ClockSource {
    /// Returns the current tick count.
    fn now_ticks(&self) -> u64;

    /// Creates a `Deadline` that fires `delta` ticks from the current time.
    ///
    /// This is the preferred way to create deadlines — it keeps deadline
    /// generation close to the clock, making TSC / APIC deadline integration
    /// more natural.
    fn deadline_after(&self, delta: u64) -> Deadline {
        Deadline::new(self.now_ticks().saturating_add(delta))
    }
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
// TimerMode – timer firing mode
// ---------------------------------------------------------------------------

/// How a timer behaves after expiring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerMode {
    /// Fire once and be removed.
    OneShot,
    /// Automatically re‑register with the same interval after firing.
    Repeating { interval_ticks: u64 },
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
///
/// Sorting only considers `deadline` and `id` (not `mode`), so timers with
/// different modes but identical deadline/id are treated the same for ordering.
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

    /// Registers a timer event with `OneShot` mode (default for backward compat).
    ///
    /// After insertion the list is sorted by deadline (full sort in v1).
    pub fn register(&mut self, deadline: Deadline, id: TimerId) {
        self.register_with_mode(deadline, id, TimerMode::OneShot);
    }

    /// Registers a timer event with an explicit [`TimerMode`].
    ///
    /// After insertion the list is sorted by deadline (full sort in v1).
    pub fn register_with_mode(&mut self, deadline: Deadline, id: TimerId, mode: TimerMode) {
        let timer = Timer { deadline, id, mode };
        let index = self.timers.binary_search(&timer).unwrap_or_else(|e| e);
        self.timers.insert(index, timer);
    }

    /// Advances the internal clock by `delta` ticks.
    ///
    /// This is the preferred way to update the clock from a scheduler
    /// that maintains its own monotonic counter — it avoids the risk of
    /// stale or non‑monotonic absolute ticks.
    pub fn advance(&mut self, delta: u64) {
        self.now = self.now.saturating_add(delta);
    }

    /// Advances the internal clock to `now`.
    ///
    /// Prefer [`advance`](Self::advance) for scheduler use; `tick` is
    /// kept for cases where an absolute clock source is unavoidable.
    pub fn tick(&mut self, now: u64) {
        if now > self.now {
            self.now = now;
        }
    }

    /// Pops the next expired timer, if any.
    ///
    /// Because timers are sorted by deadline, only the first element needs
    /// to be checked.
    ///
    /// **For repeating timers:** the timer is automatically re‑registered
    /// with its interval before being returned. This means the caller does
    /// NOT need to manually re‑register cursor blink / periodic timers.
    pub fn pop_expired(&mut self) -> Option<Timer> {
        if self
            .timers
            .first()
            .is_some_and(|t| t.deadline.ticks() <= self.now)
        {
            let timer = self.pop_front_expired();

            // Re‑register repeating timers before returning
            if let TimerMode::Repeating { interval_ticks } = timer.mode {
                let new_deadline = Deadline::new(self.now.saturating_add(interval_ticks));
                self.register_with_mode(new_deadline, timer.id, timer.mode);
            }

            Some(timer)
        } else {
            None
        }
    }

    // ── private helpers ──────────────────────────────────────

    /// Remove and return the front timer.
    fn pop_front_expired(&mut self) -> Timer {
        self.timers.remove(0)
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
            mode: TimerMode::OneShot,
        };
        let t2 = Timer {
            deadline: Deadline(50),
            id: TimerId(2),
            mode: TimerMode::OneShot,
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

    #[test]
    fn test_repeating_timer() {
        let mut cl = ChronoLine::new();
        let interval = 10;

        // Register a repeating timer
        cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: interval },
        );
        assert_eq!(cl.len(), 1);

        // Expire at t=10 → should fire and re‑register
        cl.tick(10);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(1));
        assert_eq!(cl.len(), 1); // still 1 because it re‑registered

        // Tick past next deadline (20)
        cl.tick(20);
        let expired = cl.pop_expired().unwrap();
        assert_eq!(expired.id, TimerId(1));
        assert_eq!(cl.len(), 1); // still repeating

        // Cancel
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
        cl.register(Deadline::new(15), TimerId(2)); // OneShot (default)

        cl.tick(20);

        // Should pop TimerId(1) first (deadline 10)
        let t1 = cl.pop_expired().unwrap();
        assert_eq!(t1.id, TimerId(1));

        // Should pop TimerId(2) next (deadline 15)
        let t2 = cl.pop_expired().unwrap();
        assert_eq!(t2.id, TimerId(2));

        // TimerId(1) should have re‑registered, so it should still be in the list
        assert_eq!(cl.len(), 1);
        // next deadline is now + interval = 20 + 10 = 30
        assert_eq!(cl.next_deadline(), Some(Deadline(30)));

        // Advance to expire it again
        cl.tick(30);
        let t1_again = cl.pop_expired().unwrap();
        assert_eq!(t1_again.id, TimerId(1));
        assert_eq!(cl.len(), 1);
    }
}