//! Gesture recognition over the pointer event stream.
//!
//! [`crate::input`] turns samples into press/move/release. A phone wants more
//! than that: a tap opens, a second tap zooms, a held press selects, a short
//! flick scrolls a page and a long drag moves a window. Those are policy, not
//! mechanism, but they are the same policy on every touch device, so they live
//! here instead of in each shell.
//!
//! Time is an argument, never a clock read, so the recogniser is testable off
//! the device and usable from an interrupt-free polling loop. One gesture per
//! call keeps the caller's loop cheap; a long press is the only gesture that
//! needs a call the pointer did not cause, and [`GestureRecognizer::tick`]
//! exists for exactly that.
//!
//! A tap is reported as it lands and again as a double tap when a second tap
//! arrives nearby in time. Consumers that must not act twice (opening a
//! window, say) should either ignore `Tap` until `double_tap_ms` passes or
//! only listen for the gestures they implement.

use crate::input::PointerPhase;

/// Thresholds for one device. Distances are surface pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureConfig {
    /// Movement a press may wander and still be a tap.
    pub tap_slop: u16,
    /// Longest a press may last and still be a tap.
    pub tap_max_ms: u32,
    /// A second press starting within this of the first ending compounds it.
    pub double_tap_ms: u32,
    /// How far the second press may land from the first.
    pub double_tap_slop: u16,
    /// A press held this long without moving is a long press.
    pub long_press_ms: u32,
    /// Movement before a press becomes a drag at all.
    pub drag_start: u16,
    /// Distance that turns a released drag into a swipe.
    pub swipe_min: u16,
    /// A drag slower than this is a placement, not a flick.
    pub swipe_max_ms: u32,
}

impl GestureConfig {
    /// Thresholds for a 1080 x 2340 panel, about what Android uses.
    pub const fn phone() -> Self {
        Self {
            tap_slop: 24,
            tap_max_ms: 400,
            double_tap_ms: 300,
            double_tap_slop: 48,
            long_press_ms: 500,
            drag_start: 16,
            swipe_min: 120,
            swipe_max_ms: 400,
        }
    }

    /// Thresholds that only ever fire on a deliberate input: for a mouse,
    /// where the pointer is precise and a drag of ten pixels is a drag.
    pub const fn pointer() -> Self {
        Self {
            tap_slop: 4,
            tap_max_ms: 300,
            double_tap_ms: 300,
            double_tap_slop: 8,
            long_press_ms: 700,
            drag_start: 3,
            swipe_min: 40,
            swipe_max_ms: 500,
        }
    }
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self::phone()
    }
}

/// Which way a swipe went, by its dominant axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeDir {
    Left,
    Right,
    Up,
    Down,
}

impl SwipeDir {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
            Self::Up => "UP",
            Self::Down => "DOWN",
        }
    }
}

/// What the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Contact down and up in one place, quickly.
    Tap { x: u16, y: u16 },
    /// A tap that landed near the previous one, soon after it.
    DoubleTap { x: u16, y: u16 },
    /// Held still long enough to mean selection.
    LongPress { x: u16, y: u16 },
    /// Movement since the previous move event, once the press is a drag.
    Drag { dx: i16, dy: i16 },
    /// A drag released fast enough and far enough to be a flick.
    Swipe {
        dir: SwipeDir,
        distance: u16,
        x: u16,
        y: u16,
    },
}

/// Where the current press stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// No contact.
    Idle,
    /// Contact down, still a candidate for tap or long press.
    Press {
        x: u16,
        y: u16,
        t0: u32,
        last_x: u16,
        last_y: u16,
        long_sent: bool,
    },
    /// Contact moving; tap and long press are off the table.
    Drag {
        x0: u16,
        y0: u16,
        t0: u32,
        last_x: u16,
        last_y: u16,
    },
    /// A tap finished; a second one nearby would make it a double.
    AfterTap { x: u16, y: u16, t: u32 },
}

