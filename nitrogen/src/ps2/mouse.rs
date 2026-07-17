//! PS/2 mouse / touchpad driver backed by the external `ps2-mouse` crate.
//!
//! The external crate handles the low-level PS/2 protocol (including hardware
//! initialisation quirks on real laptops), while the hand-rolled packet
//! decoder serves as a well-tested fallback.  On native hardware the crate's
//! `init()` is tried first; if it fails we fall through to the internal init
//! so the system remains usable even with unusual or legacy controllers.

use ps2_mouse::{Mouse as Ps2MouseInner, MouseState as Ps2MouseState};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Global PS/2 mouse instance backed by the external crate.
pub static MOUSE: Mutex<Option<Ps2MouseInner>> = Mutex::new(None);

/// Relative movement accumulated since the previous poll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseState {
    x: i16,
    y: i16,
}

impl MouseState {
    pub const fn new() -> Self {
        Self { x: 0, y: 0 }
    }

    pub const fn get_x(self) -> i16 {
        self.x
    }

    pub const fn get_y(self) -> i16 {
        self.y
    }
}

/// Three-byte PS/2 packet decoder (kept as a portable fallback).
#[derive(Debug, Clone, Copy)]
struct PacketDecoder {
    packet: [u8; 3],
    index: usize,
}

impl PacketDecoder {
    const fn new() -> Self {
        Self {
            packet: [0; 3],
            index: 0,
        }
    }

    fn push(&mut self, byte: u8) -> Option<(MouseState, u8)> {
        if self.index == 0 && byte & 0x08 == 0 {
            return None;
        }
        self.packet[self.index] = byte;
        self.index += 1;
        if self.index != self.packet.len() {
            return None;
        }
        self.index = 0;

        let status = self.packet[0];
        if status & 0xc0 != 0 {
            return Some((MouseState::new(), status & 0x07));
        }
        let x = decode_axis(self.packet[1], status & 0x10 != 0);
        let y = decode_axis(self.packet[2], status & 0x20 != 0);
        Some((MouseState { x, y }, status & 0x07))
    }
}

fn decode_axis(low: u8, negative: bool) -> i16 {
    let value = i16::from(low);
    if negative { value - 256 } else { value }
}

#[derive(Clone)]
enum Backend {
    External,
    Internal,
}

static DECODER: Mutex<PacketDecoder> = Mutex::new(PacketDecoder::new());
static LATEST_STATE: Mutex<MouseState> = Mutex::new(MouseState::new());
static LATEST_STATUS: Mutex<u8> = Mutex::new(0);
static BACKEND: Mutex<Option<Backend>> = Mutex::new(None);
static PACKET_IDX: Mutex<u8> = Mutex::new(0);

fn mouse_port_present() -> bool {
    let mut status_port: Port<u8> = Port::new(super::PS2_STATUS_PORT);
    let mut data_port: Port<u8> = Port::new(super::PS2_DATA_PORT);
    let mut command_port: Port<u8> = Port::new(super::PS2_COMMAND_PORT);
    super::read_config_byte(&mut command_port, &mut data_port, &mut status_port)
        .is_some_and(|config| config & super::CFG_SECOND_PORT_CLOCK == 0)
}

fn send_mouse_command(
    command_port: &mut Port<u8>,
    data_port: &mut Port<u8>,
    status_port: &mut Port<u8>,
    command: u8,
) -> bool {
    if !super::write_second_port(command_port, data_port, status_port, command) {
        return false;
    }
    matches!(super::read_data(data_port, status_port), Some(0xfa))
}

