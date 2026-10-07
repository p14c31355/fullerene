//! Constraint results.  Unknown is intentionally distinct from violation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintStatus {
    Satisfied,
    Unsatisfied,
    Unknown,
    Violated,
}

impl ConstraintStatus {
    pub const fn is_converged(self) -> bool {
        matches!(self, Self::Satisfied)
    }
    pub const fn is_impossible(self) -> bool {
        matches!(self, Self::Violated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintResult {
    pub name: &'static str,
    pub status: ConstraintStatus,
    pub detail: &'static str,
}

impl ConstraintResult {
    pub const fn new(name: &'static str, status: ConstraintStatus, detail: &'static str) -> Self {
        Self {
            name,
            status,
            detail,
        }
    }
}
