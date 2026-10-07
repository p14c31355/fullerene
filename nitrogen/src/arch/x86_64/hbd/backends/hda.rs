//! HDA HBD observation adapter.
//!
//! HDA initialisation owns the controller and DMA resources, so HBD only
//! observes the controller's atomic readiness state here.  It deliberately
//! does not issue additional stream MMIO reads from the shell path: an HDA
//! controller can be absent or disconnected, and a diagnostic read must not
//! turn `hbd status` into another unbounded hardware wait.

use crate::hbd::action::{Action, ActionOutcome};
use crate::hbd::constraint::{ConstraintResult, ConstraintStatus};
use crate::hbd::observation::Observation;
use crate::hbd::solver::{SolveResult, SolverBackend, SolverBudget, solve};
use crate::hbd::state::State;
use crate::hda::HdaController;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdaObservation {
    pub controller_present: bool,
    pub controller_ready: bool,
}

pub fn observe(controller: &HdaController) -> HdaObservation {
    HdaObservation {
        controller_present: true,
        controller_ready: controller.is_ready(),
    }
}

pub struct HdaBackend<'a> {
    controller: &'a HdaController,
    last: Option<HdaObservation>,
}

impl<'a> HdaBackend<'a> {
    pub fn new(controller: &'a HdaController) -> Self {
        Self {
            controller,
            last: None,
        }
    }

    pub fn solve(controller: &'a HdaController, budget: SolverBudget) -> SolveResult {
        solve(&mut Self::new(controller), budget)
    }
}

impl SolverBackend for HdaBackend<'_> {
    fn name(&self) -> &'static str {
        "hda"
    }

    fn observe(&mut self) -> Vec<Observation> {
        let snapshot = observe(self.controller);
        self.last = Some(snapshot);
        alloc::vec![
            Observation::boolean("controller.present", snapshot.controller_present),
            Observation::boolean("controller.ready", snapshot.controller_ready),
        ]
    }

    fn state(&self, _: &[Observation]) -> State {
        match self.last {
            Some(snapshot) if snapshot.controller_ready => State::converged("ready"),
            Some(_) => State::new("initialization_pending"),
            None => State::new("unobserved"),
        }
    }

    fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
        let Some(snapshot) = self.last else {
            return alloc::vec![ConstraintResult::new(
                "observation",
                ConstraintStatus::Unknown,
                "no HDA observation"
            )];
        };
        alloc::vec![
            ConstraintResult::new(
                "controller_present",
                if snapshot.controller_present {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Violated
                },
                "HDA controller is registered",
            ),
            ConstraintResult::new(
                "controller_ready",
                if snapshot.controller_ready {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Unsatisfied
                },
                "HDA codec route and DMA stream are initialized",
            ),
        ]
    }

    fn actions(&self, _: &[Observation], _: State, _: &[ConstraintResult]) -> Vec<Action> {
        // Lazy HDA init requires kernel-owned DMA regions and is intentionally
        // not performed from a shell diagnostic.  The next audio request or
        // scheduler-owned audio path will perform it safely.
        Vec::new()
    }

    fn execute(&mut self, _: Action) -> ActionOutcome {
        ActionOutcome::NoProgress
    }
}
