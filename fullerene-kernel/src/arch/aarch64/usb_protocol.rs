//! USB 2.0 control-endpoint protocol shared by the Bramble gadget and QEMU
//! self-tests.
//!
//! The DWC3 register programming remains platform-specific.  Descriptor
//! selection and EP0 state transitions do not need to be, so keeping them in
//! this small no-alloc module lets QEMU exercise the part of enumeration that
//! currently fails on the phone.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    DataIn(usize),
    StatusIn,
    StatusOut,
    Setup,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

pub const DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x01, 0x00, 0, 1, 1, 2, 0, 1,
];

pub const CONFIG_DESCRIPTOR: [u8; 18] =
    [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0];

pub const LANGID_DESCRIPTOR: [u8; 4] = [4, 3, 0x09, 0x04];

pub const MANUFACTURER_DESCRIPTOR: [u8; 20] = [
    20, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0,
];

pub const PRODUCT_DESCRIPTOR: [u8; 36] = [
    36, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0, b' ',
    0, b'A', 0, b'A', 0, b'r', 0, b'c', 0, b'h', 0, b'6', 0, b'4', 0,
];

pub fn descriptor(kind: u8, index: u8) -> Option<&'static [u8]> {
    match (kind, index) {
        (1, 0) => Some(&DEVICE_DESCRIPTOR),
        (2, 0) => Some(&CONFIG_DESCRIPTOR),
        (3, 0) => Some(&LANGID_DESCRIPTOR),
        (3, 1) => Some(&MANUFACTURER_DESCRIPTOR),
        (3, 2) => Some(&PRODUCT_DESCRIPTOR),
        _ => None,
    }
}

pub struct Ep0Simulator {
    state: Ep0State,
    address: u8,
    pending_address: Option<u8>,
    configured: bool,
    pending_configured: Option<bool>,
    control_in: bool,
    control_has_data: bool,
}

impl Ep0Simulator {
    pub const fn new() -> Self {
        Self {
            state: Ep0State::Setup,
            address: 0,
            pending_address: None,
            configured: false,
            pending_configured: None,
            control_in: false,
            control_has_data: false,
        }
    }

    pub fn reset(&mut self) {
        self.state = Ep0State::Setup;
        self.address = 0;
        self.pending_address = None;
        self.configured = false;
        self.pending_configured = None;
        self.control_in = false;
        self.control_has_data = false;
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn configured(&self) -> bool {
        self.configured
    }

    pub fn on_setup(&mut self, packet: [u8; 8], response: &mut [u8]) -> ControlAction {
        if self.state != Ep0State::Setup {
            return ControlAction::Unsupported;
        }

        let request_type = packet[0];
        let request = packet[1];
        let value = u16::from_le_bytes([packet[2], packet[3]]);
        let requested_length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
        self.control_in = request_type & 0x80 != 0;
        self.control_has_data = requested_length != 0;

        if request_type == 0x80 && request == 6 {
            let kind = (value >> 8) as u8;
            let index = value as u8;
            if let Some(bytes) = descriptor(kind, index) {
                let length = requested_length.min(bytes.len()).min(response.len());
                response[..length].copy_from_slice(&bytes[..length]);
                self.state = Ep0State::Data;
                return ControlAction::DataIn(length);
            }
        }

        if request_type == 0x80 && request == 0 {
            let length = requested_length.min(2).min(response.len());
            if length != 0 {
                response[..length].fill(0);
            }
            self.state = Ep0State::Data;
            return ControlAction::DataIn(length);
        }

        if request_type == 0x80 && request == 8 {
            let length = requested_length.min(1).min(response.len());
            if length != 0 {
                response[0] = self.configured as u8;
            }
            self.state = Ep0State::Data;
            return ControlAction::DataIn(length);
        }

        if request_type == 0 && request == 5 && value <= 0x7f && requested_length == 0 {
            self.pending_address = Some(value as u8);
            self.control_has_data = false;
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        if request_type == 0 && request == 9 && requested_length == 0 {
            self.pending_configured = Some(value != 0);
            self.control_has_data = false;
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        self.state = Ep0State::Setup;
        ControlAction::Unsupported
    }

    pub fn on_transfer_complete(&mut self) -> ControlAction {
        match self.state {
            Ep0State::Data => {
                self.state = Ep0State::Status;
                if self.control_has_data && self.control_in {
                    ControlAction::StatusOut
                } else {
                    ControlAction::StatusIn
                }
            }
            Ep0State::Status => {
                self.state = Ep0State::Setup;
                if let Some(address) = self.pending_address.take() {
                    self.address = address;
                }
                if let Some(configured) = self.pending_configured.take() {
                    self.configured = configured;
                }
                self.control_in = false;
                self.control_has_data = false;
                ControlAction::Setup
            }
            Ep0State::Setup => ControlAction::Setup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONFIG_DESCRIPTOR, ControlAction, DEVICE_DESCRIPTOR, Ep0Simulator, MANUFACTURER_DESCRIPTOR,
        PRODUCT_DESCRIPTOR,
    };

    #[test]
    fn descriptor_lengths_match_their_headers() {
        for descriptor in [
            &DEVICE_DESCRIPTOR[..],
            &MANUFACTURER_DESCRIPTOR[..],
            &PRODUCT_DESCRIPTOR[..],
        ] {
            assert_eq!(descriptor[0] as usize, descriptor.len());
        }
        assert_eq!(CONFIG_DESCRIPTOR[0], 9);
        assert_eq!(
            u16::from_le_bytes([CONFIG_DESCRIPTOR[2], CONFIG_DESCRIPTOR[3]]),
            18
        );
        assert_eq!(CONFIG_DESCRIPTOR.len(), 18);
    }

    #[test]
    fn get_device_descriptor_reaches_a_new_setup() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 64, 0], &mut response),
            ControlAction::DataIn(18)
        );
        assert_eq!(&response[..18], &DEVICE_DESCRIPTOR);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
    }

    #[test]
    fn set_address_and_configuration_commit_after_status() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0, 5, 7, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.address(), 0);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(ep0.address(), 7);
        assert_eq!(
            ep0.on_setup([0, 9, 1, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert!(ep0.configured());
    }

    #[test]
    fn reset_returns_to_default_state() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        let _ = ep0.on_setup([0, 5, 7, 0, 0, 0, 0, 0], &mut response);
        ep0.reset();
        assert_eq!(ep0.address(), 0);
        assert!(!ep0.configured());
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 18, 0], &mut response),
            ControlAction::DataIn(18)
        );
    }
}
