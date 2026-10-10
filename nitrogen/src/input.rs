//! Transport-independent pointer input model.
//!
//! A touch controller is a sampler: ask it for a point and it answers. A user
//! interface wants events: a press started, the finger moved, the press ended.
//! This module owns that translation so every touch backend - the XPT2046 on
//! the ESP32, and whatever the next board carries - produces the same event
//! stream and the same debounce behaviour instead of re-implementing edge
//! detection in each shell.
//!
//! The tracker is deliberately cheap: one sample in, at most one event out, no
//! allocation, and no assumption about where the samples come from.

/// What a pointer sample means for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    /// Contact began.
    Down,
    /// Contact continued at a new position.
    Move,
    /// Contact ended. The position is the last known point.
    Up,
}

impl PointerPhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Down => "DOWN",
            Self::Move => "MOVE",
            Self::Up => "UP",
        }
    }
}

/// One pointer event in surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub phase: PointerPhase,
    pub x: u16,
    pub y: u16,
}

/// One raw device reading in surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerSample {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

impl PointerSample {
    pub const fn released() -> Self {
        Self {
            x: 0,
            y: 0,
            pressed: false,
        }
    }
}

/// Debounce and motion thresholds for [`PointerTracker`].
///
/// Resistive panels are noisy at both edges: a finger landing produces a
/// scatter of samples, and lifting can chatter. Requiring consecutive samples
/// before believing a transition removes most of it, and requiring a minimum
/// displacement before reporting a move keeps jitter from flooding the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerConfig {
    pub press_confirm_samples: u8,
    pub release_confirm_samples: u8,
    pub move_threshold: u16,
    pub move_interval_ms: u16,
}

impl PointerConfig {
    /// A sane default for a resistive panel sampled in a tight loop.
    pub const fn resistive_default() -> Self {
        Self {
            press_confirm_samples: 3,
            release_confirm_samples: 2,
            move_threshold: 4,
            move_interval_ms: 0,
        }
    }
}

impl Default for PointerConfig {
    fn default() -> Self {
        Self::resistive_default()
    }
}

/// Turns a stream of device samples into Down/Move/Up events.
#[derive(Clone, Copy, Debug)]
pub struct PointerTracker {
    config: PointerConfig,
    pressed: bool,
    pending_press: u8,
    pending_release: u8,
    last_x: u16,
    last_y: u16,
    last_move_ms: u32,
    events: u32,
}

impl PointerTracker {
    pub const fn new(config: PointerConfig) -> Self {
        Self {
            config,
            pressed: false,
            pending_press: 0,
            pending_release: 0,
            last_x: 0,
            last_y: 0,
            last_move_ms: 0,
            events: 0,
        }
    }

    pub const fn config(&self) -> PointerConfig {
        self.config
    }

    pub fn set_config(&mut self, config: PointerConfig) {
        self.config = config;
    }

    /// Whether the tracker currently reports contact.
    pub const fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Position of the last reported point.
    pub const fn last_point(&self) -> (u16, u16) {
        (self.last_x, self.last_y)
    }

    /// How many events this tracker has emitted since construction.
    pub const fn event_count(&self) -> u32 {
        self.events
    }

    /// Forget any in-flight contact, keeping the counters. Used when the
    /// display is torn down or a shell takes over the stream.
    pub fn reset(&mut self) {
        self.pressed = false;
        self.pending_press = 0;
        self.pending_release = 0;
    }

