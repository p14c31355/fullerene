//! Static Fullerene application registration.

use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_TASKS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationState {
    Ready,
    Running,
    Finished,
}

#[derive(Clone, Debug)]
pub struct Application {
    pub name: String,
    pub state: ApplicationState,
    pub stack_size: usize,
}

pub fn register_applications() -> Vec<Application> {
    [
        ("system-info", 3 * 1024),
        ("files", 3 * 1024),
        ("settings", 3 * 1024),
    ]
    .into_iter()
    .map(|(name, stack_size)| Application {
        name: String::from(name),
        state: ApplicationState::Ready,
        stack_size,
    })
    .collect()
}

pub fn application_names() -> alloc::vec::Vec<String> {
    register_applications()
        .into_iter()
        .map(|application| application.name)
        .collect()
}

/// Fixed entry points are not yet assigned; zero marks an unlaunched app.
pub fn application_tasks() -> Vec<(&'static str, usize)> {
    [("system-info", 0), ("files", 0), ("settings", 0)]
        .into_iter()
        .collect()
}
