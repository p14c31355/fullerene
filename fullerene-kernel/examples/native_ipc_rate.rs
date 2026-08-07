#![no_std]
#![no_main]

//! Minimal native user ELF used by the QEMU IPC smoke path.

use core::arch::asm;

const REQUESTS: u64 = 100_000;
// Keep these standalone literals synchronized with
// `fullerene_abi::syscall_numbers` and `fullerene_abi::IpcMessageHeader`.
const CHANNEL_CREATE: u64 = 80;
const CHANNEL_SEND: u64 = 81;
const CHANNEL_RECV: u64 = 82;
const WRITE: u64 = 4;
const EXIT: u64 = 1;
const IPC_MESSAGE_MAGIC: u32 = 0x4644_4950;
const IPC_MESSAGE_VERSION: u16 = 1;
const IPC_MESSAGE_HEADER_SIZE: usize = 32;
const IPC_MESSAGE_REQUEST: u32 = 1;
const IPC_CHANNEL_MAX_MESSAGE_SIZE: usize = 65_536;
const OPCODE_ECHO: u32 = 1;

#[panic_handler]
fn panic_handler(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(destination: *mut u8, source: *const u8, length: usize) -> *mut u8 {
    for index in 0..length {
        unsafe {
            destination.add(index).write(source.add(index).read());
        }
    }
    destination
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(
    destination: *mut u8,
    source: *const u8,
    length: usize,
) -> *mut u8 {
    if (destination as usize) <= (source as usize) {
        unsafe { memcpy(destination, source, length) }
    } else {
        for index in (0..length).rev() {
            unsafe {
                destination.add(index).write(source.add(index).read());
            }
        }
        destination
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(destination: *mut u8, value: i32, length: usize) -> *mut u8 {
    for index in 0..length {
        unsafe {
            destination.add(index).write(value as u8);
        }
    }
    destination
}

unsafe fn syscall(number: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") number,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

fn failed(value: u64) -> bool {
    (value as i64) < 0
}

fn error_number(value: u64) -> u64 {
    0u64.wrapping_sub(value)
}

fn encode_echo_request(bytes: &mut [u8; IPC_MESSAGE_HEADER_SIZE + 8], value: u64) {
    unsafe {
        core::ptr::write_bytes(bytes.as_mut_ptr(), 0, IPC_MESSAGE_HEADER_SIZE);
    }
    put_u32(&mut bytes[0..4], IPC_MESSAGE_MAGIC);
    put_u16(&mut bytes[4..6], IPC_MESSAGE_VERSION);
    put_u16(&mut bytes[6..8], IPC_MESSAGE_HEADER_SIZE as u16);
    put_u32(&mut bytes[8..12], OPCODE_ECHO);
    put_u32(&mut bytes[12..16], IPC_MESSAGE_REQUEST);
    put_u64(&mut bytes[16..24], value);
    put_u32(&mut bytes[24..28], 8);
    put_u64(&mut bytes[IPC_MESSAGE_HEADER_SIZE..], value);
}

fn put_u16(destination: &mut [u8], value: u16) {
    let bytes = value.to_ne_bytes();
    destination[0] = bytes[0];
    destination[1] = bytes[1];
}

fn put_u32(destination: &mut [u8], value: u32) {
    let bytes = value.to_ne_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        destination[index] = *byte;
    }
}

fn put_u64(destination: &mut [u8], value: u64) {
    let bytes = value.to_ne_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        destination[index] = *byte;
    }
}

fn copy_bytes(destination: &mut [u8], source: &[u8]) {
    for (index, byte) in source.iter().enumerate() {
        destination[index] = *byte;
    }
}

fn valid_echo(bytes: &[u8; IPC_MESSAGE_HEADER_SIZE + 8], request_id: u64) -> bool {
    let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let version = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
    let header_size = u16::from_ne_bytes(bytes[6..8].try_into().unwrap());
    let opcode = u32::from_ne_bytes(bytes[8..12].try_into().unwrap());
    let flags = u32::from_ne_bytes(bytes[12..16].try_into().unwrap());
    let received_id = u64::from_ne_bytes(bytes[16..24].try_into().unwrap());
    let payload_len = u32::from_ne_bytes(bytes[24..28].try_into().unwrap());
    let reserved = u32::from_ne_bytes(bytes[28..32].try_into().unwrap());
    let mut payload = [0u8; 8];
    for (index, byte) in bytes[IPC_MESSAGE_HEADER_SIZE..].iter().enumerate() {
        payload[index] = *byte;
    }
    magic == IPC_MESSAGE_MAGIC
        && version == IPC_MESSAGE_VERSION
        && header_size as usize == IPC_MESSAGE_HEADER_SIZE
        && flags == IPC_MESSAGE_REQUEST
        && opcode == OPCODE_ECHO
        && received_id == request_id
        && payload_len == 8
        && reserved == 0
        && u64::from_ne_bytes(payload) == request_id
}

fn write_decimal(mut value: u64, output: &mut [u8; 32]) -> usize {
    let mut cursor = output.len();
    if value == 0 {
        cursor -= 1;
        output[cursor] = b'0';
    } else {
        while value != 0 {
            cursor -= 1;
            output[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    let len = output.len() - cursor;
    for index in 0..len {
        output[index] = output[cursor + index];
    }
    len
}

fn report(failures: u64, first_phase: u64, first_error: u64) -> ! {
    let mut line = [0u8; 160];
    let prefix = b"IPC kernel rate: ";
    copy_bytes(&mut line[..prefix.len()], prefix);
    let mut cursor = prefix.len();
    let mut decimal = [0u8; 32];
    let len = write_decimal(failures, &mut decimal);
    copy_bytes(&mut line[cursor..cursor + len], &decimal[..len]);
    cursor += len;
    line[cursor] = b'/';
    cursor += 1;
    let len = write_decimal(REQUESTS, &mut decimal);
    copy_bytes(&mut line[cursor..cursor + len], &decimal[..len]);
    cursor += len;
    copy_bytes(
        &mut line[cursor..cursor + b" request failures\n".len()],
        b" request failures\n",
    );
    cursor += b" request failures\n".len();
    if failures != 0 {
        let prefix = b"IPC kernel first failure: phase=";
        copy_bytes(&mut line[cursor..cursor + prefix.len()], prefix);
        cursor += prefix.len();
        let len = write_decimal(first_phase, &mut decimal);
        copy_bytes(&mut line[cursor..cursor + len], &decimal[..len]);
        cursor += len;
        let prefix = b" errno=";
        copy_bytes(&mut line[cursor..cursor + prefix.len()], prefix);
        cursor += prefix.len();
        let len = write_decimal(first_error, &mut decimal);
        copy_bytes(&mut line[cursor..cursor + len], &decimal[..len]);
        cursor += len;
        line[cursor] = b'\n';
        cursor += 1;
    }
    unsafe {
        let _ = syscall(WRITE, 1, line.as_ptr() as u64, cursor as u64);
        let _ = syscall(EXIT, (failures != 0) as u64, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let channel = unsafe { syscall(CHANNEL_CREATE, 0, 0, 0) };
    if failed(channel) {
        report(REQUESTS, 0, error_number(channel));
    }

    let mut failures = 0u64;
    let mut request = [0u8; IPC_MESSAGE_HEADER_SIZE + 8];
    let mut response = [0u8; IPC_MESSAGE_HEADER_SIZE + 8];
    if request.len() > IPC_CHANNEL_MAX_MESSAGE_SIZE {
        report(REQUESTS, 4, 0);
    }

    let mut first_phase = 0u64;
    let mut first_error = 0u64;
    for request_id in 0..REQUESTS {
        encode_echo_request(&mut request, request_id);
        let sent = unsafe {
            syscall(
                CHANNEL_SEND,
                channel,
                request.as_ptr() as u64,
                request.len() as u64,
            )
        };
        if failed(sent) {
            failures += 1;
            if failures == 1 {
                first_phase = 1;
                first_error = error_number(sent);
            }
            continue;
        }

        let received = unsafe {
            syscall(
                CHANNEL_RECV,
                channel,
                response.as_mut_ptr() as u64,
                response.len() as u64,
            )
        };
        if failed(received)
            || received as usize != response.len()
            || !valid_echo(&response, request_id)
        {
            failures += 1;
            if failures == 1 {
                first_phase = if failed(received) { 2 } else { 3 };
                first_error = if failed(received) {
                    error_number(received)
                } else {
                    0
                };
            }
        }
    }
    report(failures, first_phase, first_error)
}
