use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations,
    graphics::text::VgaBuffer,
};
use spin::{Mutex, Once};

// Global singleton using petroleum's VgaBuffer
pub static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

// Initialize the VGA screen with the given physical memory offset
pub fn init_vga(_physical_memory_offset: x86_64::VirtAddr) {
    petroleum::debug_log!("Initializing VGA using petroleum implementation");
    
    // Use petroleum's VgaBuffer
    VGA_BUFFER.call_once(|| {
        let mut vga = VgaBuffer::new();
        vga.enable();
        vga.set_color(Color::Green, Color::Black);
        vga.clear_screen();
        Mutex::new(vga)
    });

    let mut writer = VGA_BUFFER.get().unwrap().lock();

    // Set VGA to text mode 3 (80x25 color text)
    petroleum::init_vga_text_mode_3!();

    petroleum::vga_write_lines!(writer,
        "Hello QEMU by FullereneOS!\n";
        "This is output directly to VGA.\n"
    );
    
    // Update cursor to ensure visibility
    petroleum::update_vga_cursor!(0);
    
    petroleum::debug_log!("VGA initialized and welcome message written");
}

