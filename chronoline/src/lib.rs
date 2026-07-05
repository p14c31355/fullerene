#![no_std]

extern crate alloc;

use alloc::collections::BinaryHeap;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerPolicy {
    FixedRate,
    FixedDelay,
}

impl Default for TimerPolicy {
    fn default() -> Self {
        TimerPolicy::FixedDelay
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegisterError {
    ZeroInterval,
    TimerFull,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TimerId(pub u64);

#[derive(Clone, Copy)]
pub struct Timer {
    pub deadline: Deadline,
    pub id: TimerId,
    pub mode: TimerMode,
    pub policy: TimerPolicy,
    pub missed_ticks: u64,
    pub max_catch_up: u64,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.id == other.id
    }
}

impl Eq for Timer {}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub struct ChronoLine {
    timers: BinaryHeap<Timer>,
    now: u64,
    next_id: u64,
    max_catch_up: u64,
}

impl Default for ChronoLine {
    fn default() -> Self {
        Self {
            timers: BinaryHeap::new(),
            now: 0,
            next_id: 0,
            max_catch_up: u64::MAX,
        }
    }
}

impl ChronoLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_max_catch_up(&mut self, limit: u64) {
        self.max_catch_up = limit;
    }

    pub fn register_auto(&mut self, deadline: Deadline) -> Result<TimerId, RegisterError> {
        let id = TimerId(self.next_id);
        self.next_id += 1;
        self.register(deadline, id)
    }

    pub fn register(&mut self, deadline: Deadline, id: TimerId) -> Result<TimerId, RegisterError> {
        self.register_with_mode(deadline, id, TimerMode::OneShot)
    }

    pub fn register_with_mode(
        &mut self,
        deadline: Deadline,
        id: TimerId,
        mode: TimerMode,
    ) -> Result<TimerId, RegisterError> {
        self.register_with_mode_and_policy(deadline, id, mode, TimerPolicy::default())
    }

    pub fn register_with_mode_and_policy(
        &mut self,
        deadline: Deadline,
        id: TimerId,
        mode: TimerMode,
        policy: TimerPolicy,
    ) -> Result<TimerId, RegisterError> {
        if let TimerMode::Repeating { interval_ticks } = mode {
            if interval_ticks == 0 {
                return Err(RegisterError::ZeroInterval);
            }
        }
        self.timers.push(Timer {
            deadline,
            id,
            mode,
            policy,
            missed_ticks: 0,
            max_catch_up: self.max_catch_up,
        });
        Ok(id)
    }

    pub fn advance(&mut self, delta: u64) {
        self.now = self.now.saturating_add(delta);
    }

    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
    }

    pub fn reset_now(&mut self, now: u64) {
        self.now = now;
    }

    pub fn pop_expired(&mut self) -> Option<Timer> {
        if !self.has_expired() {
            return None;
        }
        let mut timer = self.timers.pop().unwrap();

        if let TimerMode::Repeating { interval_ticks } = timer.mode {
            let elapsed = self.now.saturating_sub(timer.deadline.ticks());
            let missed = (elapsed / interval_ticks).saturating_add(1);
            timer.missed_ticks = missed.min(timer.max_catch_up);

            let new_deadline = match timer.policy {
                TimerPolicy::FixedRate => Deadline::new(
                    timer
                        .deadline
                        .ticks()
                        .saturating_add(interval_ticks.saturating_mul(missed)),
                ),
                TimerPolicy::FixedDelay => Deadline::new(self.now.saturating_add(interval_ticks)),
            };

            self.timers.push(Timer {
                deadline: new_deadline,
                id: timer.id,
                mode: timer.mode,
                policy: timer.policy,
                missed_ticks: 0,
                max_catch_up: timer.max_catch_up,
            });
        }

        Some(timer)
    }

    pub fn has_expired(&self) -> bool {
        self.timers
            .peek()
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
        // O(n) in-place cancellation using into_vec() + retain() + heapify.
        // Avoids the O(n log n) old approach of alloc::vec + filter + BinaryHeap::from.
        let len_before = self.timers.len();
        let mut vec = core::mem::take(&mut self.timers).into_vec();
        vec.retain(|t| t.id != id);
        self.timers = BinaryHeap::from(vec);
        self.timers.len() != len_before
    }

