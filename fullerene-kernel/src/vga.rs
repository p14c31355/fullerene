use petroleum::{Color, ColorCode, ScreenChar, TextBufferOperations, graphics::text::VgaBuffer};
use spin::{Mutex, Once};
use alloc::boxed::Box;

// Initialize the VGA screen and register it as the primary console
pub fn init_vga(_physical_memory_offset: x86_64::VirtAddr, vga_virt_addr: usize) {
    petroleum::debug_log!("Initializing VGA as primary console");

    // 1. Set VGA hardware to text mode 3 (80x25 color text)
    petroleum::init_vga_text_mode_3!();

    // 2. Initialize the VgaBuffer
    let mut vga = VgaBuffer::with_address(vga_virt_addr);
    vga.enable();
    vga.set_color(Color::Green, Color::Black);
    vga.clear_screen();

    // 3. Register as primary console
    crate::graphics::set_primary_console(Box::new(vga));

    petroleum::debug_log!("VGA initialized and registered as primary console");
}
