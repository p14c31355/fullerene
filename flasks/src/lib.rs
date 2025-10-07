use std::path::Path;

/// Finds the path to `libpthread.so.0` in common locations.
///
/// This function is a workaround for the `LD_PRELOAD` issue with QEMU on some systems.
/// It checks a list of common paths for the library and returns the first one that exists.
/// If the library is not found, it returns a default path.
pub fn find_libpthread() -> Option<String> {
    const COMMON_PATHS: &[&str] = &[
        "/lib/x86_64-linux-gnu/libpthread.so.0", // Debian/Ubuntu
        "/usr/lib64/libpthread.so.0",            // Fedora/CentOS
        "/usr/lib/libpthread.so.0",              // Arch/Other
    ];

    for path in COMMON_PATHS {
        if Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}
