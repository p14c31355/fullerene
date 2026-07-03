// Linux binary launcher
use crate::loader::LoadError;
use crate::process::ProcessId;

/// Launch the built-in test binary ("Hello from Linux!") to verify ABI.
pub fn launch_test_binary() -> Result<ProcessId, LoadError> {
    launch_linux_from_data(crate::linux::test_binary::HELLO_ELF, "hello-linux")
}

/// Launch a Linux ELF binary from the VFS at `path`.
pub fn launch_linux_binary(path: &str) -> Result<ProcessId, LoadError> {
    let data = match crate::fs::read_entire_file(path) {
        Ok(d) => d,
        Err(_) => return Err(LoadError::InvalidFormat),
    };
    launch_linux_from_data(&data, path)
}

/// Launch a Linux ELF binary from raw bytes.
pub fn launch_linux_from_slice(data: &[u8], name: &str) -> Result<ProcessId, LoadError> {
    launch_linux_from_data(data, name)
}

/// Launch a Linux ELF binary from raw bytes, extracting filename from path.
pub fn launch_linux_from_data(data: &[u8], name: &str) -> Result<ProcessId, LoadError> {
    // Use the existing ELF loader with is_linux=true
    let _ = name.rsplit('/').next().unwrap_or(name);
    // We need a static name. Use a simple approach.
    let static_name = "linux-app";
    crate::loader::load_program_with_runtime(data, static_name, true)
}

/// Launch BusyBox shell from embedded initramfs data.
pub fn launch_busybox() -> Result<ProcessId, &'static str> {
    // Look for busybox in standard locations
    let locations = [
        "/bin/busybox",
        "/sbin/busybox",
        "/usr/bin/busybox",
        "/usr/sbin/busybox",
        "/busybox",
        "/init",
    ];

    for path in &locations {
        if crate::contexts::vfs::exists(path) {
            log::info!("Found BusyBox at {}", path);
            return launch_linux_binary(path).map_err(|_| "Failed to load BusyBox");
        }
    }

    Err("BusyBox not found in filesystem")
}

/// Initialize the initramfs: creates basic Linux filesystem structure
/// and places built-in binaries if available.
pub fn init_initramfs() {
    log::info!("Initramfs: creating Linux filesystem structure");

    // Create standard Linux directories
    let dirs = [
        "/bin",
        "/sbin",
        "/usr",
        "/usr/bin",
        "/usr/sbin",
        "/etc",
        "/dev",
        "/proc",
        "/sys",
        "/tmp",
        "/var",
        "/var/log",
        "/root",
        "/home",
        "/lib",
        "/lib64",
    ];

    for dir in &dirs {
        let _ = crate::contexts::vfs::mkdir(dir);
    }

    // Create /dev/null
    let _ = crate::contexts::vfs::create("/dev/null");

    // Create a simple /etc/hostname
    let _ = crate::fs::write_entire_file("/etc/hostname", b"fullerene\n");

    log::info!("Initramfs: Linux filesystem structure created");
}
