//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use crate::traits::Initializable;
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::{self, Vec},
};
use petroleum::{InitSequence, common::logging::SystemError, init_log, write_serial_bytes};
use spin::Once;

macro_rules! init_step {
    ($name:expr, $closure:expr) => {
        (
            $name,
            Box::new($closure) as Box<dyn Fn() -> Result<(), &'static str>>,
        )
    };
}

#[cfg(target_os = "uefi")]
pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    init_log!("Initializing common components");

    let steps = [
        init_step!("VGA", move || {
            crate::vga::init_vga(physical_memory_offset);
            Ok(())
        }),
        init_step!("APIC", || {
            interrupts::init_apic();
            Ok(())
        }),
        init_step!("process", || {
            crate::process::init();
            Ok(())
        }),
        init_step!("syscall", || {
            crate::syscall::init();
            Ok(())
        }),
        init_step!("fs", || {
            crate::fs::init();
            Ok(())
        }),
        init_step!("loader", || {
            crate::loader::init();
            Ok(())
        }),
    ];
    InitSequence::new(&steps).run();

    init_log!("About to create test process");
    let test_pid = crate::process::create_process(
        "test_process",
        x86_64::VirtAddr::new(crate::process::test_process_main as usize as u64),
    );
    init_log!("Test process created: {}", test_pid);
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common(physical_memory_offset: x86_64::VirtAddr) {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
        [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        use petroleum::page_table::ALLOCATOR;
        ALLOCATOR
            .lock()
            .init(heap_start_ptr, crate::heap::HEAP_SIZE);
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga(physical_memory_offset);
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
    pub fn initialize_system(&mut self) -> petroleum::common::logging::SystemResult<()> {
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
        ) -> petroleum::common::logging::SystemResult<()> {
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

static SYSTEM_INITIALIZER: Once<spin::Mutex<SystemInitializer>> = Once::new();

// Register a component globally
pub fn register_system_component(component: Box<dyn Initializable + Send>) {
    SYSTEM_INITIALIZER
        .call_once(|| spin::Mutex::new(SystemInitializer::new()))
        .lock()
        .register_component(component);
}

// Initialize the entire system
pub fn initialize_system() -> petroleum::common::logging::SystemResult<()> {
    SYSTEM_INITIALIZER
        .call_once(|| spin::Mutex::new(SystemInitializer::new()))
        .lock()
        .initialize_system()
}
