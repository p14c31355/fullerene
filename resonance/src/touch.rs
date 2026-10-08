//! Source-grounded parsers for reusable touchscreen report formats.
//!
//! This module deliberately contains no bus, GPIO, or reset access.  A
//! platform backend owns transport and supplies the bytes received from the
//! controller.

use crate::event::{TouchContact, TouchPhase};

/// Goodix GT1x contact record size used by the public GT1x driver.
pub const GT1X_CONTACT_SIZE: usize = 8;
/// Status byte bit indicating that the coordinate buffer is valid.
pub const GT1X_STATUS_READY: u8 = 0x80;
/// Maximum contacts accepted by the generic parser.
pub const GT1X_MAX_CONTACTS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gt1xReportError {
    TooShort,
    NotReady,
    TooManyContacts,
    InvalidContactId,
}

/// Parse a Goodix GT1x 8-byte contact report.
///
/// The report starts with a status byte.  Its low nibble is the contact count;
/// each contact is encoded as `id, x_le16, y_le16, width_le16, reserved`.
/// `out` is caller-owned so the parser remains allocation-free and usable from
/// early platform code.
pub fn parse_gt1x_report(
    report: &[u8],
    out: &mut [TouchContact],
) -> Result<usize, Gt1xReportError> {
    let status = *report.first().ok_or(Gt1xReportError::TooShort)?;
    if status & GT1X_STATUS_READY == 0 {
        return Err(Gt1xReportError::NotReady);
    }
    let count = (status & 0x0f) as usize;
    if count > GT1X_MAX_CONTACTS || count > out.len() {
        return Err(Gt1xReportError::TooManyContacts);
    }
    let needed = 1 + count * GT1X_CONTACT_SIZE;
    if report.len() < needed {
        return Err(Gt1xReportError::TooShort);
    }

    for (index, slot) in out.iter_mut().take(count).enumerate() {
        let base = 1 + index * GT1X_CONTACT_SIZE;
        let id = report[base] & 0x0f;
        if id as usize >= GT1X_MAX_CONTACTS {
            return Err(Gt1xReportError::InvalidContactId);
        }
        let x = u16::from_le_bytes([report[base + 1], report[base + 2]]);
        let y = u16::from_le_bytes([report[base + 3], report[base + 4]]);
        let width = u16::from_le_bytes([report[base + 5], report[base + 6]]);
        *slot = TouchContact {
            id,
            x: x as i32,
            y: y as i32,
            pressure: width,
            phase: TouchPhase::Move,
        };
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_contacts_little_endian() {
        let report = [
            0x82, 0x01, 0x38, 0x04, 0x62, 0x09, 0x2c, 0x01, 0x00, 0x02, 0x10,
            0x00, 0x20, 0x00, 0x08, 0x00, 0x00,
        ];
        let mut contacts = [TouchContact {
            id: 0,
            x: 0,
            y: 0,
            pressure: 0,
            phase: TouchPhase::Cancel,
        }; 2];
        let count = parse_gt1x_report(&report, &mut contacts).unwrap();
        assert_eq!(count, 2);
        assert_eq!(contacts[0].id, 1);
        assert_eq!((contacts[0].x, contacts[0].y, contacts[0].pressure), (1080, 2402, 300));
        assert_eq!(contacts[1].id, 2);
        assert_eq!((contacts[1].x, contacts[1].y, contacts[1].pressure), (16, 32, 8));
    }

    #[test]
    fn rejects_unready_and_short_reports() {
        let mut contacts = [TouchContact {
            id: 0,
            x: 0,
            y: 0,
            pressure: 0,
            phase: TouchPhase::Cancel,
        }; 1];
        assert_eq!(parse_gt1x_report(&[0x01], &mut contacts), Err(Gt1xReportError::NotReady));
        assert_eq!(parse_gt1x_report(&[0x81], &mut contacts), Err(Gt1xReportError::TooShort));
    }
}
