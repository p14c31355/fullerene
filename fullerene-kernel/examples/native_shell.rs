#![no_std]
#![no_main]

//! The first userland shell for Fullerene.
//!
//! This deliberately talks only to the native syscall ABI.  It is kept small
//! while the userland command/service ABI grows; importantly, it is no longer
//! linked into the kernel or entered as a kernel task.

use core::arch::asm;

const EXIT: u64 = 1;
const READ: u64 = 3;
const WRITE: u64 = 4;
const GETPID: u64 = 20;
const YIELD: u64 = 22;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(
    destination: *mut u8,
    source: *const u8,
    length: usize,
) -> *mut u8 {
    if (destination as usize) <= (source as usize) {
        for index in 0..length {
            unsafe { destination.add(index).write(source.add(index).read()) };
        }
    } else {
        for index in (0..length).rev() {
            unsafe { destination.add(index).write(source.add(index).read()) };
        }
    }
    destination
}

unsafe fn syscall(number: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") number,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

fn write(bytes: &[u8]) {
    unsafe {
        let _ = syscall(WRITE, 1, bytes.as_ptr() as u64, bytes.len() as u64);
    }
}

fn write_decimal(mut value: u64, output: &mut [u8; 24]) -> usize {
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
    let length = output.len() - cursor;
    output.copy_within(cursor.., 0);
    length
}

fn equals(line: &[u8], literal: &[u8]) -> bool {
    line == literal
}

fn starts_with(line: &[u8], prefix: &[u8]) -> bool {
    line.len() >= prefix.len() && line[..prefix.len()] == *prefix
}

fn run_command(line: &[u8]) -> bool {
    if equals(line, b"exit") {
        unsafe { syscall(EXIT, 0, 0, 0) };
        return false;
    }
    if equals(line, b"help") {
        write(b"commands: help echo pid clear exit\n");
    } else if equals(line, b"pid") {
        let mut decimal = [0u8; 24];
        let length = write_decimal(unsafe { syscall(GETPID, 0, 0, 0) }, &mut decimal);
        write(b"pid=");
        write(&decimal[..length]);
        write(b"\n");
    } else if equals(line, b"clear") {
        write(b"\x1b[2J\x1b[H");
    } else if starts_with(line, b"echo ") {
        write(&line[5..]);
        write(b"\n");
    } else if !line.is_empty() {
        write(b"shell: command not found: ");
        write(line);
        write(b"\n");
    }
    true
}

fn shell() -> ! {
    let mut line = [0u8; 256];
    let mut length = 0usize;
    write(b"Fullerene user shell\nType 'help' for commands.\n");
    loop {
        write(b"fullerene> ");
        length = 0;
        loop {
            let mut byte = [0u8; 1];
            let read = unsafe { syscall(READ, 0, byte.as_mut_ptr() as u64, 1) };
            if read == 0 {
                unsafe { syscall(YIELD, 0, 0, 0) };
                continue;
            }
            match byte[0] {
                b'\n' | b'\r' => {
                    write(b"\n");
                    break;
                }
                8 | 127 if length > 0 => {
                    length -= 1;
                    write(b"\x08 \x08");
                }
                ch if ch >= 0x20 && length < line.len() => {
                    line[length] = ch;
                    length += 1;
                    write(&byte);
                }
                _ => {}
            }
        }
        if !run_command(&line[..length]) {
            unsafe { syscall(EXIT, 0, 0, 0) };
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    shell()
}