/// Turns the pointer stream into gestures.
#[derive(Clone, Copy, Debug)]
pub struct GestureRecognizer {
    cfg: GestureConfig,
    stage: Stage,
    /// The current press is the second half of a double tap.
    pending_second: bool,
}

impl GestureRecognizer {
    pub const fn new(cfg: GestureConfig) -> Self {
        Self {
            cfg,
            stage: Stage::Idle,
            pending_second: false,
        }
    }

    pub const fn config(&self) -> GestureConfig {
        self.cfg
    }

    /// True while a finger is down.
    pub fn active(&self) -> bool {
        matches!(self.stage, Stage::Press { .. } | Stage::Drag { .. })
    }

    /// Feed one pointer event. Returns the gesture it completed, if any.
    pub fn event(&mut self, phase: PointerPhase, x: u16, y: u16, now_ms: u32) -> Option<Gesture> {
        match phase {
            PointerPhase::Down => {
                let second = match self.stage {
                    Stage::AfterTap { x: px, y: py, t } => {
                        now_ms.wrapping_sub(t) <= self.cfg.double_tap_ms
                            && near(x, y, px, py, self.cfg.double_tap_slop)
                    }
                    _ => false,
                };
                self.stage = Stage::Press {
                    x,
                    y,
                    t0: now_ms,
                    last_x: x,
                    last_y: y,
                    long_sent: false,
                };
                // The release of this press decides: a second tap that lands
                // and lifts in place becomes a DoubleTap there.
                self.pending_second = second;
                None
            }
            PointerPhase::Move => match self.stage {
                Stage::Press {
                    x: x0,
                    y: y0,
                    t0,
                    last_x,
                    last_y,
                    long_sent,
                } => {
                    if !near(x, y, x0, y0, self.cfg.drag_start) {
                        self.stage = Stage::Drag {
                            x0,
                            y0,
                            t0,
                            last_x: x,
                            last_y: y,
                        };
                        Some(Gesture::Drag {
                            dx: delta(last_x, x),
                            dy: delta(last_y, y),
                        })
                    } else {
                        // Still a press; remember where the finger was so the
                        // first drag delta is one sample, not the whole travel.
                        self.stage = Stage::Press {
                            x: x0,
                            y: y0,
                            t0,
                            last_x: x,
                            last_y: y,
                            long_sent,
                        };
                        None
                    }
                }
                Stage::Drag {
                    x0,
                    y0,
                    t0,
                    last_x,
                    last_y,
                } => {
                    self.stage = Stage::Drag {
                        x0,
                        y0,
                        t0,
                        last_x: x,
                        last_y: y,
                    };
                    Some(Gesture::Drag {
                        dx: delta(last_x, x),
                        dy: delta(last_y, y),
                    })
                }
                _ => None,
            },
            PointerPhase::Up => {
                let second = self.pending_second;
                self.pending_second = false;
                let out = match self.stage {
                    Stage::Press {
                        x: x0, y: y0, t0, ..
                    } => {
                        let held = now_ms.wrapping_sub(t0);
                        if near(x, y, x0, y0, self.cfg.tap_slop) && held <= self.cfg.tap_max_ms {
                            if second {
                                Gesture::DoubleTap { x, y }
                            } else {
                                Gesture::Tap { x, y }
                            }
                        } else {
                            // A slow press in place: the long press was due
                            // but no tick ran. Report it now rather than lose it.
                            Gesture::LongPress { x, y }
                        }
                    }
                    Stage::Drag { x0, y0, t0, .. } => {
                        let dist = distance(x0, y0, x, y);
                        let held = now_ms.wrapping_sub(t0);
                        if dist >= self.cfg.swipe_min && held <= self.cfg.swipe_max_ms {
                            Gesture::Swipe {
                                dir: direction(x0, y0, x, y),
                                distance: dist,
                                x,
                                y,
                            }
                        } else {
                            // A placement: the drag already reported its motion.
                            self.stage = Stage::Idle;
                            return None;
                        }
                    }
                    _ => {
                        self.stage = Stage::Idle;
                        return None;
                    }
                };
                self.stage = match out {
                    Gesture::Tap { x, y } => Stage::AfterTap { x, y, t: now_ms },
                    Gesture::DoubleTap { .. } => Stage::Idle,
                    _ => Stage::Idle,
                };
                Some(out)
            }
        }
    }

