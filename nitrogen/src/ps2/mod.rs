//! PS/2 device drivers.
//!
//! This module provides drivers for PS/2 peripherals (mouse, keyboard, etc.)
//! using the underlying port I/O primitives available in Nitrogen.
//!
//! It also includes PS/2 controller initialization, which is essential on
//! real hardware (e.g. InsydeH2O) where the firmware may leave the PS/2
//! controller in a disabled state (ports disabled, interrupts masked).

use x86_64::instructions::port::Port;

pub mod keyboard;
pub mod keymap;
pub mod mouse;

/// PS/2 I/O port addresses
const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;

/// PS/2 controller commands
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_FIRST_PORT: u8 = 0xAD;
const CMD_DISABLE_SECOND_PORT: u8 = 0xA7;
const CMD_ENABLE_FIRST_PORT: u8 = 0xAE;
const CMD_ENABLE_SECOND_PORT: u8 = 0xA8;
const CMD_SELF_TEST: u8 = 0xAA;
const CMD_TEST_FIRST_PORT: u8 = 0xAB;
const CMD_TEST_SECOND_PORT: u8 = 0xA9;
const CMD_WRITE_SECOND_PORT: u8 = 0xD4;

/// Configuration byte bits
const CFG_FIRST_PORT_INTERRUPT: u8 = 1 << 0;
const CFG_SECOND_PORT_INTERRUPT: u8 = 1 << 1;
const CFG_FIRST_PORT_CLOCK: u8 = 1 << 4;
const CFG_SECOND_PORT_CLOCK: u8 = 1 << 5;
const CFG_FIRST_PORT_TRANSLATION: u8 = 1 << 6;

/// Wait for the PS/2 controller input buffer to be empty (bit 1 = 0).
/// Returns `true` if ready within the timeout, `false` otherwise.
fn wait_input_buffer_empty(status_port: &mut Port<u8>) -> bool {
    crate::timing::wait_timeout_us(100_000, || {
        let status: u8 = unsafe { status_port.read() };
        status & 0x02 == 0
    }).is_ok()
}

/// Wait for the PS/2 controller output buffer to be full (bit 0 = 1).
/// Returns `true` if data available within the timeout, `false` otherwise.
fn wait_output_buffer_full(status_port: &mut Port<u8>) -> bool {
    crate::timing::wait_timeout_us(100_000, || {
        let status: u8 = unsafe { status_port.read() };
        status & 0x01 != 0
    }).is_ok()
}

/// Send a command byte to the PS/2 controller and wait for it to be accepted.
fn send_command(command_port: &mut Port<u8>, status_port: &mut Port<u8>, command: u8) -> bool {
    if !wait_input_buffer_empty(status_port) {
        return false;
    }
    unsafe { command_port.write(command) };
    true
}

/// Read a data byte from the PS/2 data port after the output buffer is full.
fn read_data(data_port: &mut Port<u8>, status_port: &mut Port<u8>) -> Option<u8> {
    if !wait_output_buffer_full(status_port) {
        return None;
    }
    Some(unsafe { data_port.read() })
}

/// Write a data byte to the PS/2 data port.
fn write_data(data_port: &mut Port<u8>, status_port: &mut Port<u8>, data: u8) -> bool {
    if !wait_input_buffer_empty(status_port) {
        return false;
    }
    unsafe { data_port.write(data) };
    true
}

/// Write a byte to the second PS/2 port (mouse).
///
/// Data sent to port 0x60 normally goes to the first port (keyboard).
/// To send a command to the second port, we must first send 0xD4 to the
/// command port, then send the data byte to the data port.
fn write_second_port(
    command_port: &mut Port<u8>,
    data_port: &mut Port<u8>,
    status_port: &mut Port<u8>,
    data: u8,
) -> bool {
    if !send_command(command_port, status_port, CMD_WRITE_SECOND_PORT) {
        return false;
    }
    write_data(data_port, status_port, data)
}

/// Read the PS/2 controller configuration byte.
fn read_config_byte(
    command_port: &mut Port<u8>,
    data_port: &mut Port<u8>,
    status_port: &mut Port<u8>,
) -> Option<u8> {
    if !send_command(command_port, status_port, CMD_READ_CONFIG) {
        log::warn!("[ps2] Failed to send READ_CONFIG command");
        return None;
    }
    read_data(data_port, status_port)
}

