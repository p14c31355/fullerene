//! ESP32 SoC register map and primitives.

use core::arch::asm;

pub const UART0_BASE: usize = 0x3ff4_0000;
pub const TIMER0_BASE: usize = 0x3ff5_f000;
pub const RTC_CNTL_BASE: usize = 0x3ff4_8000;
pub const RTC_WDT_CONFIG0: usize = RTC_CNTL_BASE + 0x8c;
pub const RTC_WDT_WPROTECT: usize = RTC_CNTL_BASE + 0xa4;
pub const RTC_WDT_WKEY: u32 = 0x50d8_3aa1;
pub const TIMG0_BASE: usize = 0x3ff5_f000;
pub const TIMG1_BASE: usize = 0x3ff6_0000;
pub const TIMG_WDT_CONFIG0: usize = 0x48;
pub const TIMG_WDT_WPROTECT: usize = 0x64;
pub const TIMG_WDT_WKEY: u32 = 0x50d8_3aa1;

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

/// Disable the ROM-started RTC watchdog before it reboots a stable kernel.
pub fn disable_rtc_wdt() {
    write_register(RTC_WDT_WPROTECT, RTC_WDT_WKEY);
    write_register(RTC_WDT_CONFIG0, 0);
    write_register(RTC_WDT_WPROTECT, 0);
}

/// The ROM/boot image also arms TIMG watchdogs. A TG reset is distinct from
/// the RTC reset we already disable, so bring-up must own both mechanisms.
pub fn disable_timer_group_watchdogs() {
    for base in [TIMG0_BASE, TIMG1_BASE] {
        write_register(base + TIMG_WDT_WPROTECT, TIMG_WDT_WKEY);
        write_register(base + TIMG_WDT_CONFIG0, 0);
        write_register(base + TIMG_WDT_WPROTECT, 0);
    }
}

pub fn software_reset() -> ! {
    write_register(RTC_CNTL_BASE, 1 << 31);
    loop {
        unsafe { asm!("waiti 15", options(nomem, nostack)) }
    }
}
