use x86_64::VirtAddr;

/// RAII guard for temporary mappings
pub struct TemporaryMapping {
    pub va: VirtAddr,
}

impl TemporaryMapping {
    pub fn new(va: VirtAddr) -> Self {
        Self { va }
    }
}