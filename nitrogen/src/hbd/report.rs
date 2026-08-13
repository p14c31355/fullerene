//! Human-readable and compact machine-readable convergence reports.

use super::action::{ActionKind, Transition};
use super::constraint::{ConstraintResult, ConstraintStatus};
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportResult {
    Converged,
    BudgetExhausted,
    Contradiction,
    NoAction,
    BackendFailure,
}

impl ReportResult {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Converged => "converged",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Contradiction => "contradiction",
            Self::NoAction => "no_action",
            Self::BackendFailure => "backend_failure",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvergenceReport {
    pub backend: &'static str,
    pub stage: &'static str,
    pub result: ReportResult,
    pub actions: usize,
    pub retries: usize,
    pub resets: usize,
    pub constraints: Vec<ConstraintResult>,
    pub transitions: Vec<Transition>,
    pub observations: Vec<super::Observation>,
}

impl ConvergenceReport {
    pub fn compact(&self) -> String {
        use core::fmt::Write;
        let mut out = String::new();
        let _ = write!(
            out,
            "backend={} stage={} result={} actions={} retries={} resets={}",
            self.backend,
            self.stage,
            self.result.name(),
            self.actions,
            self.retries,
            self.resets
        );
        for constraint in &self.constraints {
            let status = match constraint.status {
                ConstraintStatus::Satisfied => "satisfied",
                ConstraintStatus::Unsatisfied => "unsatisfied",
                ConstraintStatus::Unknown => "unknown",
                ConstraintStatus::Violated => "violated",
            };
            let _ = write!(out, " {}={}", constraint.name, status);
        }
        for observation in &self.observations {
            let _ = write!(out, " {}={}", observation.key, observation.value);
        }
        out
    }

    pub fn human(&self) -> String {
        use core::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "[hbd:{}]", self.backend);
        let _ = writeln!(out, "state: {}", self.stage);
        let _ = writeln!(out, "result: {}", self.result.name());
        let _ = writeln!(
            out,
            "attempts: actions={} retries={} resets={}",
            self.actions, self.retries, self.resets
        );
        if !self.observations.is_empty() {
            let _ = writeln!(out, "observations:");
            for observation in &self.observations {
                let _ = writeln!(out, "  {}={}", observation.key, observation.value);
            }
        }
        writeln!(out, "constraints:").ok();
        for constraint in &self.constraints {
            let status = match constraint.status {
                ConstraintStatus::Satisfied => "satisfied",
                ConstraintStatus::Unsatisfied => "unsatisfied",
                ConstraintStatus::Unknown => "unknown",
                ConstraintStatus::Violated => "violated",
            };
            let _ = writeln!(
                out,
                "  {}: {} ({})",
                constraint.name, status, constraint.detail
            );
        }
        if !self.transitions.is_empty() {
            let _ = writeln!(out, "transitions:");
            for transition in &self.transitions {
                let _ = writeln!(
                    out,
                    "  {} -> {:?} -> {}",
                    transition.from, transition.action, transition.to
                );
            }
        }
        out
    }
}

#[allow(dead_code)]
fn _action_name(action: ActionKind) -> &'static str {
    match action {
        ActionKind::Observe => "observe",
        ActionKind::Retry => "retry",
        ActionKind::WarmReset => "warm_reset",
        ActionKind::ColdReset => "cold_reset",
        ActionKind::Poll => "poll",
        ActionKind::Backend(name) => name,
    }
}
