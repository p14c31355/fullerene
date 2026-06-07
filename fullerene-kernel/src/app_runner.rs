//! External Application Runner / Execution Model
//!
//! Provides a safe way to launch user-space applications (ELF binaries)
//! from the shell or from the GUI.  Implements the "external app
//! execution model" described in the TODO.
//!
//! # Architecture
//!
//! ```
//! Shell "run <app>" → app_runner::launch(name)
//!                     → loader::load_program(binary, name)
//!                     → process::create_process(...)
//!                     → scheduler picks up the new process
//! ```
//!
//! # Login / Authentication
//!
//! In the current single-user kernel model there is no login mechanism.
//! The kernel runs in ring 0 and the shell runs cooperatively inside it.
//! A future multi-user / multi-session environment would add:
//! - A login manager process (greeter)
//! - Per-user home directories in the VFS
//! - User/group IDs on Process structures
//! - Access control checks in syscall dispatch
//!
//! For now, the "login" is implicitly handled by the boot sequence —
//! the system boots directly into the shell.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use x86_64::VirtAddr;

/// Built-in application binary (embedded in kernel).
struct KnownApp {
    name: &'static str,
    binary: &'static [u8],
}

/// Registry of known external applications.
static APPS: [KnownApp; 2] = [
    KnownApp {
        name: "toluene",
        binary: &[],
    },
    KnownApp {
        name: "hello",
        binary: &[],
    },
];

/// Launch an external application by name.
///
/// Searches the built-in app registry first, then falls back to
/// loading from the VFS if a file with the given name exists.
pub fn launch(name: &str) -> Result<u64, AppError> {
    // Check built-in apps
    for app in APPS.iter() {
        if app.name == name {
            if app.binary.is_empty() {
                return Err(AppError::NotYetAvailable);
            }
            return launch_binary(app.name, app.binary);
        }
    }

    // Try loading from VFS
    let path = alloc::format!("/{}", name);
    if let Ok(data) = crate::fs::read_entire_file(&path) {
        if data.is_empty() {
            return Err(AppError::NotYetAvailable);
        }
        let pid = crate::loader::load_program(&data, "vfs-app")
            .map_err(|_| AppError::LoadFailed)?;
        log::info!("Launched VFS app '{}' as PID {}", name, pid.0);
        return Ok(pid.0);
    }
    Err(AppError::NotFound)
}

/// Launch a raw binary as a new process.
fn launch_binary(name: &'static str, data: &[u8]) -> Result<u64, AppError> {
    if data.is_empty() {
        return Err(AppError::NotYetAvailable);
    }

    let pid = crate::loader::load_program(data, name)
        .map_err(|_| AppError::LoadFailed)?;

    log::info!("Launched app '{}' as PID {}", name, pid.0);

    Ok(pid.0)
}

/// Application runner errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    NotFound,
    NotYetAvailable,
    LoadFailed,
    PermissionDenied,
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AppError::NotFound => write!(f, "app not found"),
            AppError::NotYetAvailable => write!(f, "app binary not yet available (compile-time)"),
            AppError::LoadFailed => write!(f, "failed to load app binary"),
            AppError::PermissionDenied => write!(f, "permission denied"),
        }
    }
}

/// Initialize the app runner.
pub fn init() {
    log::info!("App runner initialized ({} known apps)", APPS.len());
}

/// List available applications.
pub fn list_apps() -> String {
    let mut out = String::from("Available applications:\n");
    for app in APPS.iter() {
        let status = if app.binary.is_empty() {
            "(binary not embedded)"
        } else {
            "(ready)"
        };
        let line = format!("  {} {}\n", app.name, status);
        out.push_str(&line);
    }
    // Also list ELF files in VFS
    if let Ok(entries) = crate::vfs::readdir("/") {
        for ent in entries {
            if !ent.is_dir {
                let line = format!("  {} (in VFS, {} bytes)\n", ent.name, ent.size);
                out.push_str(&line);
            }
        }
    }
    out
}