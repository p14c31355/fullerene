//! PS/2 mouse packet decoder and controller integration.
//!
//! The driver owns the standard three-byte PS/2 packet state directly. This
//! keeps the input path no_std, removes an obsolete x86_64 dependency, and
//! makes packet resynchronisation independently testable.

use spin::Mutex;
use x86_64::instructions::port::Port;

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
        // Bit 3 is always set in the first byte. Discard bytes until a packet
        // boundary is found so an IRQ lost during boot cannot permanently
        // misalign the stream.
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
            // Overflow bits mean the relative delta is not representable.
            return Some((MouseState::new(), status & 0x07));
        }
        let x = i16::from(self.packet[1] as i8);
        let y = i16::from(self.packet[2] as i8);
        Some((MouseState { x, y }, status & 0x07))
    }
}

static DECODER: Mutex<PacketDecoder> = Mutex::new(PacketDecoder::new());
static LATEST_STATE: Mutex<MouseState> = Mutex::new(MouseState::new());
static LATEST_STATUS: Mutex<u8> = Mutex::new(0);
static INITIALIZED: Mutex<bool> = Mutex::new(false);

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

/// Enable default settings and streaming on the second PS/2 port.
pub fn init_mouse() -> Result<(), crate::DriverError> {
    if !mouse_port_present() {
        return Err(crate::DriverError::DeviceNotFound);
    }

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
    *INITIALIZED.lock() = true;
    Ok(())
}

/// Feed one byte from IRQ12 into the packet decoder.
pub fn handle_mouse_data(byte: u8) {
    if !*INITIALIZED.lock() {
        return;
    }
    if let Some((delta, buttons)) = DECODER.lock().push(byte) {
        let mut state = LATEST_STATE.lock();
        state.x = state.x.saturating_add(delta.x);
        state.y = state.y.saturating_add(delta.y);
        *LATEST_STATUS.lock() = buttons;
    }
}

pub fn latest_state() -> MouseState {
    x86_64::instructions::interrupts::without_interrupts(|| *LATEST_STATE.lock())
}

/// Drain accumulated movement while retaining the latest button state.
pub fn consume_state() -> MouseState {
    x86_64::instructions::interrupts::without_interrupts(|| {
        core::mem::take(&mut *LATEST_STATE.lock())
    })
}

pub fn mouse_buttons() -> u8 {
    x86_64::instructions::interrupts::without_interrupts(|| *LATEST_STATUS.lock())
}

#[cfg(test)]
mod tests {
    use super::{MouseState, PacketDecoder};

    #[test]
    fn decodes_signed_relative_motion_and_buttons() {
        let mut decoder = PacketDecoder::new();
        assert_eq!(decoder.push(0x0b), None);
        assert_eq!(decoder.push(0xfe), None);
        assert_eq!(decoder.push(0x05), Some((MouseState { x: -2, y: 5 }, 0x03)));
    }

    #[test]
    fn resynchronises_on_the_first_byte_marker() {
        let mut decoder = PacketDecoder::new();
        assert_eq!(decoder.push(0x01), None);
        assert_eq!(decoder.push(0x08), None);
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
