//! Events fed back into a backend between observations.

use super::action::ActionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Observation,
    ActionCompleted(ActionKind),
    ActionFailed(ActionKind),
    Timeout,
}
