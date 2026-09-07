//! Host side of the legacy Fullerene Rust debug transport.
//!
//! The device also advertises the standard ADB interface and bounded wire
//! framing for connection/return bring-up. Diagnostic commands continue to
//! use FDBG, while `shell:*` and `reboot:*` use the standard ADB wire so the
//! boot-verification path exercises the same transport that platform-tools
//! uses.

use nusb::{
    descriptors::TransferType,
    transfer::{Bulk, Direction, In, Out},
};
use std::{
    fmt::Display,
    io,
    time::{Duration, Instant},
};
use tokio::runtime::Builder;

const VENDOR_ID: u16 = 0x1234;
const PRODUCT_ID: u16 = 0x0001;
const REQUEST_MAGIC: &[u8; 4] = b"FDBG";
const RESPONSE_MAGIC: &[u8; 4] = b"FDRP";
const VERSION: u16 = 1;
const REQUEST_HEADER_BYTES: usize = 16;
const RESPONSE_HEADER_BYTES: usize = 20;
const MAX_RESPONSE: usize = 512;

const ADB_CNXN: u32 = u32::from_le_bytes(*b"CNXN");
const ADB_OPEN: u32 = u32::from_le_bytes(*b"OPEN");
const ADB_OKAY: u32 = u32::from_le_bytes(*b"OKAY");
const ADB_CLSE: u32 = u32::from_le_bytes(*b"CLSE");
const ADB_WRTE: u32 = u32::from_le_bytes(*b"WRTE");
const ADB_VERSION: u32 = 0x0100_0001;
const ADB_MAX_DATA: usize = 4096;
const ADB_HEADER_BYTES: usize = 24;
const ADB_READ_BUFFER_BYTES: usize = 4096;
const ADB_SHELL_V2_STDOUT: u8 = 1;
const ADB_SHELL_V2_STDERR: u8 = 2;
const ADB_SHELL_V2_EXIT: u8 = 3;

const COMMAND_STATUS: u16 = 1;
const COMMAND_TRACE: u16 = 2;
const COMMAND_RETURN: u16 = 3;
const COMMAND_SHELL: u16 = 4;

pub fn run(command: &str) -> io::Result<()> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(other)?
        .block_on(async {
            if is_standard_adb_command(command) {
                run_standard_async(command).await
            } else {
                run_async(command).await
            }
        })
}

fn is_standard_adb_command(command: &str) -> bool {
    command == "reboot"
        || command.starts_with("reboot:")
        || command == "shell"
        || command.starts_with("shell:")
}

