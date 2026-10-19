//! Multitouch contacts, recovered from per-slot controller frames.
//!
//! A capacitive controller does not report "finger 3 moved"; it reports a
//! frame of slots, each holding an id and a position, and the ids are only
//! valid inside that frame. Turning frames into a stream of down/move/up is
//! the difference between a driver that knows where fingers are and a shell
//! that can implement pinch and two-finger scroll.
//!
//! [`ContactTracker`] owns the previous frame and diffs it: a slot that was
//! empty and now holds an id is a down, the same id in the same slot is a
//! move, an id that vanished is an up, and a slot whose id changed is an up
//! followed by a down - controllers recycle ids without ever lifting.
//!
//! The tracker is deliberately frame-agnostic about *how many* slots the
//! hardware has; the caller says so by the length of the slice it passes.

/// Most fingers a phone panel reports. Five is the usual cap.
pub const MAX_CONTACTS: usize = 5;

/// What happened to one contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactPhase {
    Down,
    Move,
    Up,
}

/// One contact event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub id: u8,
    pub x: u16,
    pub y: u16,
    pub phase: ContactPhase,
}

/// One slot of a controller frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotSample {
    /// Controller-assigned id; only meaningful while `active`.
    pub id: u8,
    pub x: u16,
    pub y: u16,
    /// Whether this slot currently holds a finger.
    pub active: bool,
}

impl SlotSample {
    pub const fn up() -> Self {
        Self {
            id: 0,
            x: 0,
            y: 0,
            active: false,
        }
    }

    pub const fn down(id: u8, x: u16, y: u16) -> Self {
        Self {
            id,
            x,
            y,
            active: true,
        }
    }
}

/// Diffs controller frames into contact events.
#[derive(Clone, Debug)]
pub struct ContactTracker {
    /// Previous frame, slot by slot: (id, x, y) when the slot had a finger.
    prev: [Option<(u8, u16, u16)>; MAX_CONTACTS],
}

impl ContactTracker {
    pub const fn new() -> Self {
        Self {
            prev: [None; MAX_CONTACTS],
        }
    }

    /// How many fingers the last frame held.
    pub fn count(&self) -> usize {
        self.prev.iter().filter(|s| s.is_some()).count()
    }

    pub fn is_down(&self) -> bool {
        self.count() > 0
    }

    /// Fold one frame in and write the events it produced.
    ///
    /// `out` needs room for two events per slot, because a recycled id is an
    /// up and a down in the same breath. A short buffer is truncated rather
    /// than panicking, and the return value says how many would have fitted.
    pub fn update(&mut self, frame: &[SlotSample], out: &mut [Contact]) -> usize {
        let mut written = 0usize;
        // Ids are the identity; slots are only where the controller happened
        // to put the finger this frame. Match by id so a finger that changes
        // slot is still one finger.
        for old in self.prev.iter().flatten() {
            let (id, x, y) = *old;
            let still = frame
                .iter()
                .take(MAX_CONTACTS)
                .any(|s| s.active && s.id == id);
            if !still {
                if written < out.len() {
                    out[written] = Contact {
                        id,
                        x,
                        y,
                        phase: ContactPhase::Up,
                    };
                }
                written += 1;
            }
        }
        let mut next: [Option<(u8, u16, u16)>; MAX_CONTACTS] = [None; MAX_CONTACTS];
        let mut used = 0usize;
        for sample in frame.iter().take(MAX_CONTACTS) {
            if !sample.active || used == MAX_CONTACTS {
                continue;
            }
            // A controller that repeats an id is confused; keep the first.
            if next[..used]
                .iter()
                .flatten()
                .any(|(id, _, _)| *id == sample.id)
            {
                continue;
            }
            next[used] = Some((sample.id, sample.x, sample.y));
            used += 1;
            let before = self
                .prev
                .iter()
                .flatten()
                .find(|(id, _, _)| *id == sample.id)
                .copied();
            let phase = match before {
                None => ContactPhase::Down,
                Some((_, x, y)) if x == sample.x && y == sample.y => continue,
                Some(_) => ContactPhase::Move,
            };
            if written < out.len() {
                out[written] = Contact {
                    id: sample.id,
                    x: sample.x,
                    y: sample.y,
                    phase,
                };
            }
            written += 1;
        }
        self.prev = next;
        written
    }

    /// Forget every finger without reading a frame, reporting each as lifted.
    ///
    /// A controller that stops answering leaves the shell holding a contact
    /// forever; the watchdog calls this so the UI cannot stay stuck mid-drag.
    pub fn lift_all(&mut self, out: &mut [Contact]) -> usize {
        let mut written = 0usize;
        for slot in 0..MAX_CONTACTS {
            if let Some((id, x, y)) = self.prev[slot] {
                if written < out.len() {
                    out[written] = Contact {
                        id,
                        x,
                        y,
                        phase: ContactPhase::Up,
                    };
                }
                written += 1;
            }
            self.prev[slot] = None;
        }
        written
    }
}

