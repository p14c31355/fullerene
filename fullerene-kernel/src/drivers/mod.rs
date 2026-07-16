pub mod fat;
pub mod registry;
pub mod virtio_gpu;

// Preserve the existing public path while grouping exFAT with the FAT-family
// mount dispatcher.
pub use fat::exfat;
