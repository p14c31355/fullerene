//! Deterministic finite-budget convergence loop.

use super::action::{Action, ActionKind, ActionOutcome, Transition};
use super::constraint::{ConstraintResult, ConstraintStatus};
use super::observation::Observation;
use super::report::{ConvergenceReport, ReportResult};
use super::state::State;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolverBudget {
    pub max_actions: usize,
    pub max_resets: usize,
    pub max_retries: usize,
}

impl SolverBudget {
    pub const fn conservative() -> Self {
        Self {
            max_actions: 16,
            max_resets: 1,
            max_retries: 3,
        }
    }
}

pub trait SolverBackend {
    fn name(&self) -> &'static str;
    fn observe(&mut self) -> Vec<Observation>;
    fn state(&self, observations: &[Observation]) -> State;
    fn constraints(&self, observations: &[Observation], state: State) -> Vec<ConstraintResult>;
    /// Actions must be returned in stable preference order and must be safe
    /// for the current observation.  The solver adds budget enforcement.
    fn actions(
        &self,
        observations: &[Observation],
        state: State,
        constraints: &[ConstraintResult],
    ) -> Vec<Action>;
    fn execute(&mut self, action: Action) -> ActionOutcome;
}

#[derive(Debug, Clone)]
pub struct SolveResult {
    pub report: ConvergenceReport,
}

