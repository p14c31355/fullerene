use alloc::vec::Vec;
use spin::Mutex;

#[derive(Clone)]
pub struct PluginEntry {
    pub name: &'static str,
}

pub struct PluginRegistry {
    entries: Vec<PluginEntry>,
}

impl PluginRegistry {
    pub fn register(name: &'static str) {
        REGISTRY.lock().entries.push(PluginEntry { name });
    }

    pub fn iter() -> alloc::vec::IntoIter<PluginEntry> {
        REGISTRY.lock().entries.clone().into_iter()
    }

    pub fn count() -> usize {
        REGISTRY.lock().entries.len()
    }
}

static REGISTRY: Mutex<PluginRegistry> = Mutex::new(PluginRegistry {
    entries: Vec::new(),
});
