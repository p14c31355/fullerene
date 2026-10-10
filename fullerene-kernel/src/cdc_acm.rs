//! CDC-ACM, device side: the descriptors and the class-request decisions.
//!
//! Why this channel: it is the lightest way to get bytes from the gadget to a host that has no
//! root and no Android involved. Linux binds its own `cdc_acm` driver to these interfaces and the
//! device appears as `/dev/ttyACM0`, so `cat /dev/ttyACM0` is the entire host side. ADB needs a
//! state machine above the same USB layer; CDC-ACM needs class descriptors that are exactly right
//! and nothing else. It is also bidirectional, which the pull-up toggle is not.
//!
//! What is here and what is not: these are the *bytes* and the *decisions*, both pure, both unit
//! tested. They are written now because they cannot be exercised on the device until
//! control-endpoint enumeration completes - the host binds no driver to a device it has not
//! finished enumerating, and today it stops at `device descriptor read/64, error -71`. Wiring them
//! into the configuration descriptor and the EP0 handler is the next step, and it is the step that
//! needs the device.
//!
//! Endpoint reuse, stated plainly: the data interface here carries the bulk pair on EP2 (OUT) and
//! EP3 (IN) - the same two endpoints the `FDBG` debug transport already uses, and the same ones the
//! single ADB-class interface advertises today. That is deliberate: the transport does not change,
//! it just moves under a class the host has a driver for. The consequence is on the host side - a
//! tool that opens those endpoints raw cannot do so while `cdc_acm` holds the interface, and vice
//! versa - and it is a choice the host makes, e.g. by unbinding the driver.
//!
//! The class-specific requests below arrive on EP0 *before* any data flows, and a single one left
//! unanswered makes the host's `open()` fail, so they are enumerated rather than guessed at:
//!
//! ```text
//! SET_LINE_CODING        0x20  host -> device, 7 bytes,  no data stage reply
//! GET_LINE_CODING        0x21  device -> host, 7 bytes
//! SET_CONTROL_LINE_STATE 0x22  host -> device, 0 bytes, wValue bit 0 = DTR
//! ```
//!
//! DTR is the useful one: it is how the host says "I have opened the port and I am reading", which
//! is exactly the moment to send the black box.

/// Interface 0: the communications class interface (the control side).
pub const CONTROL_INTERFACE: u8 = 0;
/// Interface 1: the data class interface, which is where the bulk pair lives.
pub const DATA_INTERFACE: u8 = 1;
/// Notification endpoint (interrupt IN) - required by Linux's `cdc_acm` probe.
pub const NOTIFICATION_ENDPOINT: u8 = 0x81;
/// Data endpoints, matching the pair the existing transport uses.
pub const DATA_IN_ENDPOINT: u8 = 0x83;
pub const DATA_OUT_ENDPOINT: u8 = 0x02;

pub const CDC_CLASS_COMMUNICATIONS: u8 = 0x02;
pub const CDC_SUBCLASS_ACM: u8 = 0x02;
pub const CDC_PROTOCOL_AT: u8 = 0x01;
pub const CDC_CLASS_DATA: u8 = 0x0a;
pub const FUNCTIONAL_DESCRIPTOR: u8 = 0x24;

pub const SET_LINE_CODING: u8 = 0x20;
pub const GET_LINE_CODING: u8 = 0x21;
pub const SET_CONTROL_LINE_STATE: u8 = 0x22;

