//! System initializer for managing component initialization

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use crate::{SystemResult, traits::Initializable};
use petroleum::common::logging::SystemError;

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
        // Build component info and dependency graph
        let mut component_names = Vec::new();
        let mut dependency_graph: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (i, component) in self.components.iter().enumerate() {
            let name = component.name();
            component_names.push((name, i, component.priority()));

            let deps: Vec<String> = component
                .dependencies()
                .iter()
                .map(|s| String::from(*s))
                .collect();
            dependency_graph.insert(String::from(name), deps);
        }

        // Perform topological sort
        let mut visited = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut order = Vec::new();

        fn visit(
            component_name: &str,
            dependency_graph: &BTreeMap<String, Vec<String>>,
            visited: &mut BTreeSet<String>,
            visiting: &mut BTreeSet<String>,
            order: &mut Vec<String>,
        ) -> SystemResult<()> {
            if visiting.contains(component_name) {
                return Err(SystemError::InvalidArgument); // Circular dependency
            }
            if visited.contains(component_name) {
                return Ok(());
            }

            visiting.insert(String::from(component_name));

            // Visit all dependencies first
            if let Some(deps) = dependency_graph.get(component_name) {
                for dep in deps {
                    visit(dep, dependency_graph, visited, visiting, order)?;
                }
            }

            visiting.remove(component_name);
            visited.insert(String::from(component_name));
            order.push(String::from(component_name));
            Ok(())
        }

        // Visit all components
        for (name, _, _) in &component_names {
            visit(
                name,
                &dependency_graph,
                &mut visited,
                &mut visiting,
                &mut order,
            )?;
        }

        // Build final initialization order
        let mut init_order = Vec::new();
        let name_to_component: BTreeMap<_, _> = component_names
            .into_iter()
            .map(|(name, idx, priority)| (String::from(name), (idx, priority)))
            .collect();

        for component_name in order.into_iter().rev() {
            // Reverse to get dependencies first
            if let Some((idx, _)) = name_to_component.get(&component_name) {
                init_order.push(*idx);
            }
        }

        // Sort by priority within dependency order
        init_order.sort_by_key(|&idx| {
            let component = &self.components[idx];
            component.priority()
        });

        // Initialize components in the final order
        for idx in init_order.into_iter().rev() {
            // Highest priority first
            let component = &mut self.components[idx];
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
