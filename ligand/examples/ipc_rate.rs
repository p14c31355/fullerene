//! Host-side request/response contract stress example.
//!
//! This exercises the same fixed envelope that DriverKit sends through a
//! kernel channel.  The queue is intentionally bounded to the kernel's
//! current 64-message limit, and the service step echoes one request at a
//! time as a response.  The next stage can replace `LoopbackTransport` with a
//! live kernel endpoint without changing the accounting or validation loop.

use std::collections::VecDeque;
use std::time::Instant;

use fullerene_abi::{
    IPC_CHANNEL_MAX_MESSAGE_SIZE, IPC_CHANNEL_MAX_MESSAGES, IPC_MESSAGE_MAGIC, IPC_MESSAGE_VERSION,
    IpcMessageHeader, ipc_message_flags,
};

const REQUESTS: u64 = 100_000;
const QUEUE_CAPACITY: usize = IPC_CHANNEL_MAX_MESSAGES;
const OPCODE_ECHO: u32 = 1;

struct LoopbackTransport {
    requests: VecDeque<Vec<u8>>,
    responses: VecDeque<Vec<u8>>,
}

impl LoopbackTransport {
    fn new() -> Self {
        Self {
            requests: VecDeque::with_capacity(QUEUE_CAPACITY),
            responses: VecDeque::with_capacity(QUEUE_CAPACITY),
        }
    }

    fn send(&mut self, request: Vec<u8>) -> Result<(), ()> {
        if self.requests.len() >= QUEUE_CAPACITY || request.len() > IPC_CHANNEL_MAX_MESSAGE_SIZE {
            return Err(());
        }
        self.requests.push_back(request);
        Ok(())
    }

    fn service_one(&mut self) -> Result<(), ()> {
        let request = self.requests.pop_front().ok_or(())?;
        let header = parse_header(&request).ok_or(())?;
        if header.flags != ipc_message_flags::REQUEST || header.opcode != OPCODE_ECHO {
            return Err(());
        }

        let response_header = IpcMessageHeader::new(
            header.opcode,
            ipc_message_flags::RESPONSE,
            header.request_id,
            header.payload_len,
        );
        let mut response = response_header.to_ne_bytes().to_vec();
        response.extend_from_slice(&request[IpcMessageHeader::BYTE_SIZE..]);
        if self.responses.len() >= QUEUE_CAPACITY {
            return Err(());
        }
        self.responses.push_back(response);
        Ok(())
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.responses.pop_front()
    }
}

fn parse_header(message: &[u8]) -> Option<IpcMessageHeader> {
    let fixed = message.get(..IpcMessageHeader::BYTE_SIZE)?;
    let header = IpcMessageHeader::from_ne_bytes(fixed.try_into().ok()?);
    (header.magic == IPC_MESSAGE_MAGIC
        && header.version == IPC_MESSAGE_VERSION
        && header.is_valid()
        && header.total_size() == Some(message.len()))
    .then_some(header)
}

fn main() {
    let started = Instant::now();
    let mut transport = LoopbackTransport::new();
    let mut failures = 0u64;

    for request_id in 0..REQUESTS {
        let payload = request_id.to_ne_bytes();
        let header = IpcMessageHeader::new(
            OPCODE_ECHO,
            ipc_message_flags::REQUEST,
            request_id,
            payload.len() as u32,
        );
        let mut request = header.to_ne_bytes().to_vec();
        request.extend_from_slice(&payload);

        let request_ok = transport.send(request).is_ok();
        let service_ok = request_ok && transport.service_one().is_ok();
        let response = service_ok.then(|| transport.recv()).flatten();
        let response_ok = response.as_deref().is_some_and(|message| {
            let Some(header) = parse_header(message) else {
                return false;
            };
            header.flags == ipc_message_flags::RESPONSE
                && header.opcode == OPCODE_ECHO
                && header.request_id == request_id
                && message[IpcMessageHeader::BYTE_SIZE..] == payload
        });

        if !request_ok || !service_ok || !response_ok {
            failures += 1;
        }
    }

    let elapsed = started.elapsed();
    let rate = REQUESTS as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("IPC rate: {failures}/{REQUESTS} request failures ({rate:.0} request/s)");
    assert_eq!(failures, 0, "IPC request/response loss detected");
}