/// The control interface, its four functional descriptors, its notification endpoint, and the data
/// interface with its bulk pair.
///
/// Laid out as one array so that the `bLength` fields cannot drift from the bytes that follow them:
/// a wrong `bLength` here is a descriptor the host parses as garbage, and it fails at the point
/// where nothing is left to read about why.
pub const FUNCTION_DESCRIPTOR: [u8; 58] = [
    // --- control interface ---
    9,
    4,
    CONTROL_INTERFACE,
    0,
    1,
    CDC_CLASS_COMMUNICATIONS,
    CDC_SUBCLASS_ACM,
    CDC_PROTOCOL_AT,
    0,
    // Header functional descriptor, bcdCDC 1.10
    5,
    FUNCTIONAL_DESCRIPTOR,
    0x00,
    0x10,
    0x01,
    // Call management: no call management over the data interface, data interface 1
    5,
    FUNCTIONAL_DESCRIPTOR,
    0x01,
    0x00,
    DATA_INTERFACE,
    // ACM capabilities: line coding and control line state are supported, so the notification of
    // serial state is meaningful
    4,
    FUNCTIONAL_DESCRIPTOR,
    0x02,
    0x02,
    // Union: interface 0 is the master, interface 1 the slave. Linux's cdc_acm refuses to bind
    // without this, because it is what tells it the two interfaces are one function.
    5,
    FUNCTIONAL_DESCRIPTOR,
    0x06,
    CONTROL_INTERFACE,
    DATA_INTERFACE,
    // Notification endpoint: interrupt IN, 8 bytes, interval 16
    7,
    5,
    NOTIFICATION_ENDPOINT,
    3,
    0x08,
    0x00,
    0x10,
    // --- data interface ---
    9,
    4,
    DATA_INTERFACE,
    0,
    2,
    CDC_CLASS_DATA,
    0,
    0,
    0,
    // Bulk OUT then bulk IN, 64-byte packets, as the existing transport already runs them
    7,
    5,
    DATA_OUT_ENDPOINT,
    2,
    0x40,
    0x00,
    0,
    7,
    5,
    DATA_IN_ENDPOINT,
    2,
    0x40,
    0x00,
    0,
];

/// What a class-specific request on EP0 is, and what has to happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassRequest {
    /// The host is telling us its line settings. Nothing to send back, but the request must be
    /// accepted or `open()` fails.
    SetLineCoding,
    /// The host wants our line settings: seven bytes.
    GetLineCoding,
    /// The host is telling us DTR/RTS. `dtr` is what "the port is open and reading" looks like.
    SetControlLineState { dtr: bool, rts: bool },
    /// Not ours. The caller decides (usually: stall, or treat as a vendor request).
    Other,
}

/// Classify a class-specific request. `request_type` is `bmRequestType`: the class requests below
/// are 0x21 (host to device, class, interface) and 0xa1 (device to host, class, interface), and
/// they are told apart by that as well as by the request byte, because the same code means
/// something else to another class.
pub fn classify(request_type: u8, request: u8) -> ClassRequest {
    match (request_type, request) {
        (0x21, SET_LINE_CODING) => ClassRequest::SetLineCoding,
        (0xa1, GET_LINE_CODING) => ClassRequest::GetLineCoding,
        (0x21, SET_CONTROL_LINE_STATE) => ClassRequest::Other,
        _ => ClassRequest::Other,
    }
}

/// Classify with the `wValue` needed to read `SET_CONTROL_LINE_STATE`, which is the one request
/// whose meaning depends on it.
pub fn classify_with_value(request_type: u8, request: u8, value: u16) -> ClassRequest {
    if request_type == 0x21 && request == SET_CONTROL_LINE_STATE {
        // wValue bit 0 is DTR, bit 1 is RTS, as `CDC_SET_CONTROL_LINE_STATE` defines them.
        return ClassRequest::SetControlLineState {
            dtr: value & 0x01 != 0,
            rts: value & 0x02 != 0,
        };
    }
    classify(request_type, request)
}

/// The seven-byte reply to `GET_LINE_CODING`. We do not implement a UART, so the values are what a
/// host needs to see to finish opening the port: 115200 8N1, which is this project's own console
/// setting and therefore the least surprising thing to advertise.
pub const LINE_CODING: [u8; 7] = [
    0x00, 0xc2, 0x01, 0x00, // dwDTERate = 115200, little-endian
    0x00, // bCharFormat = 1 stop bit
    0x00, // bParityType = none
    0x08, // bDataBits = 8
];