pub fn solve<B: SolverBackend>(backend: &mut B, budget: SolverBudget) -> SolveResult {
    let mut actions_taken = 0;
    let mut retries = 0;
    let mut resets = 0;
    let mut transitions = Vec::new();
    let mut observations = backend.observe();
    let mut state = backend.state(&observations);
    let mut constraints = backend.constraints(&observations, state);
    let result = loop {
        if state.converged && constraints.iter().all(|c| c.status.is_converged()) {
            break ReportResult::Converged;
        }
        if state.terminal_failure
            || constraints
                .iter()
                .any(|c| c.status == ConstraintStatus::Violated)
        {
            break ReportResult::Contradiction;
        }
        if actions_taken >= budget.max_actions {
            break ReportResult::BudgetExhausted;
        }
        let candidates = backend.actions(&observations, state, &constraints);
        let had_candidates = !candidates.is_empty();
        let Some(action) = candidates.into_iter().find(|action| match action.kind {
            ActionKind::WarmReset | ActionKind::ColdReset => resets < budget.max_resets,
            ActionKind::Retry => retries < budget.max_retries,
            _ => true,
        }) else {
            break if had_candidates {
                ReportResult::BudgetExhausted
            } else {
                ReportResult::NoAction
            };
        };
        if matches!(action.kind, ActionKind::WarmReset | ActionKind::ColdReset) {
            resets += 1;
        }
        if matches!(action.kind, ActionKind::Retry) {
            retries += 1;
        }
        actions_taken += 1;
        let from = state.stage;
        let outcome = backend.execute(action);
        if matches!(outcome, ActionOutcome::Failed) {
            if matches!(action.kind, ActionKind::Retry) && retries >= budget.max_retries {
                break ReportResult::BackendFailure;
            }
        }
        observations = backend.observe();
        state = backend.state(&observations);
        constraints = backend.constraints(&observations, state);
        transitions.push(Transition {
            from,
            action: action.kind,
            to: state.stage,
        });
        if matches!(outcome, ActionOutcome::Failed) && state.stage == from {
            if retries >= budget.max_retries {
                break ReportResult::BackendFailure;
            }
        }
    };
    let stage = state.stage;
    SolveResult {
        report: ConvergenceReport {
            backend: backend.name(),
            stage,
            result,
            actions: actions_taken,
            retries,
            resets,
            constraints,
            transitions,
            observations,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hbd::observation::ObservationValue;

    struct Fake {
        value: u8,
        fail: bool,
        fail_once: bool,
    }
    impl SolverBackend for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn observe(&mut self) -> Vec<Observation> {
            alloc::vec![Observation {
                key: "value",
                value: ObservationValue::Integer(self.value as u64)
            }]
        }
        fn state(&self, obs: &[Observation]) -> State {
            if obs[0].value == ObservationValue::Integer(2) {
                State::converged("ready")
            } else {
                State::new("pending")
            }
        }
        fn constraints(&self, obs: &[Observation], _: State) -> Vec<ConstraintResult> {
            let status = if obs[0].value == ObservationValue::Integer(2) {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Unsatisfied
            };
            alloc::vec![ConstraintResult::new("value", status, "value reaches two")]
        }
        fn actions(&self, _: &[Observation], _: State, _: &[ConstraintResult]) -> Vec<Action> {
            alloc::vec![Action::new(ActionKind::Retry, 1)]
        }
        fn execute(&mut self, _: Action) -> ActionOutcome {
            if self.fail {
                ActionOutcome::Failed
            } else if self.fail_once {
                self.fail_once = false;
                ActionOutcome::Failed
            } else {
                self.value += 1;
                ActionOutcome::Applied
            }
        }
    }

    #[test]
    fn normal_convergence_is_deterministic() {
        let mut fake = Fake {
            value: 0,
            fail: false,
            fail_once: false,
        };
        let result = solve(
            &mut fake,
            SolverBudget {
                max_actions: 4,
                max_resets: 0,
                max_retries: 4,
            },
        );
        assert_eq!(result.report.result, ReportResult::Converged);
        assert_eq!(result.report.actions, 2);
    }

    #[test]
    fn action_budget_exhaustion_is_reported() {
        let mut fake = Fake {
            value: 0,
            fail: false,
            fail_once: false,
        };
        let result = solve(
            &mut fake,
            SolverBudget {
                max_actions: 1,
                max_resets: 0,
                max_retries: 1,
            },
        );
        assert_eq!(result.report.result, ReportResult::BudgetExhausted);
    }

    #[test]
    fn failed_action_does_not_retry_forever() {
        let mut fake = Fake {
            value: 0,
            fail: true,
            fail_once: false,
        };
        let result = solve(
            &mut fake,
            SolverBudget {
                max_actions: 8,
                max_resets: 0,
                max_retries: 2,
            },
        );
        assert_eq!(result.report.result, ReportResult::BackendFailure);
        assert_eq!(result.report.actions, 2);
    }

    #[test]
    fn already_converged_state_performs_no_action() {
        let mut fake = Fake {
            value: 2,
            fail: false,
            fail_once: false,
        };
        let result = solve(&mut fake, SolverBudget::conservative());
        assert_eq!(result.report.result, ReportResult::Converged);
        assert_eq!(result.report.actions, 0);
    }

    struct Contradictory;
    impl SolverBackend for Contradictory {
        fn name(&self) -> &'static str {
            "contradictory"
        }
        fn observe(&mut self) -> Vec<Observation> {
            Vec::new()
        }
        fn state(&self, _: &[Observation]) -> State {
            State::new("inconsistent")
        }
        fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
            alloc::vec![ConstraintResult::new(
                "route",
                ConstraintStatus::Violated,
                "route cannot exist"
            )]
        }
        fn actions(&self, _: &[Observation], _: State, _: &[ConstraintResult]) -> Vec<Action> {
            alloc::vec![Action::new(ActionKind::Retry, 1)]
        }
        fn execute(&mut self, _: Action) -> ActionOutcome {
            ActionOutcome::Applied
        }
    }

    #[test]
    fn contradictory_observation_is_not_retried() {
        let mut backend = Contradictory;
        let result = solve(&mut backend, SolverBudget::conservative());
        assert_eq!(result.report.result, ReportResult::Contradiction);
        assert_eq!(result.report.actions, 0);
    }

    #[test]
    fn reset_budget_exhaustion_is_reported() {
        struct ResetOnly;
        impl SolverBackend for ResetOnly {
            fn name(&self) -> &'static str {
                "reset-only"
            }
            fn observe(&mut self) -> Vec<Observation> {
                Vec::new()
            }
            fn state(&self, _: &[Observation]) -> State {
                State::new("stalled")
            }
            fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
                Vec::new()
            }
            fn actions(&self, _: &[Observation], _: State, _: &[ConstraintResult]) -> Vec<Action> {
                alloc::vec![Action::new(ActionKind::ColdReset, 10)]
            }
            fn execute(&mut self, _: Action) -> ActionOutcome {
                ActionOutcome::Applied
            }
        }
        let mut backend = ResetOnly;
        let result = solve(
            &mut backend,
            SolverBudget {
                max_actions: 2,
                max_resets: 0,
                max_retries: 0,
            },
        );
        assert_eq!(result.report.result, ReportResult::BudgetExhausted);
    }

    #[test]
    fn recoverable_intermediate_failure_can_progress() {
        let mut fake = Fake {
            value: 0,
            fail: false,
            fail_once: true,
        };
        let result = solve(
            &mut fake,
            SolverBudget {
                max_actions: 4,
                max_resets: 0,
                max_retries: 4,
            },
        );
        assert_eq!(result.report.result, ReportResult::Converged);
        assert_eq!(result.report.actions, 3);
    }

    #[test]
    fn observation_shortage_without_safe_action_is_reported() {
        struct NoAction;
        impl SolverBackend for NoAction {
            fn name(&self) -> &'static str {
                "no-action"
            }
            fn observe(&mut self) -> Vec<Observation> {
                Vec::new()
            }
            fn state(&self, _: &[Observation]) -> State {
                State::new("unknown")
            }
            fn constraints(&self, _: &[Observation], _: State) -> Vec<ConstraintResult> {
                alloc::vec![ConstraintResult::new(
                    "device",
                    ConstraintStatus::Unknown,
                    "observation missing"
                )]
            }
            fn actions(&self, _: &[Observation], _: State, _: &[ConstraintResult]) -> Vec<Action> {
                Vec::new()
            }
            fn execute(&mut self, _: Action) -> ActionOutcome {
                ActionOutcome::NoProgress
            }
        }
        let mut backend = NoAction;
        let result = solve(&mut backend, SolverBudget::conservative());
        assert_eq!(result.report.result, ReportResult::NoAction);
    }

    #[test]
    fn one_backend_failure_does_not_change_an_independent_result() {
        let mut failed = Fake {
            value: 0,
            fail: true,
            fail_once: false,
        };
        let failed_result = solve(
            &mut failed,
            SolverBudget {
                max_actions: 2,
                max_resets: 0,
                max_retries: 2,
            },
        );
        let mut healthy = Fake {
            value: 0,
            fail: false,
            fail_once: false,
        };
        let healthy_result = solve(&mut healthy, SolverBudget::conservative());
        assert_eq!(failed_result.report.result, ReportResult::BackendFailure);
        assert_eq!(healthy_result.report.result, ReportResult::Converged);
    }
}
