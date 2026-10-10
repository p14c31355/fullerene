//! State values shared by all convergence backends.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub stage: &'static str,
    pub converged: bool,
    pub terminal_failure: bool,
}

impl State {
    pub const fn new(stage: &'static str) -> Self {
        Self {
            stage,
            converged: false,
            terminal_failure: false,
        }
    }

    pub const fn converged(stage: &'static str) -> Self {
        Self {
            stage,
            converged: true,
            terminal_failure: false,
        }
    }

    pub const fn failed(stage: &'static str) -> Self {
        Self {
            stage,
            converged: false,
            terminal_failure: true,
        }
    }
}
