use petroleum::debug_log;

/// Legacy VGA text mode fallback initialization.
/// Only used when GOP framebuffer is not available.
pub fn init_vga_legacy() {
    debug_log!("Initializing legacy VGA text mode fallback");

    // Legacy VGA text buffer
    let _vga_buffer = petroleum::graphics::text::VgaBuffer::with_address(
        petroleum::page_table::constants::VGA_MEMORY_START as usize
    );

    petroleum::serial::serial_log(format_args!("Legacy VGA text mode initialized (limited functionality)\n"));
}