    /// Feed one sample. Returns the event it completed, if any.
    ///
    /// `now_ms` is only consulted for the move rate limit, so a caller without
    /// a clock can pass 0 and still get correct Down/Move/Up semantics.
    pub fn update(&mut self, sample: PointerSample, now_ms: u32) -> Option<PointerEvent> {
        if sample.pressed {
            self.pending_release = 0;
            if !self.pressed {
                self.pending_press = self.pending_press.saturating_add(1);
                if self.pending_press < self.config.press_confirm_samples {
                    return None;
                }
                self.pressed = true;
                self.pending_press = 0;
                self.last_x = sample.x;
                self.last_y = sample.y;
                self.last_move_ms = now_ms;
                self.events = self.events.saturating_add(1);
                return Some(PointerEvent {
                    phase: PointerPhase::Down,
                    x: sample.x,
                    y: sample.y,
                });
            }

            let moved_x = sample.x.abs_diff(self.last_x);
            let moved_y = sample.y.abs_diff(self.last_y);
            if moved_x < self.config.move_threshold && moved_y < self.config.move_threshold {
                return None;
            }
            if self.config.move_interval_ms != 0 {
                let elapsed = now_ms.wrapping_sub(self.last_move_ms);
                if elapsed < u32::from(self.config.move_interval_ms) {
                    return None;
                }
            }
            self.last_x = sample.x;
            self.last_y = sample.y;
            self.last_move_ms = now_ms;
            self.events = self.events.saturating_add(1);
            return Some(PointerEvent {
                phase: PointerPhase::Move,
                x: sample.x,
                y: sample.y,
            });
        }

        self.pending_press = 0;
        if !self.pressed {
            return None;
        }
        self.pending_release = self.pending_release.saturating_add(1);
        if self.pending_release < self.config.release_confirm_samples {
            return None;
        }
        self.pressed = false;
        self.pending_release = 0;
        self.events = self.events.saturating_add(1);
        Some(PointerEvent {
            phase: PointerPhase::Up,
            x: self.last_x,
            y: self.last_y,
        })
    }
}

/// A fixed-capacity queue of pointer events.
///
/// A display flush produces events while the caller is inside the transfer, so
/// they need somewhere to wait that does not allocate. Overflow drops the
/// *oldest* event and records the loss: a shell that has fallen behind wants
/// the newest position, not a backlog of stale ones.
#[derive(Debug)]
pub struct PointerQueue<const N: usize> {
    slots: [Option<PointerEvent>; N],
    head: usize,
    len: usize,
    dropped: u32,
    pushed: u32,
}

impl<const N: usize> PointerQueue<N> {
    pub const fn new() -> Self {
        Self {
            slots: [None; N],
            head: 0,
            len: 0,
            dropped: 0,
            pushed: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total events ever accepted (including ones later dropped on overflow).
    pub const fn pushed(&self) -> u32 {
        self.pushed
    }

    /// Events discarded because the queue was full.
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn clear(&mut self) {
        self.slots = [None; N];
        self.head = 0;
        self.len = 0;
    }

    pub fn push(&mut self, event: PointerEvent) {
        if N == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        if self.len == N {
            // Make room by discarding the oldest.
            self.head = (self.head + 1) % N;
            self.len -= 1;
            self.dropped = self.dropped.saturating_add(1);
        }
        let index = (self.head + self.len) % N;
        self.slots[index] = Some(event);
        self.len += 1;
        self.pushed = self.pushed.saturating_add(1);
    }

    pub fn pop(&mut self) -> Option<PointerEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.slots[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        event
    }
}

impl<const N: usize> Default for PointerQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn event(x: u16) -> PointerEvent {
        PointerEvent {
            phase: PointerPhase::Move,
            x,
            y: 0,
        }
    }

    #[test]
    fn events_come_back_in_order() {
        let mut queue: PointerQueue<4> = PointerQueue::new();
        queue.push(event(1));
        queue.push(event(2));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop().map(|event| event.x), Some(1));
        assert_eq!(queue.pop().map(|event| event.x), Some(2));
        assert_eq!(queue.pop(), None);
        assert_eq!(queue.dropped(), 0);
    }

