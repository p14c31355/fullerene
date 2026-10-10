//! aarch64 (ANE) platform layer: the minimal set the phone bring-up needs.
//!
//! Mirrors the names the shared code expects (`serial`, `halt_loop`, ...);
//! the real console on this target is the blackbox ring, not a UART.

pub mod serial;

pub fn halt_loop() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
pub fn cpu_pause() {
    core::hint::spin_loop();
}

#[inline(always)]
pub fn cpu_halt() {
    core::hint::spin_loop();
}

/// There is no legacy UART shim on the ANE; the ring is the console.
pub fn write_serial_bytes(_port: u16, _status_port: u16, _bytes: &[u8]) {}

/// No hardware text cursor on the ANE panel path yet.
pub fn set_hardware_cursor(_pos: usize) {}
