//! Small Rust-only control transport for a running Bramble image.
//!
//! This is intentionally not the Android ADB wire protocol.  The image is
//! already a vendor-class USB gadget with a bulk IN/OUT pair, so a bounded
//! request/response channel gives the bring-up loop a reliable way to query
//! state and, when explicitly enabled at build time, return to the firmware
//! without pretending that Bionic/adbd is present.

use core::fmt::{self, Write};

const REQUEST_MAGIC: &[u8; 4] = b"FDBG";
const RESPONSE_MAGIC: &[u8; 4] = b"FDRP";
const VERSION: u16 = 1;
const REQUEST_HEADER_BYTES: usize = 16;
const RESPONSE_HEADER_BYTES: usize = 20;
const MAX_FRAME_BYTES: usize = 512;
const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - RESPONSE_HEADER_BYTES;

pub const COMMAND_STATUS: u16 = 1;
pub const COMMAND_TRACE: u16 = 2;
pub const COMMAND_RETURN: u16 = 3;
pub const COMMAND_SHELL: u16 = 4;

const STATUS_OK: i32 = 0;
const STATUS_INVALID_ARGUMENT: i32 = -22;
const STATUS_NOT_SUPPORTED: i32 = -95;
const STATUS_BUSY: i32 = -16;

#[repr(C, align(64))]
struct ResponseBuffer([u8; MAX_FRAME_BYTES]);

#[unsafe(link_section = ".usb_dma")]
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; MAX_FRAME_BYTES]);
static mut RESPONSE_PENDING: bool = false;
static mut RETURN_AFTER_RESPONSE: bool = false;

/// Handle one completed bulk OUT packet from EP2.
pub(super) fn on_bulk_out(data: &[u8], error: bool) {
    if error {
        return;
    }
    let Some((command, request_id, payload)) = parse_request(data) else {
        queue_response(0, 0, STATUS_INVALID_ARGUMENT, b"invalid request\n", false);
        return;
    };

    if unsafe { RESPONSE_PENDING } {
        queue_response(
            command,
            request_id,
            STATUS_BUSY,
            b"response pending\n",
            false,
        );
        return;
    }

    match command {
        COMMAND_STATUS => {
            let mut payload = [0u8; MAX_PAYLOAD_BYTES];
            let length = status_payload(&mut payload);
            queue_response(command, request_id, STATUS_OK, &payload[..length], false);
        }
        COMMAND_TRACE => {
            let mut payload = [0u8; 8];
            payload[..4].copy_from_slice(&super::trace_head().to_le_bytes());
            payload[4..].copy_from_slice(&super::trace_last_event().to_le_bytes());
            queue_response(command, request_id, STATUS_OK, &payload, false);
        }
        COMMAND_RETURN => {
            if option_env!("FULLERENE_AARCH64_DEBUG_RETURN") != Some("1") {
                queue_response(
                    command,
                    request_id,
                    STATUS_NOT_SUPPORTED,
                    b"return disabled at build time\n",
                    false,
                );
            } else {
                queue_response(command, request_id, STATUS_OK, b"returning\n", true);
            }
        }
        COMMAND_SHELL => handle_shell(request_id, payload),
        _ => queue_response(
            command,
            request_id,
            STATUS_NOT_SUPPORTED,
            b"unknown command\n",
            false,
        ),
    }
}

/// Handle completion of the response on EP3 IN.  Returning only after the
/// response transfer has completed makes the host-side command observable.
pub(super) fn on_bulk_in_complete(error: bool) {
    unsafe {
        RESPONSE_PENDING = false;
        if !error && RETURN_AFTER_RESPONSE {
            RETURN_AFTER_RESPONSE = false;
            super::return_to_boot_chain();
        }
    }
}

/// Forget a response that was invalidated by a USB reset or disconnect.
pub(super) fn reset() {
    unsafe {
        RESPONSE_PENDING = false;
        RETURN_AFTER_RESPONSE = false;
    }
}

