#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn get_workspace_root() -> PathBuf {
        let mut path = std::env::current_dir().expect("Failed to get current directory");
        while !path.join("Cargo.toml").exists() || !path.join("flasks").exists() {
            if !path.pop() {
                panic!("Could not find workspace root");
            }
        }
        path
    }

    #[test]
    fn test_workspace_root_finding() {
        let root = get_workspace_root();
        assert!(root.join("Cargo.toml").exists());
        assert!(root.join("flasks").exists());
    }

    #[test]
    fn test_build_paths() {
        let workspace_root = get_workspace_root();
        let target_dir = workspace_root
            .join("target")
            .join("x86_64-unknown-uefi")
            .join("debug");
        let kernel_path = target_dir.join("fullerene-kernel.efi");
        let bellows_path = target_dir.join("bellows.efi");

        // Check that paths are constructed correctly (files may not exist in test environment)
        assert_eq!(kernel_path.file_name().unwrap(), "fullerene-kernel.efi");
        assert_eq!(bellows_path.file_name().unwrap(), "bellows.efi");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_find_libpthread() {
        // Since find_libpthread is a fn in flasks/src/main.rs, we test the logic directly
        // Instead of testing the function directly, we'll test similar path logic
        let path_candidates = [
            "/lib/x86_64-linux-gnu/libpthread.so.0",
            "/usr/lib64/libpthread.so.0",
            "/usr/lib/libpthread.so.0",
        ];

        let found = path_candidates
            .iter()
            .any(|path| std::path::Path::new(path).exists());

        // Should find at least one libpthread on typical Linux systems
        // This test can be adjusted based on your system configuration
        assert!(found, "libpthread.so.0 was not found in any of the common paths.");
    }
}
