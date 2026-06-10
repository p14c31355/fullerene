//! Program Executor — ELF binary loader for Toluene SDK
//!
//! Provides high-level APIs for loading and executing ELF binaries
//! on Fullerene OS.  Designed to be used both by built-in applications
//! and as a public SDK for third-party Fullerene programs.
//!
//! # Architecture
//!
//! ```text
//! toluene::exec::spawn(binary, args)
//!   → syscall: exec (creates new process from ELF)
//!   → kernel loader parses ELF, maps segments, sets up process
//!   → scheduler picks up new process
//! ```
//!
//! # Example
//!
//! ```ignore
//! use toluene::exec;
//!
//! // Load and execute a binary from VFS
//! let pid = exec::spawn("/bin/toluene", &["--help"])?;
//!
//! // Wait for it to complete
//! exec::wait(pid);
//! ```

use alloc::string::String;
use alloc::vec::Vec;

/// Process identifier returned after spawning a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pid(pub u64);

/// Error type for program execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// Binary not found at the given path.
    NotFound,
    /// Binary exists but is not a valid ELF executable.
    InvalidFormat,
    /// Not enough memory to load the binary.
    OutOfMemory,
    /// The binary requested an unsupported feature or architecture.
    Unsupported,
    /// Permission denied (e.g. trying to execute non-executable file).
    PermissionDenied,
    /// Generic I/O or system error.
    IoError,
}

impl core::fmt::Display for ExecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ExecError::NotFound => write!(f, "program not found"),
            ExecError::InvalidFormat => write!(f, "invalid ELF format"),
            ExecError::OutOfMemory => write!(f, "out of memory"),
            ExecError::Unsupported => write!(f, "unsupported binary"),
            ExecError::PermissionDenied => write!(f, "permission denied"),
            ExecError::IoError => write!(f, "I/O error"),
        }
    }
}

/// Result type for exec operations.
pub type Result<T> = core::result::Result<T, ExecError>;

/// Spawn a new process from an ELF binary at `path`.
///
/// The binary is loaded from the VFS and executed in a new process.
/// Returns the process ID of the spawned process.
///
/// # Arguments
///
/// * `path` - Path to the ELF binary in the VFS.
/// * `args` - Command-line arguments passed to the new process.
pub fn spawn(path: &str, args: &[&str]) -> Result<Pid> {
    // Currently args are not supported by the exec ABI
    if !args.is_empty() {
        return Err(ExecError::Unsupported);
    }
    spawn_internal(path)
}

/// Spawn a new process with default (empty) arguments.
pub fn spawn_simple(path: &str) -> Result<Pid> {
    spawn_internal(path)
}

/// Internal spawn implementation using kernel syscalls.
fn spawn_internal(path: &str) -> Result<Pid> {
    // Read the binary from VFS
    let binary = read_file(path)?;
    if binary.is_empty() {
        return Err(ExecError::NotFound);
    }

    // Attempt to load and execute the ELF binary
    load_and_exec(path, &binary)
}

/// Read a file from the VFS into a buffer.
#[cfg(feature = "vfs")]
fn read_file(path: &str) -> Result<Vec<u8>> {
    // VFS support not yet implemented in petroleum
    let _ = path;
    Err(ExecError::Unsupported)
}

#[cfg(not(feature = "vfs"))]
fn read_file(path: &str) -> Result<Vec<u8>> {
    // Without VFS support, this is a stub.
    // Real implementation requires petroleum's vfs or raw syscalls.
    let _ = path;
    Err(ExecError::Unsupported)
}

/// Load an ELF binary from raw bytes and execute it as a new process.
fn load_and_exec(name: &str, data: &[u8]) -> Result<Pid> {
    // Validate ELF magic
    if data.len() < 4 || &data[0..4] != b"\x7FELF" {
        return Err(ExecError::InvalidFormat);
    }

    // Parse ELF header to verify it's executable
    let is_64bit = data.get(4) == Some(&2); // ELFCLASS64
    if !is_64bit {
        return Err(ExecError::Unsupported);
    }

    // Verify it's an executable (ET_EXEC)
    // e_type is at offset 16 (2 bytes, little-endian)
    let e_type = u16::from_le_bytes([
        *data.get(16).unwrap_or(&0),
        *data.get(17).unwrap_or(&0),
    ]);
    if e_type != 2 {
        // ET_EXEC = 2
        return Err(ExecError::InvalidFormat);
    }

    // Call kernel to create a process from this ELF binary.
    // This uses the kernel's loader infrastructure (fullerene-kernel/src/loader.rs).
    exec_syscall(name, data)
}

/// Syscall wrapper to ask the kernel to load and run an ELF binary.
///
/// This calls into the kernel's program loader which:
/// 1. Parses the ELF headers
/// 2. Maps LOAD segments into the new process's address space
/// 3. Creates a process with the entry point
/// 4. Returns the PID
#[cfg(feature = "kernel-syscall")]
fn exec_syscall(name: &str, data: &[u8]) -> Result<Pid> {
    // exec syscall not yet implemented in petroleum
    let _ = name;
    let _ = data;
    Err(ExecError::Unsupported)
}

#[cfg(not(feature = "kernel-syscall"))]
fn exec_syscall(name: &str, data: &[u8]) -> Result<Pid> {
    // Stub: without kernel syscall support, return Unsupported
    let _ = name;
    let _ = data;
    Err(ExecError::Unsupported)
}

/// Wait for a spawned process to terminate.
///
/// Blocks the current process until the child with `pid` exits.
pub fn wait(pid: Pid) -> Result<i32> {
    wait_internal(pid)
}

/// Internal wait implementation.
#[cfg(feature = "kernel-syscall")]
fn wait_internal(pid: Pid) -> Result<i32> {
    // waitpid syscall not yet implemented in petroleum
    let _ = pid;
    Err(ExecError::Unsupported)
}

#[cfg(not(feature = "kernel-syscall"))]
fn wait_internal(pid: Pid) -> Result<i32> {
    let _ = pid;
    Err(ExecError::Unsupported)
}

/// Check if a binary at the given path exists and is a valid executable.
pub fn is_executable(path: &str) -> bool {
    match read_file(path) {
        Ok(data) => {
            // Check ELF magic, ELFCLASS64, and e_type (ET_EXEC=2 or ET_DYN=3)
            if data.len() < 18 || &data[0..4] != b"\x7FELF" || data.get(4) != Some(&2) {
                return false;
            }
            // e_type at offset 16 (2 bytes, little-endian)
            let e_type = u16::from_le_bytes([
                *data.get(16).unwrap_or(&0),
                *data.get(17).unwrap_or(&0),
            ]);
            // ET_EXEC = 2, ET_DYN = 3 (position-independent executable)
            e_type == 2 || e_type == 3
        }
        Err(_) => false,
    }
}

/// List all executable programs available in the system.
pub fn list_programs() -> Vec<String> {
    // In a real implementation, this would scan known binary directories.
    // For now, return a stub list.
    let mut programs = Vec::new();
    programs.push(String::from("toluene"));
    programs.push(String::from("shell"));
    programs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_error_display() {
        assert_eq!(
            alloc::format!("{}", ExecError::NotFound),
            "program not found"
        );
        assert_eq!(
            alloc::format!("{}", ExecError::InvalidFormat),
            "invalid ELF format"
        );
    }

    #[test]
    fn test_is_executable_empty() {
        // Without VFS, is_executable always returns false
        assert!(!is_executable("/nonexistent"));
    }

    #[test]
    fn test_list_programs_non_empty() {
        let progs = list_programs();
        assert!(!progs.is_empty());
    }
}