/// Wait until exactly one Fullerene USB debug device is visible.
///
/// This deliberately only enumerates USB devices. It does not open an
/// interface or send a request, so the caller can use it as the observation
/// half of a boot-loop before deciding which diagnostic command to issue.
pub fn wait_for_device(timeout: Duration) -> io::Result<()> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(other)?
        .block_on(async move {
            let deadline = Instant::now() + timeout;
            loop {
                let devices = nusb::list_devices().await.map_err(other)?;
                let count = devices
                    .filter(|device| {
                        device.vendor_id() == VENDOR_ID && device.product_id() == PRODUCT_ID
                    })
                    .count();
                match count {
                    1 => return Ok(()),
                    count if count > 1 => {
                        return Err(other(format!(
                            "refusing to observe {count} Fullerene debug devices"
                        )));
                    }
                    _ => {}
                }
                if Instant::now() >= deadline {
                    return Err(other(format!(
                        "Fullerene debug device {VENDOR_ID:04x}:{PRODUCT_ID:04x} did not appear"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
}

async fn run_async(command: &str) -> io::Result<()> {
    let devices = nusb::list_devices().await.map_err(other)?;
    let devices: Vec<_> = devices
        .filter(|device| device.vendor_id() == VENDOR_ID && device.product_id() == PRODUCT_ID)
        .collect();
    let info = match devices.as_slice() {
        [] => {
            return Err(other(format!(
                "no Fullerene debug device found ({VENDOR_ID:04x}:{PRODUCT_ID:04x})"
            )));
        }
        [info] => info,
        many => {
            return Err(other(format!(
                "refusing to choose between {} debug devices",
                many.len()
            )));
        }
    };

    let device = info.open().await.map_err(other)?;
    let interface = device.claim_interface(0).await.map_err(other)?;
    let descriptor = interface
        .descriptors()
        .find(|alternate| {
            alternate
                .endpoints()
                .any(|endpoint| endpoint.transfer_type() == TransferType::Bulk)
        })
        .ok_or_else(|| other("Fullerene interface has no bulk endpoints"))?;
    let out = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::Out
        })
        .ok_or_else(|| other("Fullerene interface has no bulk OUT endpoint"))?;
    let input = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::In
        })
        .ok_or_else(|| other("Fullerene interface has no bulk IN endpoint"))?;

    let mut endpoint_out = interface
        .endpoint::<Bulk, Out>(out.address())
        .map_err(other)?;
    let mut endpoint_in = interface
        .endpoint::<Bulk, In>(input.address())
        .map_err(other)?;
    let (command_id, payload) = encode_command(command)?;
    let request = encode_request(command_id, 1, &payload);
    endpoint_out.submit(request.into());
    endpoint_out
        .next_complete()
        .await
        .into_result()
        .map_err(other)?;

    endpoint_in.submit(nusb::transfer::Buffer::new(MAX_RESPONSE));
    let response = tokio::time::timeout(Duration::from_secs(2), endpoint_in.next_complete())
        .await
        .map_err(|_| other("timed out waiting for Fullerene debug response"))?
        .into_result()
        .map_err(other)?;
    print_response(&response)
}

/// Exercise the standard ADB connection and stream framing.
///
/// This is deliberately limited to the boot bring-up services. It is enough
/// to make `flasks adb --adb-command reboot:bootloader` use the same CNXN/OPEN
/// exchange as a normal ADB host, while status/trace remain available through
/// the deterministic FDBG diagnostics above.
async fn run_standard_async(command: &str) -> io::Result<()> {
    let devices = nusb::list_devices().await.map_err(other)?;
    let devices: Vec<_> = devices
        .filter(|device| device.vendor_id() == VENDOR_ID && device.product_id() == PRODUCT_ID)
        .collect();
    let info = match devices.as_slice() {
        [] => {
            return Err(other(format!(
                "no Fullerene debug device found ({VENDOR_ID:04x}:{PRODUCT_ID:04x})"
            )));
        }
        [info] => info,
        many => {
            return Err(other(format!(
                "refusing to choose between {} debug devices",
                many.len()
            )));
        }
    };

    let device = info.open().await.map_err(other)?;
    let interface = device.claim_interface(0).await.map_err(other)?;
    let descriptor = interface
        .descriptors()
        .find(|alternate| {
            alternate
                .endpoints()
                .any(|endpoint| endpoint.transfer_type() == TransferType::Bulk)
        })
        .ok_or_else(|| other("Fullerene interface has no bulk endpoints"))?;
    let out = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::Out
        })
        .ok_or_else(|| other("Fullerene interface has no bulk OUT endpoint"))?;
    let input = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::In
        })
        .ok_or_else(|| other("Fullerene interface has no bulk IN endpoint"))?;

    let mut endpoint_out = interface
        .endpoint::<Bulk, Out>(out.address())
        .map_err(other)?;
    let mut endpoint_in = interface
        .endpoint::<Bulk, In>(input.address())
        .map_err(other)?;

    send_adb_frame(
        &mut endpoint_out,
        ADB_CNXN,
        ADB_VERSION,
        ADB_MAX_DATA as u32,
        b"host::features=shell_v2\0",
    )
    .await?;
    let connected = receive_adb_frame(&mut endpoint_in).await?;
    if connected.0 != ADB_CNXN {
        return Err(other(format!(
            "expected ADB CNXN, received {}",
            command_name(connected.0)
        )));
    }
    println!(
        "adb connected: {}",
        String::from_utf8_lossy(&connected.3).trim_end_matches('\0')
    );

    let service = if command == "shell" {
        "shell,v2,raw:".to_owned()
    } else if let Some(shell) = command.strip_prefix("shell:") {
        format!("shell,v2,raw:{shell}")
    } else if command == "reboot" {
        "reboot".to_owned()
    } else {
        command.to_owned()
    };
    let mut service_bytes = service.into_bytes();
    service_bytes.push(0);
    send_adb_frame(&mut endpoint_out, ADB_OPEN, 1, 0, &service_bytes).await?;
    let opened = receive_adb_frame(&mut endpoint_in).await?;
    if opened.0 != ADB_OKAY {
        return Err(other(format!(
            "ADB service open failed with {}",
            command_name(opened.0)
        )));
    }

    if command == "reboot" || command.starts_with("reboot:") {
        println!("ADB service accepted: {command}");
        return Ok(());
    }

    let local_id = opened.2;
    let remote_id = opened.1;
    let mut exit_code = None;
    loop {
        let frame = receive_adb_frame(&mut endpoint_in).await?;
        match frame.0 {
            ADB_WRTE => {
                let (kind, payload) = parse_shell_v2_frame(&frame.3)?;
                match kind {
                    ADB_SHELL_V2_STDOUT | ADB_SHELL_V2_STDERR => {
                        print!("{}", String::from_utf8_lossy(payload));
                    }
                    ADB_SHELL_V2_EXIT => {
                        exit_code = Some(u32::from_le_bytes(payload.try_into().unwrap()));
                    }
                    _ => return Err(other("unknown shell-v2 payload kind")),
                }
                send_adb_frame(&mut endpoint_out, ADB_OKAY, local_id, remote_id, &[]).await?;
            }
            ADB_CLSE => {
                send_adb_frame(&mut endpoint_out, ADB_CLSE, local_id, remote_id, &[]).await?;
                break;
            }
            _ => {
                return Err(other(format!(
                    "unexpected ADB shell frame {}",
                    command_name(frame.0)
                )));
            }
        }
    }
    if exit_code.is_some_and(|code| code != 0) {
        return Err(other(format!(
            "ADB shell exited with status {}",
            exit_code.unwrap()
        )));
    }
    Ok(())
}

