//! Parsing an FT5x06-family touch report.
//!
//! This lives above the platform because it is the device's protocol, not
//! the bus's business: the panel driver reads 31 bytes and this turns them
//! into points. It is also the part of touch that was wrong for weeks - the
//! count came from the wrong bits and "up" counted as a press - so it is
//! worth having where the host can test it.
//!
//! The layout is the one `edt-ft5x06.c` reads: a status byte at 0x02 whose
//! low nibble is the number of valid points, then five six-byte records
//! starting at 0x03. In a record the first two bytes are the X with the
//! event in the top bits, the next two the Y, then weight and area.

/// Bytes to read: the status byte at 0x02 and five six-byte records from
/// 0x03, so 0x00 through 0x20 inclusive.
pub const FRAME_LEN: usize = 0x21;
/// Records the controller keeps.
pub const MAX_POINTS: usize = 5;
/// Register the frame starts at.
pub const FRAME_REG: u8 = 0x00;

/// One finger, as the report describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u16,
    pub y: u16,
    /// Down or moving. A report with the up event still carries the
    /// position of the finger that just left, so this cannot be inferred
    /// from the coordinates.
    pub pressed: bool,
}

/// Event field of byte 0 of a record.
const EVENT_UP: u8 = 1;
const EVENT_RESERVED: u8 = 3;

/// How many points a frame claims, clamped to what a frame can hold.
pub fn count(frame: &[u8]) -> usize {
    if frame.len() <= 2 {
        return 0;
    }
    (frame[2] & 0x0f) as usize
}

/// Parse a frame into points, in slot order, skipping reserved slots.
///
/// Returns the points written and the array. Short frames simply yield what
/// fits: the bus read is the only thing that can shorten it, and a partial
/// frame is not worth a panic in the input path.
pub fn parse(frame: &[u8]) -> (usize, [Option<Point>; MAX_POINTS]) {
    let mut out = [None; MAX_POINTS];
    let mut n = 0;
    let claimed = count(frame).min(MAX_POINTS);
    for slot in 0..claimed {
        let at = 3 + slot * 6;
        let Some(rec) = frame.get(at..at + 6) else {
            break;
        };
        let event = rec[0] >> 6;
        if event == EVENT_RESERVED {
            continue;
        }
        out[n] = Some(Point {
            x: (((rec[0] & 0x0f) as u16) << 8) | rec[1] as u16,
            y: (((rec[2] & 0x0f) as u16) << 8) | rec[3] as u16,
            pressed: event != EVENT_UP,
        });
        n += 1;
    }
    (n, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(count: u8, records: &[(u8, u16, u16)]) -> [u8; FRAME_LEN] {
        let mut f = [0u8; FRAME_LEN];
        f[2] = count;
        for (i, (event, x, y)) in records.iter().enumerate() {
            let at = 3 + i * 6;
            f[at] = (event << 6) | ((x >> 8) as u8 & 0x0f);
            f[at + 1] = *x as u8;
            f[at + 2] = ((y >> 8) as u8) & 0x0f;
            f[at + 3] = *y as u8;
        }
        f
    }

    #[test]
    fn one_finger_down_carries_its_coordinates() {
        let f = frame_with(1, &[(0, 100, 200)]);
        let (n, p) = parse(&f);
        assert_eq!(n, 1);
        assert_eq!(
            p[0],
            Some(Point {
                x: 100,
                y: 200,
                pressed: true
            })
        );
    }

    #[test]
    fn a_contact_event_is_still_pressed() {
        // Event 2 is "contact" - a finger that is on the panel and moving.
        let f = frame_with(1, &[(2, 1079, 2339)]);
        let (n, p) = parse(&f);
        assert_eq!(n, 1);
        assert_eq!(p[0].unwrap().pressed, true);
        assert_eq!(p[0].unwrap().x, 1079);
        assert_eq!(p[0].unwrap().y, 2339);
    }

    #[test]
    fn the_up_event_is_a_release_with_a_position() {
        let f = frame_with(1, &[(1, 500, 600)]);
        let (n, p) = parse(&f);
        assert_eq!(n, 1);
        assert!(!p[0].unwrap().pressed);
        assert_eq!(p[0].unwrap().x, 500);
    }

    #[test]
    fn reserved_slots_are_skipped_rather_than_reported_as_fingers() {
        // Slot 0 reserved, slot 1 a real contact.
        let f = frame_with(2, &[(3, 0, 0), (0, 40, 50)]);
        let (n, p) = parse(&f);
        assert_eq!(n, 1);
        assert_eq!(p[0].unwrap().x, 40);
        assert_eq!(p[1], None);
    }

    #[test]
    fn the_count_comes_from_the_low_nibble() {
        // The old code read bits 6:4, which are zero in every real frame.
        let mut f = frame_with(1, &[(0, 7, 8)]);
        f[2] = 0x01;
        assert_eq!(count(&f), 1);
        f[2] = 0x21; // high nibble noise must not matter
        assert_eq!(count(&f), 1);
    }

    #[test]
    fn more_slots_than_the_controller_has_is_clamped() {
        // A frame claiming fifteen points is a corrupt status byte, not a
        // fifteen-finger gesture: read five records and stop.
        let f = frame_with(15, &[(0, 1, 2)]);
        let (n, p) = parse(&f);
        assert_eq!(n, MAX_POINTS);
        assert_eq!(p[0].unwrap().x, 1);
    }

    #[test]
    fn a_short_frame_yields_only_what_fits() {
        let f = frame_with(5, &[(0, 1, 2)]);
        // Nine bytes hold the status and exactly one record.
        let (n, p) = parse(&f[..9]);
        assert_eq!(n, 1);
        assert_eq!(p[0].unwrap().x, 1);
    }

    #[test]
    fn an_empty_frame_reports_nothing() {
        let (n, p) = parse(&[0u8; FRAME_LEN]);
        assert_eq!(n, 0);
        assert_eq!(p, [None; MAX_POINTS]);
    }
}