/// Decode `SET_LINE_CODING`'s seven bytes into a rate, for a log line or a sanity check. Returns
/// `None` when the payload is the wrong size, so a short read cannot be read as a rate of zero.
pub fn line_coding_rate(payload: &[u8]) -> Option<u32> {
    if payload.len() != 7 {
        return None;
    }
    Some(u32::from_le_bytes([
        payload[0], payload[1], payload[2], payload[3],
    ]))
}

/// What the caller should do with a setup packet, in terms the EP0 path already speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send `length` bytes that have been written into the response buffer.
    DataIn(usize),
    /// Ours, but not answerable yet: `SET_LINE_CODING` carries a host-to-device data stage that no
    /// path here can arm. Stalling is the honest failure; see `handle` for why silence is worse.
    Unsupported,
    /// Set (or clear) DTR. The caller latches it; the port being open is what it means.
    Dtr(bool),
    /// Not a CDC-ACM request; let the ordinary path have it.
    NotOurs,
}

/// Handle one setup packet for this function, writing any reply into `response`.
///
/// This is the whole class-request surface in one place: three requests, and getting any of them
/// wrong makes the host's `open()` fail *after* it has already enumerated us successfully, which is
/// the least debuggable place to fail. So it is a pure function of the packet, with tests.
///
/// `packet` is the eight setup bytes as they arrive: bmRequestType, bRequest, wValue (little
/// endian), wIndex, wLength (little endian).
pub fn handle(packet: &[u8; 8], response: &mut [u8]) -> Action {
    let request_type = packet[0];
    let request = packet[1];
    let value = u16::from_le_bytes([packet[2], packet[3]]);
    let requested = u16::from_le_bytes([packet[6], packet[7]]) as usize;
    // Only interface-targeted class requests belong here. A request to the device or to another
    // interface is somebody else's, and answering it would be worse than ignoring it.
    if packet[4] != CONTROL_INTERFACE {
        return Action::NotOurs;
    }
    match classify_with_value(request_type, request, value) {
        ClassRequest::GetLineCoding => {
            if requested == 0 {
                return Action::NotOurs;
            }
            let length = LINE_CODING.len().min(requested).min(response.len());
            response[..length].copy_from_slice(&LINE_CODING[..length]);
            Action::DataIn(length)
        }
        // Not answered on purpose. The seven bytes the host sends must be consumed by an OUT data
        // stage before a status stage is legal, and no path in this crate arms one for EP0 yet.
        // Returning "accepted" here would leave the host mid-transfer with its bytes unread.
        ClassRequest::SetLineCoding => Action::Unsupported,
        ClassRequest::SetControlLineState { dtr, .. } => Action::Dtr(dtr),
        ClassRequest::Other => Action::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    //! The descriptor set is asserted field by field rather than as a blob: a blob test would pass
    //! just as happily with the union's interface numbers swapped, which is the one thing in here
    //! that makes Linux refuse to bind.

    use super::*;

    /// Walk the descriptor set using only the `bLength` fields, as a host does. This is the check
    /// that the set is self-consistent: every step must land exactly on the next descriptor.
    fn walked() -> ([(u8, u8, usize); 16], usize) {
        let mut out = [(0u8, 0u8, 0usize); 16];
        let mut count = 0usize;
        let mut offset = 0usize;
        while offset < FUNCTION_DESCRIPTOR.len() {
            let length = FUNCTION_DESCRIPTOR[offset] as usize;
            assert!(length >= 2, "a descriptor shorter than its own header");
            assert!(
                offset + length <= FUNCTION_DESCRIPTOR.len(),
                "descriptor runs past the end at offset {offset}"
            );
            out[count] = (FUNCTION_DESCRIPTOR[offset + 1], length as u8, offset);
            count += 1;
            offset += length;
        }
        assert_eq!(
            offset,
            FUNCTION_DESCRIPTOR.len(),
            "the set must end exactly"
        );
        (out, count)
    }

    #[test]
    fn every_descriptor_chains_exactly_into_the_next() {
        let (walked, count) = walked();
        let mut kinds = [0u8; 16];
        for index in 0..count {
            kinds[index] = walked[index].0;
        }
        let expected = [
            4, // interface, control
            0x24, 0x24, 0x24, 0x24, // four functional descriptors
            5,    // endpoint, notification
            4,    // interface, data
            5, 5, // two endpoints
        ];
        assert_eq!(&kinds[..count], &expected[..]);
    }

    #[test]
    fn the_union_names_the_two_interfaces_that_are_one_function() {
        let (walked, count) = walked();
        // The union is the functional descriptor whose length is 5 and whose subtype is 6, and its
        // payload is the master interface followed by the slave interface.
        let mut unions = 0usize;
        let mut union_offset = 0usize;
        for index in 0..count {
            let (kind, length, offset) = walked[index];
            if kind == 0x24 && length as usize == 5 && FUNCTION_DESCRIPTOR[offset + 2] == 0x06 {
                unions += 1;
                union_offset = offset;
            }
        }
        assert_eq!(unions, 1, "exactly one union descriptor");
        assert_eq!(FUNCTION_DESCRIPTOR[union_offset + 3], CONTROL_INTERFACE);
        assert_eq!(FUNCTION_DESCRIPTOR[union_offset + 4], DATA_INTERFACE);
    }

    #[test]
    fn the_two_interfaces_declare_the_classes_the_host_drivers_match_on() {
        let (walked, count) = walked();
        let mut interfaces = [0usize; 4];
        let mut found = 0usize;
        for index in 0..count {
            let (kind, _, offset) = walked[index];
            if kind == 4 {
                interfaces[found] = offset;
                found += 1;
            }
        }
        assert_eq!(found, 2);
        let control = interfaces[0];
        assert_eq!(FUNCTION_DESCRIPTOR[control + 5], CDC_CLASS_COMMUNICATIONS);
        assert_eq!(FUNCTION_DESCRIPTOR[control + 6], CDC_SUBCLASS_ACM);
        assert_eq!(FUNCTION_DESCRIPTOR[control + 7], CDC_PROTOCOL_AT);
        assert_eq!(
            FUNCTION_DESCRIPTOR[control + 4],
            1,
            "one notification endpoint"
        );
        let data = interfaces[1];
        assert_eq!(FUNCTION_DESCRIPTOR[data + 5], CDC_CLASS_DATA);
        assert_eq!(FUNCTION_DESCRIPTOR[data + 4], 2, "a bulk pair");
    }

    #[test]
    fn the_endpoints_are_the_ones_the_transport_already_uses() {
        let (walked, count) = walked();
        let mut endpoints = [0usize; 4];
        let mut found = 0usize;
        for index in 0..count {
            let (kind, _, offset) = walked[index];
            if kind == 5 {
                endpoints[found] = offset;
                found += 1;
            }
        }
        assert_eq!(found, 3);
        let endpoints = &endpoints[..found];
        for endpoint in endpoints {
            let address = FUNCTION_DESCRIPTOR[*endpoint + 2];
            assert!(
                address == NOTIFICATION_ENDPOINT
                    || address == DATA_IN_ENDPOINT
                    || address == DATA_OUT_ENDPOINT,
                "unexpected endpoint address {address:#04x}"
            );
        }
        // The notification endpoint is interrupt (3) and the data pair is bulk (2).
        assert_eq!(FUNCTION_DESCRIPTOR[endpoints[0] + 3], 3);
        assert_eq!(FUNCTION_DESCRIPTOR[endpoints[1] + 3], 2);
        assert_eq!(FUNCTION_DESCRIPTOR[endpoints[2] + 3], 2);
    }

    #[test]
    fn the_class_requests_are_told_apart_by_direction_as_well_as_by_code() {
        assert_eq!(classify(0x21, SET_LINE_CODING), ClassRequest::SetLineCoding);
        assert_eq!(classify(0xa1, GET_LINE_CODING), ClassRequest::GetLineCoding);
        // The same request byte in the other direction is not this class's business.
        assert_eq!(classify(0xa1, SET_LINE_CODING), ClassRequest::Other);
        assert_eq!(classify(0x21, GET_LINE_CODING), ClassRequest::Other);
        assert_eq!(classify(0x21, 0x23), ClassRequest::Other);
    }

    #[test]
    fn dtr_is_read_from_wvalue() {
        assert_eq!(
            classify_with_value(0x21, SET_CONTROL_LINE_STATE, 0x0001),
            ClassRequest::SetControlLineState {
                dtr: true,
                rts: false
            }
        );
        assert_eq!(
            classify_with_value(0x21, SET_CONTROL_LINE_STATE, 0x0003),
            ClassRequest::SetControlLineState {
                dtr: true,
                rts: true
            }
        );
        assert_eq!(
            classify_with_value(0x21, SET_CONTROL_LINE_STATE, 0x0000),
            ClassRequest::SetControlLineState {
                dtr: false,
                rts: false
            }
        );
    }

    /// Build a setup packet the way the wire carries it.
    fn setup(request_type: u8, request: u8, value: u16, index: u8, length: u16) -> [u8; 8] {
        let v = value.to_le_bytes();
        let l = length.to_le_bytes();
        [request_type, request, v[0], v[1], index, 0, l[0], l[1]]
    }

    #[test]
    fn get_line_coding_answers_with_seven_bytes() {
        let mut response = [0u8; 64];
        let packet = setup(0xa1, GET_LINE_CODING, 0, CONTROL_INTERFACE, 7);
        assert_eq!(handle(&packet, &mut response), Action::DataIn(7));
        assert_eq!(&response[..7], &LINE_CODING);
        // A shorter request must be answered with a shorter reply, not with seven bytes.
        let packet = setup(0xa1, GET_LINE_CODING, 0, CONTROL_INTERFACE, 4);
        assert_eq!(handle(&packet, &mut response), Action::DataIn(4));
    }

    #[test]
    fn set_line_coding_and_control_line_state_are_accepted() {
        let mut response = [0u8; 64];
        // `SET_LINE_CODING` is deliberately unsupported until an OUT data stage can be armed: the
        // assertion pins that decision so it cannot drift into a silent "accepted".
        assert_eq!(
            handle(
                &setup(0x21, SET_LINE_CODING, 0, CONTROL_INTERFACE, 7),
                &mut response
            ),
            Action::Unsupported
        );
        assert_eq!(
            handle(
                &setup(0x21, SET_CONTROL_LINE_STATE, 1, CONTROL_INTERFACE, 0),
                &mut response
            ),
            Action::Dtr(true)
        );
        assert_eq!(
            handle(
                &setup(0x21, SET_CONTROL_LINE_STATE, 0, CONTROL_INTERFACE, 0),
                &mut response
            ),
            Action::Dtr(false)
        );
    }

    #[test]
    fn requests_for_other_interfaces_or_other_classes_are_left_alone() {
        let mut response = [0u8; 64];
        // Another interface number: not this function's request.
        assert_eq!(
            handle(&setup(0xa1, GET_LINE_CODING, 0, 9, 7), &mut response),
            Action::NotOurs
        );
        // A standard request, and an unknown class request.
        assert_eq!(
            handle(
                &setup(0x80, 6, 0x0100, CONTROL_INTERFACE, 18),
                &mut response
            ),
            Action::NotOurs
        );
        assert_eq!(
            handle(&setup(0x21, 0x23, 0, CONTROL_INTERFACE, 0), &mut response),
            Action::NotOurs
        );
    }

    #[test]
    fn the_line_coding_reply_is_the_shape_the_host_parses() {
        assert_eq!(LINE_CODING.len(), 7);
        assert_eq!(line_coding_rate(&LINE_CODING), Some(115_200));
        assert_eq!(LINE_CODING[6], 8, "eight data bits");
        // A short payload must not be read as a rate.
        assert_eq!(line_coding_rate(&LINE_CODING[..4]), None);
    }
}
