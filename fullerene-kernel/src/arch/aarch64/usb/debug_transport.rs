//! Rust control transports for a running Bramble image.
//!
//! The legacy `FDBG/FDRP` channel remains available for the bring-up loop. The
//! same bulk pair also accepts the AOSP ADB transport framing. The current
//! service set is intentionally bounded, but shell-v2 output and exit frames
//! are kept wire-compatible with a normal ADB host.

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

const ADB_CNXN: u32 = 0x4e58_4e43;
const ADB_OPEN: u32 = 0x4e45_504f;
const ADB_OKAY: u32 = 0x5941_4b4f;
const ADB_CLSE: u32 = 0x4553_4c43;
const ADB_WRTE: u32 = 0x4554_5257;
const ADB_VERSION_ORIGINAL: u32 = 0x0100_0000;
const ADB_VERSION: u32 = 0x0100_0001;
const ADB_MAX_DATA: usize = 4096;
const ADB_HEADER_BYTES: usize = 24;
const ADB_STREAM_NONE: u8 = 0;
const ADB_STREAM_RETURN: u8 = 1;
const ADB_STREAM_SHELL: u8 = 2;
const ADB_SHELL_FOLLOWUP_OUTPUT: u8 = 1;
const ADB_SHELL_COMMAND_CAPACITY: usize = 128;
const ADB_STREAM_SHELL_V2: u8 = 3;
const ADB_SHELL_V2_STDOUT: u8 = 1;
const ADB_SHELL_V2_EXIT: u8 = 3;

#[repr(C, align(64))]
struct ResponseBuffer([u8; MAX_FRAME_BYTES]);

#[unsafe(link_section = ".usb_dma")]
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; MAX_FRAME_BYTES]);
static mut RESPONSE_PENDING: bool = false;
static mut RETURN_AFTER_RESPONSE: bool = false;
static mut ADB_CONNECTED: bool = false;
static mut ADB_SKIP_CHECKSUM: bool = false;
static mut ADB_HOST_ID: u32 = 0;
static mut ADB_DEVICE_ID: u32 = 1;
static mut ADB_STREAM_KIND: u8 = ADB_STREAM_NONE;
static mut ADB_SHELL_FOLLOWUP: u8 = 0;
static mut ADB_SHELL_CLOSE_AFTER_OKAY: bool = false;
static mut ADB_SHELL_V2_OUTPUT_SENT: bool = false;
static mut ADB_SHELL_V2: bool = false;
static mut ADB_SHELL_EXIT_CODE: u32 = 0;
static mut ADB_SHELL_COMMAND: [u8; ADB_SHELL_COMMAND_CAPACITY] = [0; ADB_SHELL_COMMAND_CAPACITY];
static mut ADB_SHELL_COMMAND_LENGTH: usize = 0;