/// Initialise the PS/2 mouse / touchpad.
///
/// Tries the external `ps2-mouse` crate first.  If that fails we fall back to
/// the hand-rolled init so the driver always has a path forward.
pub fn init_mouse() -> Result<(), crate::DriverError> {
    if !mouse_port_present() {
        return Err(crate::DriverError::DeviceNotFound);
    }

    // ── Attempt 1: external crate ──
    let mut mouse = Ps2MouseInner::new();
    mouse.set_on_complete(|state: Ps2MouseState| {
        let x = state.get_x();
        let y = -state.get_y();
        let mut s = LATEST_STATE.lock();
        s.x = s.x.saturating_add(x);
        s.y = s.y.saturating_add(y);
    });
    match mouse.init() {
        Ok(()) => {
            log::info!("[nitrogen] PS/2 mouse: external crate init succeeded");
            *MOUSE.lock() = Some(mouse);
            *BACKEND.lock() = Some(Backend::External);
            return Ok(());
        }
        Err(e) => {
            log::warn!(
                "[nitrogen] PS/2 mouse: external crate init failed ({:?}), falling back",
                e
            );
        }
    }

    // ── Attempt 2: hand-rolled init ──
    let mut command_port: Port<u8> = Port::new(super::PS2_COMMAND_PORT);
    let mut data_port: Port<u8> = Port::new(super::PS2_DATA_PORT);
    let mut status_port: Port<u8> = Port::new(super::PS2_STATUS_PORT);
    if !super::send_command(
        &mut command_port,
        &mut status_port,
        super::CMD_ENABLE_SECOND_PORT,
    ) || !send_mouse_command(&mut command_port, &mut data_port, &mut status_port, 0xf6)
        || !send_mouse_command(&mut command_port, &mut data_port, &mut status_port, 0xf4)
    {
        return Err(crate::DriverError::DeviceFault);
    }

    *DECODER.lock() = PacketDecoder::new();
    *LATEST_STATE.lock() = MouseState::new();
    *LATEST_STATUS.lock() = 0;
    *BACKEND.lock() = Some(Backend::Internal);
    log::info!("[nitrogen] PS/2 mouse: hand-rolled fallback init succeeded");
    Ok(())
}

/// Feed one byte from IRQ12 into the mouse driver.
pub fn handle_mouse_data(byte: u8) {
    let backend = BACKEND.lock().clone();
    match backend {
        Some(Backend::External) => {
            let mut idx = PACKET_IDX.lock();
            if *idx == 0 {
                *LATEST_STATUS.lock() = byte & 0x07;
            }
            *idx = (*idx + 1) % 3;
            drop(idx);

            if let Some(ref mut mouse) = *MOUSE.lock() {
                mouse.process_packet(byte);
            }
        }
        Some(Backend::Internal) => {
            if let Some((delta, buttons)) = DECODER.lock().push(byte) {
                let mut state = LATEST_STATE.lock();
                state.x = state.x.saturating_add(delta.x);
                state.y = state.y.saturating_add(delta.y);
                *LATEST_STATUS.lock() = buttons;
            }
        }
        None => {}
    }
}

/// Return the current accumulated mouse state without consuming it.
pub fn latest_state() -> MouseState {
    x86_64::instructions::interrupts::without_interrupts(|| *LATEST_STATE.lock())
}

/// Drain accumulated movement while retaining the latest button state.
pub fn consume_state() -> MouseState {
    x86_64::instructions::interrupts::without_interrupts(|| {
        core::mem::take(&mut *LATEST_STATE.lock())
    })
}

/// Return the latest button flags (bit 0 = left, bit 1 = right, bit 2 = middle).
pub fn mouse_buttons() -> u8 {
    x86_64::instructions::interrupts::without_interrupts(|| *LATEST_STATUS.lock())
}

#[cfg(test)]
mod tests {
    use super::{MouseState, PacketDecoder};

    #[test]
    fn decodes_signed_relative_motion_and_buttons() {
        let mut decoder = PacketDecoder::new();
        assert_eq!(decoder.push(0x1b), None);
        assert_eq!(decoder.push(0xfe), None);
        assert_eq!(decoder.push(0x05), Some((MouseState { x: -2, y: 5 }, 0x03)));
    }

    #[test]
    fn decodes_full_nine_bit_axis_range() {
        let mut decoder = PacketDecoder::new();
        decoder.push(0x08);
        decoder.push(0xff);
        assert_eq!(decoder.push(0x80), Some((MouseState { x: 255, y: 128 }, 0)));

        decoder.push(0x38);
        decoder.push(0x00);
        assert_eq!(
            decoder.push(0x7f),
            Some((MouseState { x: -256, y: -129 }, 0))
        );
    }

    #[test]
    fn resynchronises_on_the_first_byte_marker() {
        let mut decoder = PacketDecoder::new();
        assert_eq!(decoder.push(0x01), None);
        assert_eq!(decoder.push(0x28), None);
        assert_eq!(decoder.push(0x01), None);
        assert_eq!(decoder.push(0xff), Some((MouseState { x: 1, y: -1 }, 0)));
    }

    #[test]
    fn overflow_packet_preserves_buttons_without_motion() {
        let mut decoder = PacketDecoder::new();
        decoder.push(0xc9);
        decoder.push(0x7f);
        assert_eq!(decoder.push(0x7f), Some((MouseState::new(), 1)));
    }
}