fn parse_request(data: &[u8]) -> Option<(u16, u32, &[u8])> {
    if data.len() < REQUEST_HEADER_BYTES || &data[..4] != REQUEST_MAGIC {
        return None;
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != VERSION {
        return None;
    }
    let command = u16::from_le_bytes([data[6], data[7]]);
    let request_id = u32::from_le_bytes(data[8..12].try_into().ok()?);
    let payload_length = usize::try_from(u32::from_le_bytes(data[12..16].try_into().ok()?)).ok()?;
    let end = REQUEST_HEADER_BYTES.checked_add(payload_length)?;
    (payload_length <= MAX_FRAME_BYTES - REQUEST_HEADER_BYTES).then_some(())?;
    (end <= data.len()).then_some(())?;
    Some((command, request_id, &data[REQUEST_HEADER_BYTES..end]))
}

fn handle_shell(request_id: u32, payload: &[u8]) {
    match payload {
        b"status" => {
            let mut response = [0u8; MAX_PAYLOAD_BYTES];
            let length = status_payload(&mut response);
            queue_response(
                COMMAND_SHELL,
                request_id,
                STATUS_OK,
                &response[..length],
                false,
            );
        }
        b"trace" => {
            let mut response = [0u8; 8];
            response[..4].copy_from_slice(&super::trace_head().to_le_bytes());
            response[4..].copy_from_slice(&super::trace_last_event().to_le_bytes());
            queue_response(COMMAND_SHELL, request_id, STATUS_OK, &response, false);
        }
        b"help" => queue_response(
            COMMAND_SHELL,
            request_id,
            STATUS_OK,
            b"status trace help return\n",
            false,
        ),
        b"return" => {
            if option_env!("FULLERENE_AARCH64_DEBUG_RETURN") != Some("1") {
                queue_response(
                    COMMAND_SHELL,
                    request_id,
                    STATUS_NOT_SUPPORTED,
                    b"return disabled at build time\n",
                    false,
                );
            } else {
                queue_response(COMMAND_SHELL, request_id, STATUS_OK, b"returning\n", true);
            }
        }
        _ => queue_response(
            COMMAND_SHELL,
            request_id,
            STATUS_NOT_SUPPORTED,
            b"command unavailable\n",
            false,
        ),
    }
}

fn status_payload(destination: &mut [u8]) -> usize {
    let mut writer = ByteWriter::new(destination);
    let _ = write!(
        writer,
        "fullerene-debug/1\ntrace_head={:08x}\nlast_event={:08x}\nreturn={}\n",
        super::trace_head(),
        super::trace_last_event(),
        if option_env!("FULLERENE_AARCH64_DEBUG_RETURN") == Some("1") {
            "enabled"
        } else {
            "disabled"
        },
    );
    writer.length
}

fn queue_response(command: u16, request_id: u32, status: i32, payload: &[u8], return_after: bool) {
    if payload.len() > MAX_PAYLOAD_BYTES || unsafe { RESPONSE_PENDING } {
        return;
    }
    let length = RESPONSE_HEADER_BYTES + payload.len();
    unsafe {
        let response = core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(RESPONSE.0).cast::<u8>(),
            MAX_FRAME_BYTES,
        );
        response[..4].copy_from_slice(RESPONSE_MAGIC);
        response[4..6].copy_from_slice(&VERSION.to_le_bytes());
        response[6..8].copy_from_slice(&command.to_le_bytes());
        response[8..12].copy_from_slice(&request_id.to_le_bytes());
        response[12..16].copy_from_slice(&status.to_le_bytes());
        response[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        response[20..length].copy_from_slice(payload);
        if super::queue_bulk_transfer(3, core::ptr::addr_of!(RESPONSE.0).cast::<u8>(), length) {
            RESPONSE_PENDING = true;
            RETURN_AFTER_RESPONSE = return_after;
        }
    }
}

struct ByteWriter<'a> {
    destination: &'a mut [u8],
    length: usize,
}

impl<'a> ByteWriter<'a> {
    fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            length: 0,
        }
    }
}

impl Write for ByteWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = self.destination.len().saturating_sub(self.length);
        if value.len() > remaining {
            return Err(fmt::Error);
        }
        self.destination[self.length..self.length + value.len()].copy_from_slice(value.as_bytes());
        self.length += value.len();
        Ok(())
    }
}
