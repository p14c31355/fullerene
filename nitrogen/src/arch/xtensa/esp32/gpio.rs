//! ESP32 GPIO output mechanism.

const GPIO_OUT_W1TS: usize = 0x3ff4_4008;
const GPIO_OUT_W1TC: usize = 0x3ff4_400c;
const GPIO_ENABLE_W1TS: usize = 0x3ff4_4024;
const IO_MUX_BASE: usize = 0x3ff4_9000;

const GPIO_ENABLE_REG: usize = 0x3ff4_4020;

fn io_mux_offset(pin: u8) -> Option<usize> {
    Some(match pin {
        2 => 0x40,
        12 => 0x34,
        13 => 0x38,
        14 => 0x3c,
        15 => 0x30,
        21 => 0x14,
        _ => return None,
    })
}

/// Select GPIO function and enable the pad for software-controlled output.
pub fn enable_output(pin: u8) {
    let Some(offset) = io_mux_offset(pin) else {
        return;
    };
    unsafe {
        // MCU_SEL=GPIO (2), drive strength=2, inputs disabled. The display
        // pins are outputs only on this board profile.
        ((IO_MUX_BASE + offset as usize) as *mut u32).write_volatile(2 << 12 | 2 << 10);
        (GPIO_ENABLE_W1TS as *mut u32).write_volatile(1 << u32::from(pin));
    }
}

#[inline]
pub fn set_output_high(pin: u8) {
    unsafe { (GPIO_OUT_W1TS as *mut u32).write_volatile(1 << u32::from(pin)) }
}

#[inline]
pub fn set_output_low(pin: u8) {
    unsafe { (GPIO_OUT_W1TC as *mut u32).write_volatile(1 << u32::from(pin)) }
}
