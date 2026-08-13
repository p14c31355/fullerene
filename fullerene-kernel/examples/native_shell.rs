#![no_std]
#![no_main]

//! Native launchd shell entry point.
//!
//! The process is a normal user ELF owned by launchd.  The Nozzle command
//! runtime still needs the kernel VFS/desktop service callbacks, so this small
//! ABI bridge enters Nozzle on this process's terminal and returns when the
//! user exits.  Keeping the bridge here preserves the #340 shell contract
//! without making shell startup a kernel boot side effect.

use core::arch::asm;

const EXIT: u64 = 1;
const RUN_NOZZLE: u64 = 116;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let result = unsafe { syscall(RUN_NOZZLE, 0, 0, 0) };
    unsafe {
        let _ = syscall(EXIT, (result != 0) as u64, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}