    pub fn next_deadline(&self) -> Option<Deadline> {
        self.timers.peek().map(|t| t.deadline)
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

        cl.register(Deadline::from_now(&clock, 10), TimerId(1))
            .unwrap();
        cl.register(Deadline::from_now(&clock, 5), TimerId(2))
            .unwrap();
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
        cl.register(Deadline(10), TimerId(1)).unwrap();
        cl.register(Deadline(20), TimerId(2)).unwrap();

        assert!(cl.cancel(TimerId(1)));
        assert_eq!(cl.len(), 1);
        assert!(!cl.cancel(TimerId(3)));
    }

    #[test]
    fn test_next_deadline() {
        let mut cl = ChronoLine::new();
        assert!(cl.next_deadline().is_none());
        cl.register(Deadline(100), TimerId(1)).unwrap();
        assert_eq!(cl.next_deadline(), Some(Deadline(100)));
    }

    #[test]
    fn test_has_expired() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1)).unwrap();
        assert!(!cl.has_expired());
        cl.tick(10);
        assert!(cl.has_expired());
    }

    #[test]
    fn test_clear() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1)).unwrap();
        cl.register(Deadline(20), TimerId(2)).unwrap();
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
            policy: TimerPolicy::FixedDelay,
            missed_ticks: 0,
            max_catch_up: 0,
        };
        let t2 = Timer {
            deadline: Deadline(50),
            id: TimerId(2),
            mode: TimerMode::OneShot,
            policy: TimerPolicy::FixedDelay,
            missed_ticks: 0,
            max_catch_up: 0,
        };
        assert!(t2 > t1);

        let mut cl = ChronoLine::new();
        cl.register(Deadline(100), TimerId(1)).unwrap();
        cl.register(Deadline(50), TimerId(2)).unwrap();
        cl.tick(200);
        let first = cl.pop_expired().unwrap();
        assert_eq!(first.id, TimerId(2));
    }

    #[test]
    fn test_multiple_expired_in_order() {
        let mut cl = ChronoLine::new();
        cl.register(Deadline(10), TimerId(1)).unwrap();
        cl.register(Deadline(5), TimerId(2)).unwrap();
        cl.register(Deadline(20), TimerId(3)).unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
        cl.register(Deadline::new(15), TimerId(2)).unwrap();

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

    #[test]
    fn test_reject_zero_interval() {
        let mut cl = ChronoLine::new();
        let result = cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 0 },
        );
        assert_eq!(result, Err(RegisterError::ZeroInterval));
        assert!(cl.is_empty());
    }

    #[test]
    fn test_fixed_rate_phase_maintained() {
        let mut cl = ChronoLine::new();

        cl.register_with_mode_and_policy(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 10 },
            TimerPolicy::FixedRate,
        )
        .unwrap();

        cl.tick(15);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 1);
        assert_eq!(cl.next_deadline(), Some(Deadline(20)));

        cl.tick(25);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 1);
        assert_eq!(cl.next_deadline(), Some(Deadline(30)));
    }

    #[test]
    fn test_catch_up_limit_fixed_delay() {
        let mut cl = ChronoLine::new();
        cl.set_max_catch_up(2);

        cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 10 },
        )
        .unwrap();

        cl.tick(50);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 2);
        // FixedDelay reschedules at now + interval = 60
        assert!(!cl.has_expired());
        assert_eq!(cl.next_deadline(), Some(Deadline(60)));
    }

    #[test]
    fn test_catch_up_limit_fixed_rate() {
        let mut cl = ChronoLine::new();
        cl.set_max_catch_up(2);

        cl.register_with_mode_and_policy(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 10 },
            TimerPolicy::FixedRate,
        )
        .unwrap();

        cl.tick(50);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 2);
        // FixedRate advances by interval * missed=5 = 50: deadline 10 + 50 = 60
        assert_eq!(cl.next_deadline(), Some(Deadline(60)));

        cl.tick(60);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 1);
        // Next deadline at current(60) + interval = 70
        assert_eq!(cl.next_deadline(), Some(Deadline(70)));
    }

    #[test]
    fn test_fixed_delay_repeating() {
        let mut cl = ChronoLine::new();

        cl.register_with_mode(
            Deadline::new(10),
            TimerId(1),
            TimerMode::Repeating { interval_ticks: 10 },
        )
        .unwrap();

        cl.tick(25);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 2);
        assert_eq!(cl.next_deadline(), Some(Deadline(35)));

        cl.tick(35);
        let t = cl.pop_expired().unwrap();
        assert_eq!(t.id, TimerId(1));
        assert_eq!(t.missed_ticks, 1);
        assert_eq!(cl.next_deadline(), Some(Deadline(45)));
    }
}
