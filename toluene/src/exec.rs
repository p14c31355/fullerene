#[derive(Debug)]
pub enum ExecError {
    NotFound,
    Unsupported,
}

pub fn spawn(_binary: &[u8], _args: &[&str]) -> Result<u64, ExecError> {
    Err(ExecError::Unsupported)
}

pub fn spawn_simple(_name: &str) -> Result<u64, ExecError> {
    Err(ExecError::Unsupported)
}

pub fn list_programs() -> alloc::vec::Vec<&'static str> {
    alloc::vec!["toluene", "shell"]
}
