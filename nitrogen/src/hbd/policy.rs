//! Small policy helpers shared by backends.

use super::action::{Action, ActionKind};
use super::constraint::{ConstraintResult, ConstraintStatus};

/// Select the first action supplied by a backend in its deterministic order.
/// A policy never retries an explicit specification violation.
pub fn first_recoverable_action(
    constraints: &[ConstraintResult],
    actions: &[Action],
) -> Option<Action> {
    if constraints
        .iter()
        .any(|c| c.status == ConstraintStatus::Violated)
    {
        return None;
    }
    actions.iter().copied().find(|action| {
        !matches!(action.kind, ActionKind::Observe)
            || constraints
                .iter()
                .any(|c| c.status != ConstraintStatus::Satisfied)
    })
}
