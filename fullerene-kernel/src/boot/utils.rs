use petroleum::common::FullereneFramebufferConfig;
use crate::kernel_log;

// Helper function to calculate framebuffer size with bpp validation and logging
pub fn calculate_framebuffer_size(
    config: &FullereneFramebufferConfig,
    source: &str,
) -> (Option<u64>, Option<u64>) {
    if config.bpp < 8 {
        kernel_log!(
            "Warning: Invalid bpp ({}) in {} config.",
            config.bpp,
            source
        );
        return (None, None);
    }
    let size_pixels = config.width as u64 * config.height as u64;
    let size_bytes = size_pixels * (config.bpp as u64 / 8);
    kernel_log!(
        "Calculated {} framebuffer size: {} bytes from {}x{} @ {} bpp",
        source,
        size_bytes,
        config.width,
        config.height,
        config.bpp
    );
    (Some(config.address), Some(size_bytes))
}
