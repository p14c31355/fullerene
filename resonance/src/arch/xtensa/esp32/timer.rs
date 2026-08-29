//! ESP32 hardware-timer event source.

pub const TIMER0_BASE: usize = 0x3ff5_f000;
const CONTROL: usize = 0x00; // TIMG_T0CONFIG_REG
const ALARM: usize = 0x10; // TIMG_T0ALARMLO_REG
const ALARM_HIGH: usize = 0x14; // TIMG_T0ALARMHI_REG
const LOAD_VALUE: usize = 0x18; // TIMG_T0LOADLO_REG
const LOAD_VALUE_HIGH: usize = 0x1c; // TIMG_T0LOADHI_REG
const LOAD: usize = 0x20; // TIMG_T0LOAD_REG
const INT_ENABLE: usize = 0x98; // TIMG_INT_ENA_TIMERS_REG
const INT_CLEAR: usize = 0xa4; // TIMG_INT_CLR_TIMERS_REG

const TIMER_ENABLE: u32 = 1 << 31;
const TIMER_INCREASE: u32 = 1 << 30;
const TIMER_AUTORELOAD: u32 = 1 << 29;
const TIMER_DIVIDER: u32 = 1 << 13;
const TIMER_LEVEL_INT_ENABLE: u32 = 1 << 11;
const TIMER_ALARM_ENABLE: u32 = 1 << 10;

#[derive(Clone, Copy, Debug)]
pub enum TimerEvent {
    Tick,
    DeadlineMissed,
}

pub fn init() {
    let write = |offset: usize, value: u32| unsafe {
        (TIMER0_BASE as *mut u8)
            .add(offset)
            .cast::<u32>()
            .write_volatile(value)
    };
    write(LOAD_VALUE, 0);
    write(LOAD_VALUE_HIGH, 0);
    write(ALARM, 80_000 - 1);
    write(ALARM_HIGH, 0);
    write(LOAD, 1);
    write(
        CONTROL,
        TIMER_ENABLE
            | TIMER_INCREASE
            | TIMER_AUTORELOAD
            | TIMER_DIVIDER
            | TIMER_LEVEL_INT_ENABLE
            | TIMER_ALARM_ENABLE,
    );
    write(INT_ENABLE, 1);
}

pub fn acknowledge_tick() {
    unsafe {
        (TIMER0_BASE as *mut u8)
            .add(INT_CLEAR)
            .cast::<u32>()
            .write_volatile(1)
    }
}
