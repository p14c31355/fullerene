//! FAT mounting for the ESP32 SDMMC adapter.

use crate::{block::BlockDevice, fat, fs::FsError, io::Read};
use alloc::{boxed::Box, vec::Vec};

/// Validate a boot-sector image without requiring hardware access.
pub fn boot_signature_valid(image: &[u8]) -> bool {
    image.len() >= 512 && image[510] == 0x55 && image[511] == 0xaa
}

/// Mount a Genome block device using the shared FAT implementation.
pub fn mount(
    device: Box<dyn BlockDevice>,
) -> Result<Box<dyn crate::vfs::FileSystem>, (FsError, Option<Box<dyn BlockDevice>>)> {
    fat::mount_device(device)
}

/// Read a bounded disk image for deterministic host diagnostics.
pub fn read_disk_image(mut source: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut image = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) if image.len() + count <= limit => image.extend_from_slice(&chunk[..count]),
            Ok(_) => return None,
            Err(_) => return None,
        }
    }
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_valid_boot_signature() {
        let mut image = alloc::vec![0u8; 512];
        image[510] = 0x55;
        image[511] = 0xaa;
        assert!(boot_signature_valid(&image));
    }
}