impl Default for ContactTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(samples: &[(u8, u16, u16)]) -> [SlotSample; MAX_CONTACTS] {
        let mut f = [SlotSample::up(); MAX_CONTACTS];
        for (slot, (id, x, y)) in samples.iter().enumerate() {
            f[slot] = SlotSample::down(*id, *x, *y);
        }
        f
    }

    #[test]
    fn one_finger_down_move_up() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 10];

        let n = t.update(&frame(&[(4, 100, 200)]), &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0].phase, ContactPhase::Down);
        assert_eq!((out[0].id, out[0].x, out[0].y), (4, 100, 200));

        let n = t.update(&frame(&[(4, 110, 190)]), &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0].phase, ContactPhase::Move);
        assert_eq!((out[0].x, out[0].y), (110, 190));

        let n = t.update(&frame(&[]), &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0].phase, ContactPhase::Up);
        assert_eq!(
            (out[0].x, out[0].y),
            (110, 190),
            "the lift keeps the last point"
        );
        assert!(!t.is_down());
    }

    #[test]
    fn two_fingers_are_tracked_independently() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 10];
        assert_eq!(t.update(&frame(&[(1, 10, 10), (2, 90, 90)]), &mut out), 2);
        assert_eq!(t.count(), 2);
        assert_eq!(t.update(&frame(&[(1, 12, 10), (2, 88, 90)]), &mut out), 2);
        assert_eq!(out[0].phase, ContactPhase::Move);
        assert_eq!(out[1].phase, ContactPhase::Move);
        // One lifts; the other stays and keeps its identity even though the
        // controller moved it into the slot the first finger left.
        assert_eq!(t.update(&frame(&[(2, 88, 90)]), &mut out), 1);
        assert_eq!(out[0].phase, ContactPhase::Up);
        assert_eq!(out[0].id, 1);
        assert_eq!(t.count(), 1);
        // A still finger reports nothing; the next real move does.
        assert_eq!(t.update(&frame(&[(2, 88, 90)]), &mut out), 0);
        assert_eq!(t.update(&frame(&[(2, 80, 90)]), &mut out), 1);
        assert_eq!(out[0].phase, ContactPhase::Move);
        assert_eq!(out[0].id, 2);
    }

    #[test]
    fn a_recycled_id_is_an_up_then_a_down() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 10];
        t.update(&frame(&[(7, 50, 50)]), &mut out);
        let n = t.update(&frame(&[(7, 400, 400)]), &mut out);
        assert_eq!(n, 1, "same id in the same slot is a jump, not a new finger");
        assert_eq!(out[0].phase, ContactPhase::Move);

        // A different id in the same slot is a lift plus a new finger.
        let n = t.update(&frame(&[(9, 60, 60)]), &mut out);
        assert_eq!(n, 2);
        assert_eq!(out[0].phase, ContactPhase::Up);
        assert_eq!(out[0].id, 7);
        assert_eq!(out[1].phase, ContactPhase::Down);
        assert_eq!(out[1].id, 9);
    }

    #[test]
    fn a_short_frame_is_a_lift_for_the_slots_it_omits() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 10];
        t.update(&frame(&[(1, 10, 10), (2, 20, 20)]), &mut out);
        let n = t.update(&[SlotSample::down(1, 11, 10)], &mut out);
        assert_eq!(n, 2);
        assert_eq!(out[0].phase, ContactPhase::Up);
        assert_eq!(out[0].id, 2);
        assert_eq!(out[1].phase, ContactPhase::Move);
        assert_eq!(out[1].id, 1);
    }

    #[test]
    fn a_small_buffer_truncates_instead_of_panicking() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 1];
        let n = t.update(&frame(&[(1, 1, 1), (2, 2, 2)]), &mut out);
        assert_eq!(n, 2, "the count is the truth");
        assert_eq!(out[0].id, 1);
        assert_eq!(t.count(), 2, "state still advances");
    }

    #[test]
    fn lift_all_releases_a_stuck_drag() {
        let mut t = ContactTracker::new();
        let mut out = [Contact {
            id: 0,
            x: 0,
            y: 0,
            phase: ContactPhase::Down,
        }; 10];
        t.update(&frame(&[(3, 5, 5), (4, 6, 6)]), &mut out);
        assert_eq!(t.lift_all(&mut out), 2);
        assert_eq!(out[0].phase, ContactPhase::Up);
        assert_eq!(out[1].phase, ContactPhase::Up);
        assert!(!t.is_down());
        assert_eq!(t.lift_all(&mut out), 0, "nothing left to lift");
    }
}
