//! System initializer for managing component initialization

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use crate::common::logging::{SystemError, SystemResult};

/// Initializable trait for components
pub trait Initializable {
    fn name(&self) -> &'static str;
    fn init(&mut self) -> SystemResult<()>;
    fn priority(&self) -> i32 {
        0 // Default priority
    }

    fn dependencies(&self) -> &[&str] {
        &[] // No dependencies by default
    }
}

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
                return Err(SystemError::InternalError); // Circular dependency
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
// Core system traits
pub trait HardwareDevice: Initializable + ErrorLogging {
    fn device_name(&self) -> &'static str;
    fn device_type(&self) -> &'static str;
    fn enable(&mut self) -> SystemResult<()>;
    fn disable(&mut self) -> SystemResult<()>;
    fn reset(&mut self) -> SystemResult<()>;
    fn is_enabled(&self) -> bool;
    fn read(&mut self, _address: usize, _buffer: &mut [u8]) -> SystemResult<usize> {
        // Default implementation returns error - hardware-specific devices should override
        Err(SystemError::NotSupported)
    }
    fn write(&mut self, _address: usize, _buffer: &[u8]) -> SystemResult<usize> {
        // Default implementation returns error - hardware-specific devices should override
        Err(SystemError::NotSupported)
    }
}

// Placeholder for syscall-related traits that may need to be defined
pub trait SyscallHandler {
    fn handle_syscall(&mut self, syscall_number: usize, args: &[usize]) -> SystemResult<usize>;
}

// Placeholder trait for page table management that may be needed
pub trait PageTableHelper {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: i32,
        frame_allocator: &mut dyn FrameAllocator,
    ) -> SystemResult<()>;
    fn unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()>;
    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize>;
    fn set_page_flags(&mut self, virtual_addr: usize, flags: i32) -> SystemResult<()>;
    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<i32>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()>;
    fn flush_tlb_all(&mut self) -> SystemResult<()>;
    fn create_page_table(&mut self) -> SystemResult<usize>;
    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()>;
    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

// Placeholder for logging-related traits that may need to be defined
pub trait Logger {
    fn log(&self, level: crate::common::logging::LogLevel, message: &str);
}

pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
    fn log_debug(&self, message: &'static str);
    fn log_trace(&self, message: &'static str);
}

pub trait MemoryManager {
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize>;
    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()>;
    fn total_memory(&self) -> usize;
    fn available_memory(&self) -> usize;
    fn map_address(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        count: usize,
    ) -> SystemResult<()>;
    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()>;
    fn virtual_to_physical(&self, virtual_addr: usize) -> SystemResult<usize>;
    fn init_paging(&mut self) -> SystemResult<()>;
    fn page_size(&self) -> usize;
}

pub trait ProcessMemoryManager {
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize>;
    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()>;
    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize>;
    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()>;
    fn copy_memory_between_processes(
        &mut self,
        from_process: usize,
        to_process: usize,
        from_addr: usize,
        to_addr: usize,
        size: usize,
    ) -> SystemResult<()>;
    fn current_process_id(&self) -> usize;
}

pub trait FrameAllocator {
    fn allocate_frame(&mut self) -> SystemResult<usize>;
    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()>;
    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize>;
    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn total_frames(&self) -> usize;
    fn available_frames(&self) -> usize;
    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn is_frame_available(&self, frame_addr: usize) -> bool;
    fn frame_size(&self) -> usize;
}

pub fn initialize_system() -> SystemResult<()> {
    SYSTEM_INITIALIZER
        .call_once(|| spin::Mutex::new(SystemInitializer::new()))
        .lock()
        .initialize_system()
}
