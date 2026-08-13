//! xHCI HBD backend.
//!
//! This adapter only observes state already owned by `XhciContext` and uses
//! the driver's bounded port poll/reset path for actions.  It does not encode
//! machine or PCI-ID quirks.

use crate::hbd::action::{Action, ActionKind, ActionOutcome};
use crate::hbd::constraint::{ConstraintResult, ConstraintStatus};
use crate::hbd::observation::{Observation, ObservationValue};
use crate::hbd::solver::{SolveResult, SolverBackend, SolverBudget, solve};
use crate::hbd::state::State;
use crate::usb::xhci::context::XhciContext;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Usb2,
    Usb3,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciPortObservation {
    pub index: u32,
    pub protocol: Protocol,
    pub connected: bool,
    pub enabled: bool,
    pub link_state: u32,
    pub speed: u32,
    pub portsc: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XhciDeviceObservation {
    pub address: u8,
    pub root_port: u32,
    pub slot: Option<u32>,
    pub route_valid: bool,
    pub parent_hub_slot: Option<u32>,
    pub endpoint_count: usize,
    pub canonical_path: String,
}

#[derive(Debug, Clone)]
pub struct XhciObservation {
    pub hci_version: u16,
    pub running: bool,
    pub legacy_handoff: bool,
    pub max_slots: u32,
    pub ports: Vec<XhciPortObservation>,
    pub devices: Vec<XhciDeviceObservation>,
}

pub fn observe(controller: &XhciContext) -> XhciObservation {
    let cap = &controller.registers.cap;
    let params = cap.hcs_params1();
    let ports = controller
        .ports
        .ports
        .iter()
        .map(|port| XhciPortObservation {
            index: port.index,
            protocol: if port.is_usb3 {
                Protocol::Usb3
            } else {
                Protocol::Usb2
            },
            connected: port.ccs(),
            enabled: port.ped(),
            link_state: port.pls(),
            speed: port.speed_raw(),
            portsc: port.portsc,
        })
        .collect();
    let devices = controller
        .devices
        .iter()
        .map(|device| XhciDeviceObservation {
            address: device.address,
            root_port: device.port_index,
            // Until xHCI exposes a separate slot field on the common USB model,
            // the current driver assigns the USB address from the slot ID.  Keep
            // this explicitly optional in the canonical observation.
            slot: (device.address != 0).then_some(device.address as u32),
            route_valid: match (device.parent_hub_slot, device.downstream_port) {
                (None, None) => true,
                (Some(_), Some(port)) => port != 0,
                _ => false,
            },
            parent_hub_slot: device.parent_hub_slot,
            endpoint_count: device.endpoints.len(),
            canonical_path: canonical_path(
                controller,
                device.port_index,
                device.parent_hub_slot,
                device.downstream_port,
                device.device_class,
            ),
        })
        .collect();
    XhciObservation {
        hci_version: cap.hci_version,
        running: controller.is_running(),
        legacy_handoff: controller.legacy_handoff_done,
        max_slots: params.max_slots,
        ports,
        devices,
    }
}

/// Convert physical port numbering and hub ancestry into a stable logical
/// path. This is based on observed protocol/topology, not PCI IDs.
pub fn canonical_path(
    controller: &XhciContext,
    root_port: u32,
    parent_hub_slot: Option<u32>,
    downstream_port: Option<u8>,
    device_class: u8,
) -> String {
    let protocol = controller
        .ports
        .get(root_port)
        .map(|port| if port.is_usb3 { "usb3" } else { "usb2" })
        .unwrap_or("usb");
    let kind = match device_class {
        crate::usb::MSC_CLASS => "mass-storage",
        0x09 => "hub",
        _ => "device",
    };
    let mut path = String::from("xhci://");
    path.push_str(protocol);
    path.push_str("/root/");
    if let (Some(slot), Some(port)) = (parent_hub_slot, downstream_port) {
        use core::fmt::Write;
        let _ = write!(path, "hub-{}/port-{}/{}", slot, port, kind);
    } else {
        path.push_str(kind);
    }
    path
}

pub struct XhciBackend<'a> {
    controller: &'a mut XhciContext,
    last: Option<XhciObservation>,
}

impl<'a> XhciBackend<'a> {
    pub fn new(controller: &'a mut XhciContext) -> Self {
        Self {
            controller,
            last: None,
        }
    }
    pub fn snapshot(controller: &XhciContext) -> XhciObservation {
        observe(controller)
    }
    pub fn solve(controller: &'a mut XhciContext, budget: SolverBudget) -> SolveResult {
        solve(&mut Self::new(controller), budget)
    }
    fn current(&self) -> Option<&XhciObservation> {
        self.last.as_ref()
    }
}

