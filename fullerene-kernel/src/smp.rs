//! Multiprocessor topology and application-processor bring-up state.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

static PROCESSORS: Mutex<Vec<nitrogen::acpi::madt::Processor>> = Mutex::new(Vec::new());
static ONLINE_APS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static ONLINE_PROCESSORS: AtomicUsize = AtomicUsize::new(1);

pub fn configure(topology: nitrogen::acpi::madt::MadtInfo) {
    let mut processors = topology.processors;
    processors.retain(|processor| processor.enabled || processor.online_capable);
    if processors.is_empty() {
        processors.push(nitrogen::acpi::madt::Processor {
            processor_uid: 0,
            apic_id: 0,
            enabled: true,
            online_capable: false,
        });
    }
    *PROCESSORS.lock() = processors;
    ONLINE_APS.lock().clear();
    ONLINE_PROCESSORS.store(1, Ordering::Release);
}

pub fn discovered_count() -> usize {
    PROCESSORS.lock().len().max(1)
}

pub fn online_count() -> usize {
    ONLINE_PROCESSORS.load(Ordering::Acquire)
}

pub fn topology() -> Vec<nitrogen::acpi::madt::Processor> {
    PROCESSORS.lock().clone()
}

/// Record an AP after its trampoline enters the 64-bit kernel.
pub fn mark_processor_online(apic_id: u32) -> bool {
    let known = PROCESSORS
        .lock()
        .iter()
        .any(|processor| processor.apic_id == apic_id);
    if !known {
        return false;
    }

    let mut online_aps = ONLINE_APS.lock();
    if !online_aps.contains(&apic_id) {
        online_aps.push(apic_id);
        ONLINE_PROCESSORS.store(1 + online_aps.len(), Ordering::Release);
    }
    true
}

pub fn format_topology() -> alloc::string::String {
    use core::fmt::Write;
    let mut output = alloc::string::String::from("APIC ID  UID       ENABLED  ONLINE-CAPABLE\n");
    output.push_str("-------  --------  -------  --------------\n");
    for processor in topology() {
        let _ = writeln!(
            output,
            "{:<7}  {:<8}  {:<7}  {}",
            processor.apic_id,
            processor.processor_uid,
            if processor.enabled { "yes" } else { "no" },
            if processor.online_capable {
                "yes"
            } else {
                "no"
            }
        );
    }
    let _ = writeln!(
        output,
        "Discovered: {}  Online: {}",
        discovered_count(),
        online_count()
    );
    output
}
