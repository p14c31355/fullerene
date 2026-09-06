//! Host side of the Fullerene Rust debug transport.
//!
//! The device presents a vendor-class bulk interface, not Android's ADB
//! function. Keeping this client separate makes the distinction explicit
//! while still providing the status/trace/return loop needed by bring-up.

use nusb::{
    descriptors::TransferType,
    transfer::{Bulk, Direction, In, Out},
};
use std::{fmt::Display, io, time::Duration};
use tokio::runtime::Builder;

const VENDOR_ID: u16 = 0x1234;
const PRODUCT_ID: u16 = 0x0001;
const REQUEST_MAGIC: &[u8; 4] = b"FDBG";
const RESPONSE_MAGIC: &[u8; 4] = b"FDRP";
const VERSION: u16 = 1;
const REQUEST_HEADER_BYTES: usize = 16;
const RESPONSE_HEADER_BYTES: usize = 20;
const MAX_RESPONSE: usize = 512;

const COMMAND_STATUS: u16 = 1;
const COMMAND_TRACE: u16 = 2;
const COMMAND_RETURN: u16 = 3;
const COMMAND_SHELL: u16 = 4;

pub fn run(command: &str) -> io::Result<()> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(other)?
        .block_on(async { run_async(command).await })
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
