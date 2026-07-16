//! Kernel-provided callbacks and cross-boundary data transfer types.

use alloc::string::String;
use alloc::vec::Vec;
pub type WallClockCallback = fn() -> Option<(u16, u8, u8, u8, u8, u8)>;
pub type VfsReadDirCallback = fn(&str) -> Result<Vec<VfsEntry>, genome::FsError>;
pub type VfsReadCallback = fn(&str) -> Result<Vec<u8>, genome::FsError>;
pub type VfsWriteCallback = fn(&str, &[u8]) -> Result<(), genome::FsError>;
pub type VfsTransferCallback = fn(&str, &str, bool) -> Result<(), genome::FsError>;
pub type VfsRemoveCallback = fn(&str, bool) -> Result<(), genome::FsError>;
pub type MountedDriveListCallback = fn() -> Vec<(String, String)>;

/// Kernel services installed into the Solvent orchestration layer.
pub struct SolventCallbacks {
    pub shell_cmd: Option<fn(&str) -> String>,
    pub launch_shell: Option<fn()>,
    pub heap_extend: Option<fn(usize) -> Result<(), ()>>,
    pub wall_clock: Option<WallClockCallback>,
    pub vfs_readdir: Option<VfsReadDirCallback>,
    pub vfs_read: Option<VfsReadCallback>,
    pub vfs_write: Option<VfsWriteCallback>,
    pub vfs_copy: Option<VfsTransferCallback>,
    pub vfs_move: Option<VfsTransferCallback>,
    pub vfs_remove: Option<VfsRemoveCallback>,
    pub process_list: Option<fn() -> Vec<ProcessEntry>>,
    pub device_list: Option<fn() -> Vec<DeviceEntry>>,
    pub mounted_drive_list: Option<MountedDriveListCallback>,
    pub usb_poll: Option<fn() -> bool>,
    pub settings_save: Option<fn()>,
}

impl SolventCallbacks {
    pub const fn none() -> Self {
        Self {
            shell_cmd: None,
            launch_shell: None,
            heap_extend: None,
            wall_clock: None,
            vfs_readdir: None,
            vfs_read: None,
            vfs_write: None,
            vfs_copy: None,
            vfs_move: None,
            vfs_remove: None,
            process_list: None,
            device_list: None,
            mounted_drive_list: None,
            usb_poll: None,
            settings_save: None,
        }
    }

    pub fn install(self) {
        crate::RUNTIME_CONTEXT.install_callbacks(self);
    }
}

pub fn exec_shell_command(input: &str) -> String {
    let callbacks = crate::RUNTIME_CONTEXT.callbacks();
    if let Some(shell_cmd) = callbacks.shell_cmd {
        drop(callbacks);
        shell_cmd(input)
    } else {
        String::from("(no shell)\n")
    }
}

pub fn launch_shell() {
    let callbacks = crate::RUNTIME_CONTEXT.callbacks();
    if let Some(launch_shell) = callbacks.launch_shell {
        drop(callbacks);
        launch_shell();
    }
}

pub fn get_mounted_drives() -> Vec<(String, String)> {
    let list_drives = crate::RUNTIME_CONTEXT.callbacks().mounted_drive_list;
    list_drives
        .map(|list_drives| list_drives())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct VfsEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub pid: u64,
    pub name: String,
    pub state: ProcessStateKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStateKind {
    Ready,
    Running,
    Blocked,
    Terminated,
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub dev_type: String,
    pub enabled: bool,
}
