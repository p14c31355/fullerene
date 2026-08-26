//! Small register-level DWC3 device-mode model for deterministic QEMU tests.

use super::usb_regs::*;

const SETUP_TRB_ADDR: u64 = 0x1000;
const DATA_TRB_ADDR: u64 = 0x1020;
const STATUS_TRB_ADDR: u64 = 0x1040;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandRecord {
    pub endpoint: u8,
    pub command: u32,
    pub param0: u32,
    pub param1: u32,
    pub param2: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EndpointState {
    configured: bool,
    max_packet: u32,
    resource: u32,
    active: bool,
    trb: Trb,
}

pub struct Dwc3DeviceModel {
    dcfg: u32,
    dalepena: u32,
    devten: u32,
    endpoints: [EndpointState; 2],
    commands: [CommandRecord; 32],
    command_count: usize,
    pending_event: Option<u32>,
    setup_packet: [u8; 8],
}

impl Dwc3DeviceModel {
    pub const fn new() -> Self {
        Self {
            dcfg: DCFG_HIGHSPEED,
            dalepena: 0,
            devten: 0,
            endpoints: [
                EndpointState {
                    configured: false,
                    max_packet: 0,
                    resource: 0,
                    active: false,
                    trb: Trb {
                        bpl: 0,
                        bph: 0,
                        size: 0,
                        ctrl: 0,
                    },
                },
                EndpointState {
                    configured: false,
                    max_packet: 0,
                    resource: 0,
                    active: false,
                    trb: Trb {
                        bpl: 0,
                        bph: 0,
                        size: 0,
                        ctrl: 0,
                    },
                },
            ],
            commands: [CommandRecord {
                endpoint: 0,
                command: 0,
                param0: 0,
                param1: 0,
                param2: 0,
            }; 32],
            command_count: 0,
            pending_event: None,
            setup_packet: [0; 8],
        }
    }

    pub fn configure_usb2_control_endpoints(&mut self) -> bool {
        self.dcfg = DCFG_HIGHSPEED;
        if !self.issue_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            return false;
        }
        for endpoint in 0..=1u32 {
            let param0 = DEPCFG_EP_TYPE_CONTROL | (64 << DEPCFG_MAX_PACKET_SHIFT);
            let param1 = DEPCFG_XFER_COMPLETE_EN
                | DEPCFG_XFER_NOT_READY_EN
                | (endpoint << DEPCFG_EP_NUMBER_SHIFT);
            if !self.issue_command(endpoint as u8, DEPCMD_SETEPCONFIG, param0, param1, 0)
                || !self.issue_command(endpoint as u8, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0)
            {
                return false;
            }
        }
        self.dalepena = 0b11;
        self.devten = DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE;
        true
    }

    pub fn queue_setup(&mut self, packet: [u8; 8]) -> bool {
        self.setup_packet = packet;
        let trb = Trb {
            bpl: SETUP_TRB_ADDR as u32,
            bph: (SETUP_TRB_ADDR >> 32) as u32,
            size: 8,
            ctrl: TRB_CONTROL_SETUP | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        };
        self.queue_transfer(0, SETUP_TRB_ADDR, trb)
    }

    pub fn receive_setup(&mut self, packet: [u8; 8]) -> bool {
        if !self.endpoints[0].active {
            return false;
        }
        self.setup_packet = packet;
        self.push_event(endpoint_event(0, EP_EVENT_TRANSFER_COMPLETE, 0))
    }

    pub fn queue_data_in(&mut self, length: usize) -> bool {
        let trb = Trb {
            bpl: DATA_TRB_ADDR as u32,
            bph: (DATA_TRB_ADDR >> 32) as u32,
            size: length as u32,
            ctrl: TRB_CONTROL_DATA | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        };
        self.queue_transfer(1, DATA_TRB_ADDR, trb)
    }

    pub fn queue_status(&mut self, endpoint: u8, has_data: bool) -> bool {
        let trb = Trb {
            bpl: STATUS_TRB_ADDR as u32,
            bph: (STATUS_TRB_ADDR >> 32) as u32,
            size: 0,
            ctrl: (if has_data {
                TRB_CONTROL_STATUS3
            } else {
                TRB_CONTROL_STATUS2
            }) | TRB_HWO
                | TRB_LST
                | TRB_IOC
                | TRB_ISP_IMI,
        };
        self.queue_transfer(endpoint, STATUS_TRB_ADDR, trb)
    }

    pub fn inject_device_event(&mut self, kind: u32) -> bool {
        self.push_event(device_event(kind))
    }

    pub fn inject_transfer_complete(&mut self, endpoint: u32) -> bool {
        self.push_event(endpoint_event(endpoint, EP_EVENT_TRANSFER_COMPLETE, 0))
    }

    pub fn inject_xfer_not_ready(&mut self, endpoint: u32) -> bool {
        self.push_event(endpoint_event(
            endpoint,
            EP_EVENT_XFER_NOT_READY,
            EP_XFER_NOT_READY_STATUS,
        ))
    }

    pub fn pop_event(&mut self) -> Option<u32> {
        let event = self.pending_event.take()?;
        if event & 1 == 0 && endpoint_event_kind(event) == EP_EVENT_TRANSFER_COMPLETE {
            let endpoint = endpoint_from_event(event) as usize;
            if endpoint < self.endpoints.len() {
                self.endpoints[endpoint].active = false;
            }
        }
        Some(event)
    }

    pub fn setup_packet(&self) -> [u8; 8] {
        self.setup_packet
    }

    pub fn endpoint_trb(&self, endpoint: usize) -> Trb {
        self.endpoints[endpoint].trb
    }

    pub fn command_count(&self) -> usize {
        self.command_count
    }

    pub fn last_command(&self) -> Option<CommandRecord> {
        self.command_count
            .checked_sub(1)
            .map(|index| self.commands[index])
    }

    pub fn endpoint_active(&self, endpoint: usize) -> bool {
        self.endpoints[endpoint].active
    }

    pub fn endpoint_configured(&self, endpoint: usize) -> bool {
        self.endpoints[endpoint].configured
    }

    pub fn dalepena(&self) -> u32 {
        self.dalepena
    }

    fn queue_transfer(&mut self, endpoint: u8, address: u64, trb: Trb) -> bool {
        if endpoint > 1 || !self.endpoints[endpoint as usize].configured {
            return false;
        }
        if !self.issue_command(
            endpoint,
            DEPCMD_STARTTRANSFER,
            (address >> 32) as u32,
            address as u32,
            0,
        ) {
            return false;
        }
        self.endpoints[endpoint as usize].active = true;
        self.endpoints[endpoint as usize].trb = trb;
        true
    }

    fn issue_command(
        &mut self,
        endpoint: u8,
        command: u32,
        param0: u32,
        param1: u32,
        param2: u32,
    ) -> bool {
        if endpoint > 1 || self.command_count == self.commands.len() {
            return false;
        }
        let record = CommandRecord {
            endpoint,
            command,
            param0,
            param1,
            param2,
        };
        self.commands[self.command_count] = record;
        self.command_count += 1;

        let state = &mut self.endpoints[endpoint as usize];
        match command & 0xf {
            DEPCMD_DEPSTARTCFG => true,
            DEPCMD_SETEPCONFIG => {
                let configured_endpoint = (param1 >> DEPCFG_EP_NUMBER_SHIFT) & 0x1f;
                if configured_endpoint != endpoint as u32 {
                    return false;
                }
                state.configured = true;
                state.max_packet = (param0 >> DEPCFG_MAX_PACKET_SHIFT) & 0xffff;
                true
            }
            DEPCMD_SETTRANSFRESOURCE => {
                state.resource = param0;
                state.resource != 0
            }
            DEPCMD_STARTTRANSFER => state.configured && state.resource != 0,
            DEPCMD_ENDTRANSFER => {
                state.active = false;
                true
            }
            _ => false,
        }
    }

    fn push_event(&mut self, event: u32) -> bool {
        if self.pending_event.is_some() {
            return false;
        }
        self.pending_event = Some(event);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Dwc3DeviceModel;
    use crate::usb_regs::{
        DEVICE_EVENT_CONNECT_DONE, DEVICE_EVENT_USB_RESET, EP_EVENT_TRANSFER_COMPLETE,
        EP_EVENT_XFER_NOT_READY, EP_XFER_NOT_READY_STATUS, device_event_kind, endpoint_event_kind,
        endpoint_event_status, endpoint_from_event,
    };

    #[test]
    fn control_endpoint_sequence_configures_both_directions() {
        let mut model = Dwc3DeviceModel::new();
        assert!(model.configure_usb2_control_endpoints());
        assert!(model.endpoint_configured(0));
        assert!(model.endpoint_configured(1));
        assert_eq!(model.dalepena(), 0b11);
        assert_eq!(model.command_count(), 5);
        assert!(model.queue_setup([0; 8]));
        assert!(model.endpoint_active(0));
    }

    #[test]
    fn event_encoding_matches_the_hardware_decoder() {
        let mut model = Dwc3DeviceModel::new();
        assert!(model.inject_device_event(DEVICE_EVENT_USB_RESET));
        assert_eq!(
            device_event_kind(model.pop_event().unwrap()),
            DEVICE_EVENT_USB_RESET
        );
        assert!(model.inject_device_event(DEVICE_EVENT_CONNECT_DONE));
        assert_eq!(
            device_event_kind(model.pop_event().unwrap()),
            DEVICE_EVENT_CONNECT_DONE
        );
        assert!(model.inject_transfer_complete(1));
        let complete = model.pop_event().unwrap();
        assert_eq!(endpoint_from_event(complete), 1);
        assert_eq!(endpoint_event_kind(complete), EP_EVENT_TRANSFER_COMPLETE);
        assert!(model.inject_xfer_not_ready(0));
        let not_ready = model.pop_event().unwrap();
        assert_eq!(endpoint_from_event(not_ready), 0);
        assert_eq!(endpoint_event_kind(not_ready), EP_EVENT_XFER_NOT_READY);
        assert_eq!(endpoint_event_status(not_ready), EP_XFER_NOT_READY_STATUS);
    }

    #[test]
    fn transfer_events_keep_the_endpoint_number_after_setup() {
        let mut model = Dwc3DeviceModel::new();
        assert!(model.configure_usb2_control_endpoints());
        assert!(model.queue_setup([0; 8]));
        assert!(model.receive_setup([0x80, 6, 0, 1, 0, 0, 64, 0]));
        assert_eq!(endpoint_from_event(model.pop_event().unwrap()), 0);
        assert!(model.queue_data_in(18));
        assert!(model.inject_transfer_complete(1));
        let event = model.pop_event().unwrap();
        assert_eq!(event, 0x42);
        assert_eq!(endpoint_from_event(event), 1);
    }
}
