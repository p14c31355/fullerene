//! Haber–Bosch daemon (HBD) — bounded hardware-state convergence.
//!
//! HBD is deliberately a small policy layer.  Drivers own hardware
//! mechanisms; a backend exposes observations and safe actions, while this
//! module provides the deterministic, budgeted convergence loop.

pub mod action;
pub mod backends;
pub mod constraint;
pub mod event;
pub mod observation;
pub mod policy;
pub mod report;
pub mod solver;
pub mod state;

pub use action::{Action, ActionKind, ActionOutcome, Transition};
pub use constraint::{ConstraintResult, ConstraintStatus};
pub use observation::{Observation, ObservationValue};
pub use report::{ConvergenceReport, ReportResult};
pub use solver::{SolveResult, SolverBackend, SolverBudget, solve};
pub use state::State;
