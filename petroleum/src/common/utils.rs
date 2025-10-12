// Common VGA mode setup helper to avoid code duplication
pub fn setup_vga_mode_common() {
    crate::graphics::setup::setup_vga_mode_13h();
}
