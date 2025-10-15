//! System initializer for managing component initialization

use alloc::{boxed::Box, vec::Vec};

use crate::{SystemResult, traits::Initializable};

// System initializer for managing component initialization
pub struct SystemInitializer {
    components: Vec<Box<dyn Initializable + Send>>,
}

impl SystemInitializer {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Register a component for initialization
    pub fn register_component(&mut self, component: Box<dyn Initializable + Send>) {
        self.components.push(component);
    }

    /// Initialize all registered components in dependency order
    pub fn initialize_system(&mut self) -> SystemResult<()> {
        // Sort components by priority (higher priority first)
        self.components.sort_by(
            |a: &Box<dyn Initializable + Send>, b: &Box<dyn Initializable + Send>| {
                b.priority().cmp(&a.priority())
            },
        );

        // TODO: Implement proper dependency resolution
        // For now, just initialize in priority order

        for component in &mut self.components {
            // Initialize component without format strings for now
            if let Err(e) = component.init() {
                return Err(e);
            }
        }

        Ok(())
    }
}

// Use spin::Once to ensure the initializer is only initialized once
use spin::Once;
static SYSTEM_INITIALIZER: Once<spin::Mutex<SystemInitializer>> = Once::new();

// Register a component globally
pub fn register_system_component(component: Box<dyn Initializable + Send>) {
    SYSTEM_INITIALIZER
        .call_once(|| spin::Mutex::new(SystemInitializer::new()))
        .lock()
        .register_component(component);
}

// Initialize the entire system
pub fn initialize_system() -> SystemResult<()> {
    SYSTEM_INITIALIZER
        .call_once(|| spin::Mutex::new(SystemInitializer::new()))
        .lock()
        .initialize_system()
}