/// Handle one completed bulk OUT packet from EP2.
pub(super) fn on_bulk_out(data: &[u8], error: bool) {
    if error {
        return;
    }
    if let Some((command, arg0, arg1, payload)) = parse_adb_request(data) {
        handle_adb(command, arg0, arg1, payload);
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
    let (send_shell_output, device_id, host_id, return_after, shell_v2) = unsafe {
        let send_shell_output = !error && ADB_SHELL_FOLLOWUP == ADB_SHELL_FOLLOWUP_OUTPUT;
        let device_id = ADB_DEVICE_ID;
        let host_id = ADB_HOST_ID;
        let return_after = !error && RETURN_AFTER_RESPONSE;
        let shell_v2 = ADB_SHELL_V2;
        ADB_SHELL_FOLLOWUP = 0;
        RESPONSE_PENDING = false;
        if error {
            RETURN_AFTER_RESPONSE = false;
            ADB_SHELL_CLOSE_AFTER_OKAY = false;
            ADB_SHELL_V2_OUTPUT_SENT = false;
            ADB_SHELL_V2 = false;
        }
        (
            send_shell_output,
            device_id,
            host_id,
            return_after,
            shell_v2,
        )
    };

    if send_shell_output {
        let mut output = [0u8; MAX_FRAME_BYTES - ADB_HEADER_BYTES];
        let command = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(ADB_SHELL_COMMAND).cast::<u8>(),
                ADB_SHELL_COMMAND_LENGTH,
            )
        };
        let length = shell_output(command, &mut output);
        if shell_v2 {
            let mut framed = [0u8; MAX_FRAME_BYTES - ADB_HEADER_BYTES];
            let framed_length = shell_v2_stdout(&output[..length], &mut framed);
            unsafe { ADB_SHELL_V2_OUTPUT_SENT = true };
            queue_adb(
                ADB_WRTE,
                device_id,
                host_id,
                &framed[..framed_length],
                false,
            );
        } else {
            unsafe { ADB_SHELL_CLOSE_AFTER_OKAY = true };
            queue_adb(ADB_WRTE, device_id, host_id, &output[..length], false);
        }
    }

    unsafe {
        if return_after {
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
        ADB_CONNECTED = false;
        ADB_SKIP_CHECKSUM = false;
        ADB_HOST_ID = 0;
        ADB_DEVICE_ID = 1;
        ADB_STREAM_KIND = ADB_STREAM_NONE;
        ADB_SHELL_FOLLOWUP = 0;
        ADB_SHELL_CLOSE_AFTER_OKAY = false;
        ADB_SHELL_V2_OUTPUT_SENT = false;
        ADB_SHELL_V2 = false;
        ADB_SHELL_EXIT_CODE = 0;
        ADB_SHELL_COMMAND_LENGTH = 0;
        ADB_SHELL_FOLLOWUP = 0;
        ADB_SHELL_CLOSE_AFTER_OKAY = false;
        ADB_SHELL_V2_OUTPUT_SENT = false;
        ADB_SHELL_V2 = false;
        ADB_SHELL_EXIT_CODE = 0;
        ADB_SHELL_COMMAND = [0; ADB_SHELL_COMMAND_CAPACITY];
        ADB_SHELL_COMMAND_LENGTH = 0;
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

fn parse_adb_request(data: &[u8]) -> Option<(u32, u32, u32, &[u8])> {
    if data.len() < ADB_HEADER_BYTES {
        return None;
    }
    let command = u32::from_le_bytes(data[0..4].try_into().ok()?);
    let arg0 = u32::from_le_bytes(data[4..8].try_into().ok()?);
    let arg1 = u32::from_le_bytes(data[8..12].try_into().ok()?);
    let payload_length = usize::try_from(u32::from_le_bytes(data[12..16].try_into().ok()?)).ok()?;
    let checksum = u32::from_le_bytes(data[16..20].try_into().ok()?);
    let magic = u32::from_le_bytes(data[20..24].try_into().ok()?);
    if magic != command ^ u32::MAX || payload_length > ADB_MAX_DATA {
        return None;
    }
    let end = ADB_HEADER_BYTES.checked_add(payload_length)?;
    if end > data.len() {
        return None;
    }
    let payload = &data[ADB_HEADER_BYTES..end];
    // ADB 1.0.1 permits checksum elision. Accepting the legacy checksum as
    // well keeps the first CNXN exchange compatible with older host tools.
    if checksum != 0 && checksum != adb_checksum(payload) {
        return None;
    }
    Some((command, arg0, arg1, payload))
}

fn adb_checksum(payload: &[u8]) -> u32 {
    payload.iter().fold(0u32, |checksum, byte| {
        checksum.wrapping_add(u32::from(*byte))
    })
}

fn handle_adb(command: u32, arg0: u32, arg1: u32, payload: &[u8]) {
    match command {
        ADB_CNXN => {
            if arg0 < ADB_VERSION_ORIGINAL || arg1 == 0 {
                return;
            }
            unsafe {
                ADB_CONNECTED = true;
                ADB_SKIP_CHECKSUM = arg0 >= ADB_VERSION;
                ADB_HOST_ID = 0;
                ADB_DEVICE_ID = 1;
                ADB_STREAM_KIND = ADB_STREAM_NONE;
            }
            let response_version = if arg0 >= ADB_VERSION {
                ADB_VERSION
            } else {
                ADB_VERSION_ORIGINAL
            };
            let _ = payload;
            queue_adb(
                ADB_CNXN,
                response_version,
                ADB_MAX_DATA as u32,
                b"device::ro.product.name=fullerene;ro.product.model=Pixel 4a (5G);ro.product.device=bramble;",
                false,
            );
        }
        ADB_OPEN if unsafe { ADB_CONNECTED } => {
            if arg0 == 0 || payload.is_empty() {
                queue_adb(ADB_CLSE, 0, arg0, &[], false);
                return;
            }
            let service = payload.split(|byte| *byte == 0).next().unwrap_or(payload);
            let device_id = unsafe {
                ADB_HOST_ID = arg0;
                ADB_DEVICE_ID = ADB_DEVICE_ID.wrapping_add(1).max(1);
                ADB_DEVICE_ID
            };
            if service == b"reboot:bootloader"
                || service == b"reboot:fastboot"
                || service == b"reboot"
            {
                if adb_return_enabled() {
                    unsafe { ADB_STREAM_KIND = ADB_STREAM_RETURN };
                    queue_adb(ADB_OKAY, device_id, arg0, &[], true);
                } else {
                    // Never turn a normal build's ADB connection into a
                    // boot-chain return merely because a host sent reboot.
                    unsafe { ADB_STREAM_KIND = ADB_STREAM_NONE };
                    queue_adb(ADB_CLSE, 0, arg0, &[], false);
                }
            } else if let Some((shell_v2, command)) = parse_shell_service(service) {
                unsafe {
                    ADB_STREAM_KIND = if shell_v2 {
                        ADB_STREAM_SHELL_V2
                    } else {
                        ADB_STREAM_SHELL
                    };
                    ADB_SHELL_V2 = shell_v2;
                    ADB_SHELL_V2_OUTPUT_SENT = false;
                    ADB_SHELL_EXIT_CODE = shell_exit_code(command);
                    ADB_SHELL_COMMAND_LENGTH = command.len().min(ADB_SHELL_COMMAND_CAPACITY);
                    ADB_SHELL_COMMAND[..ADB_SHELL_COMMAND_LENGTH]
                        .copy_from_slice(&command[..ADB_SHELL_COMMAND_LENGTH]);
                    ADB_SHELL_FOLLOWUP = ADB_SHELL_FOLLOWUP_OUTPUT;
                    ADB_SHELL_CLOSE_AFTER_OKAY = false;
                }
                queue_adb(ADB_OKAY, device_id, arg0, &[], false);
            } else {
                unsafe { ADB_STREAM_KIND = ADB_STREAM_NONE };
                queue_adb(ADB_CLSE, 0, arg0, &[], false);
            }
        }
        ADB_OKAY | ADB_WRTE if unsafe { ADB_CONNECTED } => {
            let valid = unsafe {
                arg0 == ADB_HOST_ID && arg1 == ADB_DEVICE_ID && ADB_STREAM_KIND != ADB_STREAM_NONE
            };
            if valid && command == ADB_WRTE {
                let (device_id, host_id) = unsafe { (ADB_DEVICE_ID, ADB_HOST_ID) };
                queue_adb(ADB_OKAY, device_id, host_id, &[], false);
            } else if valid && command == ADB_OKAY {
                let (close, v2_output_sent, device_id, host_id) = unsafe {
                    (
                        ADB_SHELL_CLOSE_AFTER_OKAY,
                        ADB_SHELL_V2_OUTPUT_SENT,
                        ADB_DEVICE_ID,
                        ADB_HOST_ID,
                    )
                };
                if v2_output_sent {
                    unsafe { ADB_SHELL_V2_OUTPUT_SENT = false };
                    let mut exit = [ADB_SHELL_V2_EXIT, 0, 0, 0, 0];
                    exit[1..].copy_from_slice(&unsafe { ADB_SHELL_EXIT_CODE }.to_le_bytes());
                    queue_adb(ADB_WRTE, device_id, host_id, &exit, false);
                    unsafe { ADB_SHELL_CLOSE_AFTER_OKAY = true };
                } else if close {
                    unsafe {
                        ADB_SHELL_CLOSE_AFTER_OKAY = false;
                        ADB_STREAM_KIND = ADB_STREAM_NONE;
                        ADB_SHELL_V2 = false;
                    }
                    queue_adb(ADB_CLSE, device_id, host_id, &[], false);
                }
            }
        }
        ADB_CLSE => unsafe {
            if arg0 == ADB_HOST_ID || arg1 == ADB_DEVICE_ID {
                ADB_STREAM_KIND = ADB_STREAM_NONE;
                ADB_HOST_ID = 0;
                ADB_SHELL_FOLLOWUP = 0;
                ADB_SHELL_CLOSE_AFTER_OKAY = false;
                ADB_SHELL_V2_OUTPUT_SENT = false;
                ADB_SHELL_V2 = false;
                ADB_SHELL_EXIT_CODE = 0;
            }
        },
        _ => {}
    }
}

fn parse_shell_service(service: &[u8]) -> Option<(bool, &[u8])> {
    if service == b"shell" {
        Some((false, &[]))
    } else if let Some(command) = service.strip_prefix(b"shell:") {
        Some((false, command))
    } else if service == b"shell,v2" {
        Some((true, &[]))
    } else if let Some(command) = service.strip_prefix(b"shell,v2:") {
        Some((true, command))
    } else if let Some(command) = service.strip_prefix(b"shell,v2,raw:") {
        Some((true, command))
    } else if let Some(command) = service.strip_prefix(b"shell,v2,TERM:") {
        Some((true, command))
    } else {
        None
    }
}

fn queue_adb(command: u32, arg0: u32, arg1: u32, payload: &[u8], return_after: bool) {
    if payload.len() > MAX_FRAME_BYTES.saturating_sub(ADB_HEADER_BYTES)
        || unsafe { RESPONSE_PENDING }
    {
        return;
    }
    let return_after = return_after && adb_return_enabled();
    let length = ADB_HEADER_BYTES + payload.len();
    unsafe {
        let response = core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(RESPONSE.0).cast::<u8>(),
            MAX_FRAME_BYTES,
        );
        response[..4].copy_from_slice(&command.to_le_bytes());
        response[4..8].copy_from_slice(&arg0.to_le_bytes());
        response[8..12].copy_from_slice(&arg1.to_le_bytes());
        response[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let checksum = if ADB_SKIP_CHECKSUM {
            0
        } else {
            adb_checksum(payload)
        };
        response[16..20].copy_from_slice(&checksum.to_le_bytes());
        response[20..24].copy_from_slice(&(command ^ u32::MAX).to_le_bytes());
        response[24..length].copy_from_slice(payload);
        if super::queue_bulk_transfer(3, core::ptr::addr_of!(RESPONSE.0).cast::<u8>(), length) {
            RESPONSE_PENDING = true;
            RETURN_AFTER_RESPONSE = return_after;
        }
    }
}

fn adb_return_enabled() -> bool {
    option_env!("FULLERENE_AARCH64_DEBUG_RETURN") == Some("1")
}

fn shell_output(command: &[u8], destination: &mut [u8]) -> usize {
    let command = command
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| &command[..length])
        .unwrap_or(command);
    let output: &[u8] = match command {
        b"" | b"id" => b"uid=0(root) gid=0(root) groups=0(root) context=u:r:su:s0\n",
        b"getprop" => {
            b"[ro.debuggable]: [1]\n[ro.hardware]: [bramble]\n[ro.property_service.version]: [2]\n"
        }
        b"getprop ro.debuggable" => b"[1]\n",
        b"getprop ro.hardware" => b"[bramble]\n",
        b"getprop ro.property_service.version" => b"[2]\n",
        b"uname" | b"uname -a" => b"Fullerene bramble 1.0.0 aarch64 GNU/Linux\n",
        b"status" => b"fullerene-debug/1\nreturn=enabled-or-build-gated\n",
        b"true" => b"",
        value if value.starts_with(b"echo ") => &value[5..],
        value => {
            let mut writer = ByteWriter::new(destination);
            let _ = writer.write_str("sh: ");
            let _ = writer.write_bytes(value);
            let _ = writer.write_str(": not found\n");
            return writer.length;
        }
    };
    let length = output.len().min(destination.len());
    destination[..length].copy_from_slice(&output[..length]);
    length
}

fn shell_v2_stdout(output: &[u8], destination: &mut [u8]) -> usize {
    if destination.is_empty() {
        return 0;
    }
    destination[0] = ADB_SHELL_V2_STDOUT;
    let length = output.len().min(destination.len() - 1);
    destination[1..1 + length].copy_from_slice(&output[..length]);
    length + 1
}

fn shell_exit_code(command: &[u8]) -> u32 {
    let command = command
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| &command[..length])
        .unwrap_or(command);
    if matches!(
        command,
        b"" | b"id"
            | b"getprop"
            | b"getprop ro.debuggable"
            | b"getprop ro.hardware"
            | b"getprop ro.property_service.version"
            | b"uname"
            | b"uname -a"
            | b"status"
            | b"true"
    ) || command.starts_with(b"echo ")
    {
        0
    } else {
        127
    }
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

impl ByteWriter<'_> {
    fn write_bytes(&mut self, value: &[u8]) -> fmt::Result {
        let remaining = self.destination.len().saturating_sub(self.length);
        if value.len() > remaining {
            return Err(fmt::Error);
        }
        self.destination[self.length..self.length + value.len()].copy_from_slice(value);
        self.length += value.len();
        Ok(())
    }
}