    #[test]
    fn overflow_drops_the_oldest_and_counts_it() {
        let mut queue: PointerQueue<2> = PointerQueue::new();
        queue.push(event(1));
        queue.push(event(2));
        queue.push(event(3));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pushed(), 3);
        // The stale first position is the one thrown away.
        assert_eq!(queue.pop().map(|event| event.x), Some(2));
        assert_eq!(queue.pop().map(|event| event.x), Some(3));
    }

    #[test]
    fn clear_keeps_the_statistics() {
        let mut queue: PointerQueue<2> = PointerQueue::new();
        queue.push(event(1));
        queue.push(event(2));
        queue.push(event(3));
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pushed(), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration that confirms a press on two samples, so the tests
    /// below describe the state machine rather than the current default.
    fn two_sample_press() -> PointerConfig {
        PointerConfig {
            press_confirm_samples: 2,
            ..PointerConfig::resistive_default()
        }
    }

    fn tracker() -> PointerTracker {
        PointerTracker::new(two_sample_press())
    }

    #[test]
    fn the_default_needs_three_samples_to_believe_a_press() {
        let mut default_tracker = PointerTracker::new(PointerConfig::resistive_default());
        assert_eq!(default_tracker.update(pressed(10, 10), 0), None);
        assert_eq!(default_tracker.update(pressed(10, 10), 0), None);
        assert!(!default_tracker.is_pressed());
        assert!(default_tracker.update(pressed(10, 10), 0).is_some());
    }

    fn pressed(x: u16, y: u16) -> PointerSample {
        PointerSample {
            x,
            y,
            pressed: true,
        }
    }

    #[test]
    fn a_single_noisy_sample_does_not_open_a_press() {
        let mut tracker = tracker();
        assert_eq!(tracker.update(pressed(10, 10), 0), None);
        assert_eq!(tracker.update(PointerSample::released(), 0), None);
        assert!(!tracker.is_pressed());
        // The confirmation counter restarts on the release, so a lone bounce
        // never becomes a Down.
        assert_eq!(tracker.update(pressed(10, 10), 0), None);
        assert!(!tracker.is_pressed());
    }

    #[test]
    fn two_consecutive_samples_make_a_down() {
        let mut tracker = tracker();
        assert_eq!(tracker.update(pressed(10, 10), 0), None);
        assert_eq!(
            tracker.update(pressed(11, 10), 0),
            Some(PointerEvent {
                phase: PointerPhase::Down,
                x: 11,
                y: 10
            })
        );
        assert!(tracker.is_pressed());
    }

    #[test]
    fn moves_below_the_threshold_are_suppressed() {
        let mut tracker = tracker();
        tracker.update(pressed(100, 100), 0);
        tracker.update(pressed(100, 100), 0);
        assert_eq!(tracker.update(pressed(101, 101), 0), None);
        assert_eq!(
            tracker.update(pressed(100, 106), 0),
            Some(PointerEvent {
                phase: PointerPhase::Move,
                x: 100,
                y: 106
            })
        );
    }

    #[test]
    fn up_reports_the_last_contact_point() {
        let mut tracker = tracker();
        tracker.update(pressed(40, 50), 0);
        tracker.update(pressed(40, 50), 0);
        tracker.update(pressed(70, 60), 0);
        assert_eq!(tracker.update(PointerSample::released(), 0), None);
        assert_eq!(
            tracker.update(PointerSample::released(), 0),
            Some(PointerEvent {
                phase: PointerPhase::Up,
                x: 70,
                y: 60
            })
        );
        assert!(!tracker.is_pressed());
    }

    #[test]
    fn the_move_rate_limit_holds_back_rapid_moves() {
        let mut tracker = PointerTracker::new(PointerConfig {
            move_interval_ms: 30,
            ..two_sample_press()
        });
        tracker.update(pressed(0, 0), 0);
        tracker.update(pressed(0, 0), 0);
        assert_eq!(
            tracker.update(pressed(50, 0), 10).map(|event| event.phase),
            None
        );
        assert_eq!(
            tracker.update(pressed(60, 0), 35).map(|event| event.phase),
            Some(PointerPhase::Move)
        );
    }

    #[test]
    fn a_full_gesture_reports_down_moves_and_up_in_order() {
        let mut tracker = tracker();
        let mut phases = alloc::vec::Vec::new();
        for (sample, now) in [
            (pressed(10, 10), 0),
            (pressed(10, 10), 1),
            (pressed(30, 10), 2),
            (pressed(30, 40), 3),
            (PointerSample::released(), 4),
            (PointerSample::released(), 5),
        ] {
            if let Some(event) = tracker.update(sample, now) {
                phases.push(event.phase);
            }
        }
        assert_eq!(
            phases,
            alloc::vec![
                PointerPhase::Down,
                PointerPhase::Move,
                PointerPhase::Move,
                PointerPhase::Up
            ]
        );
    }
}