    /// Call from the idle loop: a press held still becomes a long press with
    /// no pointer event to trigger it.
    pub fn tick(&mut self, now_ms: u32) -> Option<Gesture> {
        if let Stage::Press {
            x,
            y,
            t0,
            long_sent: false,
            ..
        } = self.stage
        {
            if now_ms.wrapping_sub(t0) >= self.cfg.long_press_ms {
                self.stage = Stage::Press {
                    x,
                    y,
                    t0,
                    last_x: x,
                    last_y: y,
                    long_sent: true,
                };
                return Some(Gesture::LongPress { x, y });
            }
        }
        None
    }

    /// Forget any half-finished press, e.g. when the panel is re-initialised
    /// under a finger and the release will never arrive.
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.pending_second = false;
    }
}

fn delta(from: u16, to: u16) -> i16 {
    to as i16 - from as i16
}

fn distance(x0: u16, y0: u16, x: u16, y: u16) -> u16 {
    let dx = delta(x0, x).unsigned_abs() as u32;
    let dy = delta(y0, y).unsigned_abs() as u32;
    isqrt(dx * dx + dy * dy) as u16
}

/// Integer square root, so the crate stays `no_std` and float-free.
fn isqrt(n: u32) -> u32 {
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    let mut n = n;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if n >= r + bit {
            n -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r
}

fn near(x: u16, y: u16, x2: u16, y2: u16, slop: u16) -> bool {
    delta(x, x2).unsigned_abs() <= slop && delta(y, y2).unsigned_abs() <= slop
}

fn direction(x0: u16, y0: u16, x: u16, y: u16) -> SwipeDir {
    let dx = delta(x0, x);
    let dy = delta(y0, y);
    if dx.unsigned_abs() >= dy.unsigned_abs() {
        if dx < 0 {
            SwipeDir::Left
        } else {
            SwipeDir::Right
        }
    } else if dy < 0 {
        SwipeDir::Up
    } else {
        SwipeDir::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> GestureRecognizer {
        GestureRecognizer::new(GestureConfig::phone())
    }

    fn tap(r: &mut GestureRecognizer, x: u16, y: u16, t: u32) -> Option<Gesture> {
        assert_eq!(r.event(PointerPhase::Down, x, y, t), None);
        r.event(PointerPhase::Up, x, y, t + 40)
    }

    #[test]
    fn a_quick_press_in_place_is_a_tap() {
        let mut r = rec();
        assert_eq!(
            tap(&mut r, 500, 900, 1000),
            Some(Gesture::Tap { x: 500, y: 900 })
        );
        assert!(!r.active());
    }

    #[test]
    fn a_second_tap_nearby_in_time_is_a_double_tap() {
        let mut r = rec();
        assert_eq!(
            tap(&mut r, 500, 900, 1000),
            Some(Gesture::Tap { x: 500, y: 900 })
        );
        assert_eq!(
            tap(&mut r, 505, 905, 1200),
            Some(Gesture::DoubleTap { x: 505, y: 905 })
        );
        // The pair is spent: a third tap starts over.
        assert_eq!(
            tap(&mut r, 505, 905, 1400),
            Some(Gesture::Tap { x: 505, y: 905 })
        );
    }

    #[test]
    fn a_second_tap_far_away_is_its_own_tap() {
        let mut r = rec();
        assert_eq!(
            tap(&mut r, 100, 100, 0),
            Some(Gesture::Tap { x: 100, y: 100 })
        );
        assert_eq!(
            tap(&mut r, 400, 400, 100),
            Some(Gesture::Tap { x: 400, y: 400 })
        );
    }

    #[test]
    fn a_slow_second_tap_is_its_own_tap() {
        let mut r = rec();
        tap(&mut r, 100, 100, 0);
        assert_eq!(
            tap(&mut r, 100, 100, 2000),
            Some(Gesture::Tap { x: 100, y: 100 })
        );
    }

    #[test]
    fn holding_still_becomes_a_long_press_on_tick() {
        let mut r = rec();
        assert_eq!(r.event(PointerPhase::Down, 700, 700, 0), None);
        assert_eq!(r.tick(400), None);
        assert_eq!(r.tick(500), Some(Gesture::LongPress { x: 700, y: 700 }));
        // The release must not report a tap as well.
        assert_eq!(
            r.event(PointerPhase::Up, 700, 700, 600),
            Some(Gesture::LongPress { x: 700, y: 700 })
        );
    }

    #[test]
    fn a_press_released_after_its_deadline_is_a_long_press_even_without_a_tick() {
        let mut r = rec();
        r.event(PointerPhase::Down, 700, 700, 0);
        assert_eq!(
            r.event(PointerPhase::Up, 702, 701, 900),
            Some(Gesture::LongPress { x: 702, y: 701 })
        );
    }

    #[test]
    fn movement_past_the_slop_turns_the_press_into_a_drag() {
        let mut r = rec();
        r.event(PointerPhase::Down, 100, 100, 0);
        assert_eq!(r.event(PointerPhase::Move, 105, 100, 10), None);
        assert_eq!(
            r.event(PointerPhase::Move, 140, 100, 20),
            Some(Gesture::Drag { dx: 35, dy: 0 })
        );
        assert_eq!(
            r.event(PointerPhase::Move, 150, 130, 30),
            Some(Gesture::Drag { dx: 10, dy: 30 })
        );
        assert!(r.active());
    }

    #[test]
    fn a_drag_released_in_place_is_not_a_swipe() {
        let mut r = rec();
        r.event(PointerPhase::Down, 100, 100, 0);
        r.event(PointerPhase::Move, 200, 100, 50);
        assert_eq!(r.event(PointerPhase::Up, 200, 100, 4000), None);
        assert!(!r.active());
    }

    #[test]
    fn a_fast_release_names_its_dominant_axis() {
        let mut r = rec();
        r.event(PointerPhase::Down, 500, 1500, 0);
        r.event(PointerPhase::Move, 505, 1300, 60);
        r.event(PointerPhase::Move, 505, 1100, 120);
        let g = r.event(PointerPhase::Up, 505, 1100, 160);
        match g {
            Some(Gesture::Swipe { dir, distance, .. }) => {
                assert_eq!(dir, SwipeDir::Up);
                assert_eq!(distance, 400);
            }
            other => panic!("expected a swipe, got {:?}", other),
        }
    }

    #[test]
    fn a_horizontal_flick_right_is_right() {
        let mut r = rec();
        r.event(PointerPhase::Down, 100, 1000, 0);
        r.event(PointerPhase::Move, 300, 1002, 50);
        match r.event(PointerPhase::Up, 500, 1000, 90) {
            Some(Gesture::Swipe { dir, .. }) => assert_eq!(dir, SwipeDir::Right),
            other => panic!("expected a swipe, got {:?}", other),
        }
    }

    #[test]
    fn reset_forgets_a_press_whose_release_never_came() {
        let mut r = rec();
        r.event(PointerPhase::Down, 10, 10, 0);
        assert!(r.active());
        r.reset();
        assert!(!r.active());
        assert_eq!(r.event(PointerPhase::Up, 10, 10, 60), None);
    }

    #[test]
    fn a_finger_that_lifts_after_a_drag_does_not_become_a_tap() {
        let mut r = rec();
        r.event(PointerPhase::Down, 200, 200, 0);
        r.event(PointerPhase::Move, 260, 200, 20);
        // Released slowly and short of the swipe distance.
        assert_eq!(r.event(PointerPhase::Up, 260, 200, 900), None);
    }

    #[test]
    fn isqrt_is_exact_on_the_squares_that_matter() {
        for n in 0..100u32 {
            let s = isqrt(n * n);
            assert_eq!(s, n);
        }
        assert_eq!(isqrt(24 * 24 + 7 * 7), 25);
    }
}
