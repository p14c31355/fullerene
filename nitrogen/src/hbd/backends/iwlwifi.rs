//! iwlwifi HBD observation adapter.

use crate::hbd::action::{Action, ActionKind, ActionOutcome};
use crate::hbd::constraint::{ConstraintResult, ConstraintStatus};
use crate::hbd::observation::Observation;
use crate::hbd::solver::{SolveResult, SolverBackend, SolverBudget, solve};
use crate::hbd::state::State;
use crate::iwlwifi::types::WifiInitPhase;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IwlwifiObservation {
    pub init_phase: WifiInitPhase,
    pub device_discovered: Option<bool>,
    pub firmware_ready: Option<bool>,
    pub link_status: Option<&'static str>,
}

pub fn observe() -> IwlwifiObservation {
    let phase = crate::iwlwifi::wifi_init_phase();
    let manager = crate::iwlwifi::wifi_state_snapshot();
    IwlwifiObservation {
        init_phase: phase,
        // `device_available=false` during early initialization means that
        // discovery has not completed yet; it is only a contradiction after
        // the lifecycle reaches its terminal Done phase.
        device_discovered: (phase == WifiInitPhase::Done)
            .then(|| manager.as_ref().map(|m| m.device_available))
            .flatten(),
        firmware_ready: Some(phase == WifiInitPhase::Done),
        link_status: manager.as_ref().map(|m| match m.status {
            bonder::wifi::WifiStatus::Disconnected => "disconnected",
            bonder::wifi::WifiStatus::Scanning => "scanning",
            bonder::wifi::WifiStatus::Authenticating => "authenticating",
            bonder::wifi::WifiStatus::Associating => "associating",
            bonder::wifi::WifiStatus::Handshake => "handshake",
            bonder::wifi::WifiStatus::Connected => "connected",
            bonder::wifi::WifiStatus::Error => "error",
        }),
    }
}

pub struct IwlwifiBackend {
    last: Option<IwlwifiObservation>,
}

impl IwlwifiBackend {
    pub const fn new() -> Self {
        Self { last: None }
    }
    pub fn solve(budget: SolverBudget) -> SolveResult {
        solve(&mut Self::new(), budget)
    }
}

impl Default for IwlwifiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SolverBackend for IwlwifiBackend {
    fn name(&self) -> &'static str {
        "iwlwifi"
    }
    fn observe(&mut self) -> Vec<Observation> {
        let snapshot = observe();
        let mut result = Vec::new();
        result.push(Observation::text("init.phase", snapshot.init_phase.name()));
        result.push(Observation::integer(
            "init.phase_id",
            snapshot.init_phase as u64,
        ));
        if let Some(value) = snapshot.device_discovered {
            result.push(Observation::boolean("device.discovered", value));
        }
        if let Some(value) = snapshot.firmware_ready {
            result.push(Observation::boolean("firmware.ready", value));
        }
        if let Some(value) = snapshot.link_status {
            result.push(Observation::text("link.state", value));
        }
        self.last = Some(snapshot);
        result
    }
    fn state(&self, _: &[Observation]) -> State {
        let Some(snapshot) = self.last else {
            return State::new("unobserved");
        };
        match snapshot.init_phase {
            WifiInitPhase::Failed => State::failed("failed"),
            WifiInitPhase::Done if snapshot.device_discovered != Some(false) => {
                State::converged("firmware_ready")
            }
            WifiInitPhase::Idle => State::new("discovered_pending"),
            _ => State::new(snapshot.init_phase.name()),
        }
    }
    fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
        let Some(snapshot) = self.last else {
            return alloc::vec![ConstraintResult::new(
                "observation",
                ConstraintStatus::Unknown,
                "no observation"
            )];
        };
        alloc::vec![
            ConstraintResult::new(
                "init_phase_known",
                if snapshot.init_phase == WifiInitPhase::Failed {
                    ConstraintStatus::Violated
                } else {
                    ConstraintStatus::Satisfied
                },
                "incremental phase is explicit"
            ),
            ConstraintResult::new(
                "device_discovered",
                if snapshot.init_phase != WifiInitPhase::Done {
                    ConstraintStatus::Unknown
                } else {
                    match snapshot.device_discovered {
                        Some(true) => ConstraintStatus::Satisfied,
                        Some(false) => ConstraintStatus::Violated,
                        None => ConstraintStatus::Unknown,
                    }
                },
                "PCI/device discovery"
            ),
            ConstraintResult::new(
                "firmware_ready",
                match snapshot.firmware_ready {
                    Some(true) => ConstraintStatus::Satisfied,
                    Some(false) => ConstraintStatus::Unsatisfied,
                    None => ConstraintStatus::Unknown,
                },
                "firmware lifecycle"
            ),
        ]
    }
    fn actions(
        &self,
        _: &[Observation],
        _: State,
        constraints: &[ConstraintResult],
    ) -> Vec<Action> {
        if constraints
            .iter()
            .any(|c| c.status == ConstraintStatus::Violated && c.name == "device_discovered")
        {
            return Vec::new();
        }
        if self
            .last
            .is_some_and(|snapshot| snapshot.init_phase == WifiInitPhase::Failed)
        {
            return alloc::vec![Action::new(ActionKind::Retry, 2)];
        }
        alloc::vec![
            Action::new(ActionKind::Backend("init_step"), 1),
            Action::new(ActionKind::Retry, 2)
        ]
    }
    fn execute(&mut self, action: Action) -> ActionOutcome {
        match action.kind {
            ActionKind::Backend("init_step") => {
                crate::iwlwifi::try_init_wifi_device_step();
                ActionOutcome::Applied
            }
            ActionKind::Retry => {
                if crate::iwlwifi::retry_wifi_initialization() {
                    ActionOutcome::Applied
                } else {
                    ActionOutcome::Failed
                }
            }
            _ => ActionOutcome::NoProgress,
        }
    }
}
