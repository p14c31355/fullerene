//! ESP32 GPIO output/input mechanisms used by direct board drivers.

const GPIO_OUT_W1TS: usize = 0x3ff4_4008;
const GPIO_OUT_W1TC: usize = 0x3ff4_400c;
const GPIO_ENABLE_W1TS: usize = 0x3ff4_4024;
const GPIO_ENABLE_W1TC: usize = 0x3ff4_4028;
const GPIO_IN: usize = 0x3ff4_403c;
const GPIO_IN_HIGH: usize = 0x3ff4_4040;
const IO_MUX_BASE: usize = 0x3ff4_9000;

fn io_mux_offset(pin: u8) -> Option<usize> {
    Some(match pin {
        2 => 0x40,
        12 => 0x34,
        13 => 0x38,
        14 => 0x30,
        15 => 0x3c,
        21 => 0x7c,
        25 => 0x24,
        32 => 0x1c,
        33 => 0x20,
        36 => 0x04,
        39 => 0x10,
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

/// Select GPIO function and enable the pad input path. Pins 34..39 are input
/// only; all supported pins remain safe to call here.
pub fn enable_input(pin: u8) {
    let Some(offset) = io_mux_offset(pin) else {
        return;
    };
    unsafe {
        // MCU_SEL=GPIO (2), input enabled. Input-only pads have no output
        // enable; writing the clear register is harmless for them.
        ((IO_MUX_BASE + offset as usize) as *mut u32)
            .write_volatile(1 << 9 | 2 << 12 | 2 << 10);
        if pin < 34 {
            (GPIO_ENABLE_W1TC as *mut u32).write_volatile(1 << u32::from(pin));
        }
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

/// Read a GPIO input. Returns `None` for pins without a configured IO_MUX
/// mapping rather than silently returning an arbitrary value.
pub fn input(pin: u8) -> Option<bool> {
    if pin >= 32 {
        let value = unsafe { (GPIO_IN_HIGH as *const u32).read_volatile() };
        Some(value & (1 << (u32::from(pin) - 32)) != 0)
    } else {
        let value = unsafe { (GPIO_IN as *const u32).read_volatile() };
        Some(value & (1 << u32::from(pin)) != 0)
    }
}