async fn send_adb_frame(
    endpoint: &mut nusb::Endpoint<Bulk, Out>,
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: &[u8],
) -> io::Result<()> {
    if payload.len() > ADB_MAX_DATA {
        return Err(other("ADB payload exceeds the bounded transport size"));
    }
    let frame = encode_adb_frame(command, arg0, arg1, payload);
    endpoint.submit(frame.into());
    endpoint
        .next_complete()
        .await
        .into_result()
        .map_err(other)?;
    Ok(())
}

async fn receive_adb_frame(
    endpoint: &mut nusb::Endpoint<Bulk, In>,
) -> io::Result<(u32, u32, u32, Vec<u8>)> {
    endpoint.submit(nusb::transfer::Buffer::new(ADB_READ_BUFFER_BYTES));
    let response = tokio::time::timeout(Duration::from_secs(2), endpoint.next_complete())
        .await
        .map_err(|_| other("timed out waiting for standard ADB response"))?
        .into_result()
        .map_err(other)?;
    parse_adb_frame(&response)
}

fn encode_adb_frame(command: u32, arg0: u32, arg1: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(ADB_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&command.to_le_bytes());
    frame.extend_from_slice(&arg0.to_le_bytes());
    frame.extend_from_slice(&arg1.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&adb_checksum(payload).to_le_bytes());
    frame.extend_from_slice(&(command ^ u32::MAX).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn parse_adb_frame(response: &[u8]) -> io::Result<(u32, u32, u32, Vec<u8>)> {
    if response.len() < ADB_HEADER_BYTES {
        return Err(other("truncated standard ADB header"));
    }
    let command = u32::from_le_bytes(response[..4].try_into().unwrap());
    let arg0 = u32::from_le_bytes(response[4..8].try_into().unwrap());
    let arg1 = u32::from_le_bytes(response[8..12].try_into().unwrap());
    let length = u32::from_le_bytes(response[12..16].try_into().unwrap()) as usize;
    let checksum = u32::from_le_bytes(response[16..20].try_into().unwrap());
    let magic = u32::from_le_bytes(response[20..24].try_into().unwrap());
    if magic != command ^ u32::MAX || length > ADB_MAX_DATA {
        return Err(other("invalid standard ADB header"));
    }
    let end = ADB_HEADER_BYTES
        .checked_add(length)
        .ok_or_else(|| other("standard ADB frame length overflow"))?;
    if end > response.len() {
        return Err(other("truncated standard ADB payload"));
    }
    let payload = &response[ADB_HEADER_BYTES..end];
    if checksum != 0 && checksum != adb_checksum(payload) {
        return Err(other("standard ADB checksum mismatch"));
    }
    Ok((command, arg0, arg1, payload.to_vec()))
}

fn parse_shell_v2_frame(payload: &[u8]) -> io::Result<(u8, &[u8])> {
    let (&kind, body) = payload
        .split_first()
        .ok_or_else(|| other("empty shell-v2 payload"))?;
    match kind {
        ADB_SHELL_V2_STDOUT | ADB_SHELL_V2_STDERR => Ok((kind, body)),
        ADB_SHELL_V2_EXIT if body.len() == 4 => Ok((kind, body)),
        _ => Err(other("invalid shell-v2 payload")),
    }
}

fn adb_checksum(payload: &[u8]) -> u32 {
    payload.iter().fold(0u32, |checksum, byte| {
        checksum.wrapping_add(u32::from(*byte))
    })
}

fn command_name(command: u32) -> String {
    String::from_utf8_lossy(&command.to_le_bytes()).into_owned()
}

fn encode_command(command: &str) -> io::Result<(u16, Vec<u8>)> {
    match command {
        "status" => Ok((COMMAND_STATUS, Vec::new())),
        "trace" => Ok((COMMAND_TRACE, Vec::new())),
        "help" => Ok((COMMAND_SHELL, b"help".to_vec())),
        "return" => Ok((COMMAND_RETURN, Vec::new())),
        value if value.starts_with("shell:") => Ok((COMMAND_SHELL, value[6..].as_bytes().to_vec())),
        value => Err(other(format!(
            "unknown adb command {value:?}; use status, trace, help, return, or shell:<cmd>"
        ))),
    }
}

fn encode_request(command: u16, request_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut request = Vec::with_capacity(REQUEST_HEADER_BYTES + payload.len());
    request.extend_from_slice(REQUEST_MAGIC);
    request.extend_from_slice(&VERSION.to_le_bytes());
    request.extend_from_slice(&command.to_le_bytes());
    request.extend_from_slice(&request_id.to_le_bytes());
    request.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    request.extend_from_slice(payload);
    request
}

fn print_response(response: &[u8]) -> io::Result<()> {
    if response.len() < RESPONSE_HEADER_BYTES || &response[..4] != RESPONSE_MAGIC {
        return Err(other("invalid Fullerene debug response"));
    }
    let version = u16::from_le_bytes([response[4], response[5]]);
    if version != VERSION {
        return Err(other(format!(
            "unsupported Fullerene debug version {version}"
        )));
    }
    let status = i32::from_le_bytes(response[12..16].try_into().unwrap());
    let payload_length = u32::from_le_bytes(response[16..20].try_into().unwrap()) as usize;
    let end = RESPONSE_HEADER_BYTES
        .checked_add(payload_length)
        .ok_or_else(|| other("Fullerene debug response length overflow"))?;
    if end > response.len() {
        return Err(other("truncated Fullerene debug response"));
    }
    let payload = &response[RESPONSE_HEADER_BYTES..end];
    if status != 0 {
        return Err(other(format!(
            "Fullerene debug command failed with status {status}: {}",
            String::from_utf8_lossy(payload).trim()
        )));
    }
    if payload.len() == 8 {
        let head = u32::from_le_bytes(payload[..4].try_into().unwrap());
        let event = u32::from_le_bytes(payload[4..].try_into().unwrap());
        println!("trace_head={head:08x}");
        println!("last_event={event:08x}");
    } else {
        print!("{}", String::from_utf8_lossy(payload));
    }
    Ok(())
}

fn other(error: impl Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ADB_CNXN, ADB_HEADER_BYTES, ADB_MAX_DATA, ADB_SHELL_V2_EXIT, ADB_SHELL_V2_STDOUT,
        adb_checksum, encode_adb_frame, parse_adb_frame, parse_shell_v2_frame,
    };

    #[test]
    fn standard_adb_frame_round_trips() {
        let payload = b"host::features=shell_v2\0";
        let frame = encode_adb_frame(ADB_CNXN, 0x0100_0001, 4096, payload);
        assert_eq!(frame.len(), ADB_HEADER_BYTES + payload.len());
        assert_eq!(
            parse_adb_frame(&frame).unwrap(),
            (ADB_CNXN, 0x0100_0001, 4096, payload.to_vec())
        );
    }

    #[test]
    fn standard_adb_parser_accepts_checksum_elision() {
        let mut frame = encode_adb_frame(ADB_CNXN, 1, 2, b"banner\0");
        frame[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_adb_frame(&frame).unwrap().3, b"banner\0");
    }

    #[test]
    fn standard_adb_parser_rejects_bad_magic_checksum_and_bounds() {
        let mut bad_magic = encode_adb_frame(ADB_CNXN, 1, 2, b"x");
        bad_magic[20..24].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_adb_frame(&bad_magic).is_err());

        let mut bad_checksum = encode_adb_frame(ADB_CNXN, 1, 2, b"x");
        bad_checksum[16..20].copy_from_slice(&(adb_checksum(b"y")).to_le_bytes());
        assert!(parse_adb_frame(&bad_checksum).is_err());

        let mut oversized = encode_adb_frame(ADB_CNXN, 1, 2, b"x");
        oversized[12..16].copy_from_slice(&((ADB_MAX_DATA as u32) + 1).to_le_bytes());
        assert!(parse_adb_frame(&oversized).is_err());
    }

    #[test]
    fn shell_v2_parser_accepts_output_and_exit_frames() {
        assert_eq!(
            parse_shell_v2_frame(&[ADB_SHELL_V2_STDOUT, b'o', b'k']).unwrap(),
            (ADB_SHELL_V2_STDOUT, &b"ok"[..])
        );
        assert_eq!(
            parse_shell_v2_frame(&[ADB_SHELL_V2_EXIT, 0, 0, 0, 0]).unwrap(),
            (ADB_SHELL_V2_EXIT, &[0, 0, 0, 0][..])
        );
        assert!(parse_shell_v2_frame(&[]).is_err());
        assert!(parse_shell_v2_frame(&[ADB_SHELL_V2_EXIT, 0]).is_err());
    }
}
