#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn get_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("Failed to get workspace root")
            .to_path_buf()
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
        // Test that find_libpthread returns a valid existing path (or the fallback)
        let path = flasks::find_libpthread();
        assert!(!path.is_empty());
        // The path should either exist or be the fallback
        assert!(
            std::path::Path::new(&path).exists()
                || path == "/lib/x86_64-linux-gnu/libpthread.so.0"
        );
    }
}
