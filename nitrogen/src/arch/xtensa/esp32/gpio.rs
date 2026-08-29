//! ESP32 GPIO output mechanism.

const GPIO_OUT_W1TS: usize = 0x3ff4_4008;
const GPIO_OUT_W1TC: usize = 0x3ff4_400c;

#[inline]
pub fn set_output_high(pin: u8) {
    unsafe { (GPIO_OUT_W1TS as *mut u32).write_volatile(1 << u32::from(pin)) }
}

#[inline]
pub fn set_output_low(pin: u8) {
    unsafe { (GPIO_OUT_W1TC as *mut u32).write_volatile(1 << u32::from(pin)) }
}
