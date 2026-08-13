//! Explicit actions and transitions used by the bounded solver.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Observe,
    Retry,
    WarmReset,
    ColdReset,
    Poll,
    Backend(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub cost: u16,
}

impl Action {
    pub const fn new(kind: ActionKind, cost: u16) -> Self {
        Self { kind, cost }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Applied,
    NoProgress,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: &'static str,
    pub action: ActionKind,
    pub to: &'static str,
}
