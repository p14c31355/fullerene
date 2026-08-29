//! ESP32 SoC register map and primitives.

use core::arch::asm;

pub const UART0_BASE: usize = 0x3ff4_0000;
pub const TIMER0_BASE: usize = 0x3ff5_f000;
pub const RTC_CNTL_BASE: usize = 0x6000_8000;

#[inline]
pub fn read_register(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

#[inline]
pub fn write_register(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

#[inline]
pub fn modify_register(address: usize, clear: u32, set: u32) {
    let value = read_register(address);
    write_register(address, (value & !clear) | set);
}

pub fn software_reset() -> ! {
    write_register(RTC_CNTL_BASE, 1 << 31);
    loop {
        unsafe { asm!("waiti 15", options(nomem, nostack)) }
    }
}