impl SolverBackend for XhciBackend<'_> {
    fn name(&self) -> &'static str {
        "xhci"
    }

    fn observe(&mut self) -> Vec<Observation> {
        let snapshot = observe(self.controller);
        let mut records = Vec::new();
        records.push(Observation {
            key: "controller.running",
            value: ObservationValue::Bool(snapshot.running),
        });
        records.push(Observation::integer(
            "controller.hci_version",
            snapshot.hci_version as u64,
        ));
        records.push(Observation::integer(
            "controller.max_slots",
            snapshot.max_slots as u64,
        ));
        records.push(Observation::boolean(
            "controller.legacy_handoff",
            snapshot.legacy_handoff,
        ));
        records.push(Observation::integer(
            "root_ports",
            snapshot.ports.len() as u64,
        ));
        records.push(Observation::integer(
            "devices",
            snapshot.devices.len() as u64,
        ));
        for (index, device) in snapshot.devices.iter().enumerate() {
            records.push(Observation::owned_text(
                if index == 0 {
                    "device.canonical"
                } else {
                    "device.canonical.more"
                },
                device.canonical_path.clone(),
            ));
        }
        self.last = Some(snapshot);
        records
    }

    fn state(&self, _: &[Observation]) -> State {
        let Some(snapshot) = self.current() else {
            return State::new("unobserved");
        };
        if snapshot.hci_version == 0 || snapshot.max_slots == 0 {
            return State::failed("invalid_capability");
        }
        if !snapshot.running {
            return State::new("controller_stopped");
        }
        let connected = snapshot.ports.iter().filter(|port| port.connected).count();
        if connected == 0 {
            return State::converged("idle");
        }
        let ports_ready = snapshot
            .ports
            .iter()
            .filter(|port| port.connected)
            .all(|port| port.enabled);
        let devices_ready = snapshot
            .devices
            .iter()
            .filter(|device| {
                snapshot
                    .ports
                    .iter()
                    .any(|port| port.index == device.root_port && port.connected)
            })
            .all(|device| device.address != 0 && device.endpoint_count > 0);
        if ports_ready && devices_ready {
            State::converged("enumerated")
        } else {
            State::new(if ports_ready {
                "enumeration_pending"
            } else {
                "port_training"
            })
        }
    }

    fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
        let Some(snapshot) = self.current() else {
            return alloc::vec![ConstraintResult::new(
                "observation",
                ConstraintStatus::Unknown,
                "no observation"
            )];
        };
        let mut result = Vec::new();
        result.push(ConstraintResult::new(
            "capability_valid",
            if snapshot.hci_version != 0 && snapshot.max_slots != 0 {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Violated
            },
            "HCI version and slot capacity",
        ));
        result.push(ConstraintResult::new(
            "controller_running",
            if snapshot.running {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Unsatisfied
            },
            "USBCMD/USBSTS running state",
        ));
        result.push(ConstraintResult::new(
            "legacy_handoff",
            if snapshot.legacy_handoff {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Unknown
            },
            "OS ownership observation",
        ));
        for port in snapshot.ports.iter().filter(|port| port.connected) {
            let status = if port.enabled {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Unsatisfied
            };
            result.push(ConstraintResult::new(
                "root_port_enabled",
                status,
                "PORTSC CCS/PED",
            ));
        }
        for device in &snapshot.devices {
            let root_valid = snapshot
                .ports
                .iter()
                .any(|port| port.index == device.root_port);
            result.push(ConstraintResult::new(
                "root_port_valid",
                if root_valid {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Violated
                },
                "device root port exists",
            ));
            result.push(ConstraintResult::new(
                "route_valid",
                if device.route_valid {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Violated
                },
                "hub ancestry and downstream port agree",
            ));
            result.push(ConstraintResult::new(
                "endpoint_configured",
                if device.endpoint_count > 0 {
                    ConstraintStatus::Satisfied
                } else if device.address == 0 {
                    ConstraintStatus::Unknown
                } else {
                    ConstraintStatus::Unsatisfied
                },
                "endpoint model is populated",
            ));
        }
        result
    }

    fn actions(
        &self,
        _: &[Observation],
        state: State,
        constraints: &[ConstraintResult],
    ) -> Vec<Action> {
        if constraints
            .iter()
            .any(|c| c.status == ConstraintStatus::Violated)
        {
            return Vec::new();
        }
        if state.stage == "controller_stopped" {
            return alloc::vec![Action::new(ActionKind::WarmReset, 4)];
        }
        if state.stage == "idle"
            || state.stage == "port_training"
            || state.stage == "enumeration_pending"
        {
            return alloc::vec![
                Action::new(ActionKind::Poll, 1),
                Action::new(ActionKind::Retry, 2)
            ];
        }
        Vec::new()
    }

    fn execute(&mut self, action: Action) -> ActionOutcome {
        match action.kind {
            ActionKind::Poll | ActionKind::Retry => {
                self.controller.poll_ports();
                ActionOutcome::Applied
            }
            ActionKind::WarmReset => match self
                .controller
                .reset()
                .and_then(|_| self.controller.start())
            {
                Ok(()) => ActionOutcome::Applied,
                Err(_) => ActionOutcome::Failed,
            },
            _ => ActionOutcome::NoProgress,
        }
    }
}