/// Write the PS/2 controller configuration byte.
fn write_config_byte(
    command_port: &mut Port<u8>,
    data_port: &mut Port<u8>,
    status_port: &mut Port<u8>,
    config: u8,
) -> bool {
    if !send_command(command_port, status_port, CMD_WRITE_CONFIG) {
        log::warn!("[ps2] Failed to send WRITE_CONFIG command");
        return false;
    }
    write_data(data_port, status_port, config)
}

/// Initialize the PS/2 controller and both ports (keyboard + mouse).
///
/// On real hardware (e.g. InsydeH2O-based laptops from 2015), the UEFI
/// firmware may leave the PS/2 controller in a partially or fully disabled
/// state:
///   - Ports may be disabled (no clock)
///   - Interrupts may be masked in the configuration byte
///   - Devices may not be in streaming mode
///
/// This function performs a complete initialization sequence:
///
/// 1. Disable both ports
/// 2. Flush the output buffer
/// 3. Read and update the configuration byte to enable interrupts and
///    translation (scan code set 1 → set 2 translation for keyboard)
/// 4. Perform controller self-test (informational, non-fatal on failure)
/// 5. Perform port tests (informational, non-fatal on failure)
/// 6. Enable both ports
/// 7. Enable keyboard scanning (send 0xF4 to first port)
/// 8. Enable mouse data reporting (reset + enable via second port)
///
/// # Returns
///
/// A bitmask indicating which devices are present:
///   - Bit 0: keyboard port present and operational
///   - Bit 1: mouse port present and operational
pub fn init_ps2_controller() -> u8 {
    log::info!("[ps2] Initializing PS/2 controller...");

    let mut command_port: Port<u8> = Port::new(PS2_COMMAND_PORT);
    let mut data_port: Port<u8> = Port::new(PS2_DATA_PORT);
    let mut status_port: Port<u8> = Port::new(PS2_STATUS_PORT);

    // ── Step 1: Disable both ports ──
    send_command(&mut command_port, &mut status_port, CMD_DISABLE_FIRST_PORT);
    send_command(&mut command_port, &mut status_port, CMD_DISABLE_SECOND_PORT);
    log::info!("[ps2] Both ports disabled");

    // ── Step 2: Flush the output buffer ──
    while wait_output_buffer_full(&mut status_port) {
        let _ = unsafe { data_port.read() };
    }
    log::info!("[ps2] Output buffer flushed");

    // ── Step 3: Read and update configuration byte ──
    let mut present = 0u8;

    match read_config_byte(&mut command_port, &mut data_port, &mut status_port) {
        Some(mut cfg) => {
            log::info!("[ps2] Current config byte: {:#04x}", cfg);

            // Enable interrupts for both ports
            cfg |= CFG_FIRST_PORT_INTERRUPT;
            cfg |= CFG_SECOND_PORT_INTERRUPT;

            // Enable translation (scan code set 2 → set 1) for keyboard port.
            // Our keyboard driver expects set 1 scancodes, but most hardware
            // uses set 2 internally.  Translation must be enabled.
            cfg |= CFG_FIRST_PORT_TRANSLATION;

            // CRITICAL: Clear bits that disable the ports' clocks.
            // On InsydeH2O, the firmware may set these bits to disable
            // unused ports.  We clear them to ensure clocks are running.
            cfg &= !CFG_FIRST_PORT_CLOCK;
            cfg &= !CFG_SECOND_PORT_CLOCK;

            log::info!("[ps2] Updated config byte: {:#04x}", cfg);

            if write_config_byte(&mut command_port, &mut data_port, &mut status_port, cfg) {
                log::info!("[ps2] Configuration byte written successfully");
            } else {
                log::warn!("[ps2] Failed to write configuration byte");
            }
        }
        None => {
            log::warn!("[ps2] Could not read controller configuration byte — PS/2 may be absent");
            return 0;
        }
    }

    // ── Step 4: Controller self-test ──
    if send_command(&mut command_port, &mut status_port, CMD_SELF_TEST) {
        match read_data(&mut data_port, &mut status_port) {
            Some(0x55) => log::info!("[ps2] Controller self-test passed"),
            Some(code) => log::warn!("[ps2] Controller self-test returned {:#04x}", code),
            None => log::warn!("[ps2] Controller self-test: no response"),
        }
    }

    // ── Step 5: Port tests ──
    if send_command(&mut command_port, &mut status_port, CMD_TEST_FIRST_PORT) {
        match read_data(&mut data_port, &mut status_port) {
            Some(0x00) => {
                log::info!("[ps2] First port test passed");
                present |= 1;
            }
            Some(code) => log::warn!("[ps2] First port test returned {:#04x}", code),
            None => log::warn!("[ps2] First port test: no response"),
        }
    }

    if send_command(&mut command_port, &mut status_port, CMD_TEST_SECOND_PORT) {
        match read_data(&mut data_port, &mut status_port) {
            Some(0x00) => {
                log::info!("[ps2] Second port test passed");
                present |= 2;
            }
            Some(code) => log::warn!("[ps2] Second port test returned {:#04x}", code),
            None => log::warn!("[ps2] Second port test: no response"),
        }
    }

    // Re-read config after port tests (they may have reset it)
    match read_config_byte(&mut command_port, &mut data_port, &mut status_port) {
        Some(mut cfg) => {
            cfg |= CFG_FIRST_PORT_INTERRUPT;
            cfg |= CFG_SECOND_PORT_INTERRUPT;
            cfg |= CFG_FIRST_PORT_TRANSLATION;
            cfg &= !CFG_FIRST_PORT_CLOCK;
            cfg &= !CFG_SECOND_PORT_CLOCK;
            write_config_byte(&mut command_port, &mut data_port, &mut status_port, cfg);
        }
        None => {}
    }

    // ── Step 6: Enable both ports ──
    if present & 1 != 0 {
        send_command(&mut command_port, &mut status_port, CMD_ENABLE_FIRST_PORT);
        log::info!("[ps2] First port (keyboard) enabled");
    }
    if present & 2 != 0 {
        send_command(&mut command_port, &mut status_port, CMD_ENABLE_SECOND_PORT);
        log::info!("[ps2] Second port (mouse) enabled");
    }

    // ── Step 7: Enable keyboard scanning ──
    if present & 1 != 0 {
        // Reset keyboard to ensure known state
        if write_data(&mut data_port, &mut status_port, 0xFF) {
            // Wait for ACK (0xFA) and self-test result (0xAA)
            let mut got_ack = false;
            let mut got_bat = false;
            for _ in 0..200_000 {
                match read_data(&mut data_port, &mut status_port) {
                    Some(0xFA) => got_ack = true,
                    Some(0xAA) => got_bat = true,
                    Some(_) => {} // consume other bytes
                    None => break,
                }
            }
            if got_bat {
                log::info!("[ps2] Keyboard reset: BAT passed (0xAA)");
            } else if got_ack {
                log::info!("[ps2] Keyboard reset: ACK received");
            } else {
                log::warn!("[ps2] Keyboard reset: no response");
            }
        }

        // Enable scanning (set default, enable)
        // 0xF6 = set defaults, 0xF4 = enable
        write_data(&mut data_port, &mut status_port, 0xF6); // set default
        if write_data(&mut data_port, &mut status_port, 0xF4) {
            // Wait for ACK
            match read_data(&mut data_port, &mut status_port) {
                Some(0xFA) => log::info!("[ps2] Keyboard scanning enabled"),
                Some(b) => log::warn!("[ps2] Keyboard enable response: {:#04x}", b),
                None => log::warn!("[ps2] Keyboard enable: no response"),
            }
        }
    }

    // ── Step 8: Enable mouse data reporting ──
    if present & 2 != 0 {
        // Reset mouse
        if write_second_port(&mut command_port, &mut data_port, &mut status_port, 0xFF) {
            // Wait for ACK (0xFA), BAT result (0xAA), and device ID (0x00)
            for _ in 0..200_000 {
                match read_data(&mut data_port, &mut status_port) {
                    Some(0xFA) => log::info!("[ps2] Mouse reset: ACK"),
                    Some(0xAA) => log::info!("[ps2] Mouse reset: BAT passed"),
                    Some(0x00) => {
                        log::info!("[ps2] Mouse device ID: 0x00 (standard PS/2 mouse)");
                    }
                    Some(b) => log::info!("[ps2] Mouse: received {:#04x}", b),
                    None => break,
                }
            }
        }

        // Mouse init is handled by nitrogen::ps2::mouse::init_mouse() which
        // sends the full PS/2 mouse initialisation sequence (set defaults,
        // enable streaming, etc.).  We just needed to ensure the port is
        // enabled and the device has been reset.
    }

    log::info!(
        "[ps2] Controller initialization complete (keyboard={}, mouse={})",
        present & 1 != 0,
        present & 2 != 0
    );

    present
}
