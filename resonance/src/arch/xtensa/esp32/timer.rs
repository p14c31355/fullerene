//! ESP32 hardware-timer event source.

pub const TIMER0_BASE: usize = 0x3ff5_f000;
const LOAD: usize = 0x00;
const CONTROL: usize = 0x08;
const INT_CLEAR: usize = 0x0c;

#[derive(Clone, Copy, Debug)]
pub enum TimerEvent {
    Tick,
    DeadlineMissed,
}

pub fn init() {
    let write = |offset: usize, value: u32| unsafe {
        (TIMER0_BASE as *mut u32).add(offset).write_volatile(value)
    };
    write(LOAD, 80_000 - 1);
    write(CONTROL, 0x01c);
}

pub fn acknowledge_tick() {
    unsafe { (TIMER0_BASE as *mut u32).add(INT_CLEAR).write_volatile(1) }
}
