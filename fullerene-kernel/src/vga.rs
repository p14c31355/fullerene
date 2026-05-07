use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations,
    graphics::text::VgaBuffer,
};
use spin::{Mutex, Once};

// Global singleton using petroleum's VgaBuffer
pub static VGA_BUFFER: Mutex<Option<VgaBuffer>> = Mutex::new(None);

// Initialize the VGA screen with the given physical memory offset and virtual address
pub fn init_vga(_physical_memory_offset: x86_64::VirtAddr, vga_virt_addr: usize) {
    petroleum::debug_log!("Initializing VGA using petroleum implementation");

    // 1. Set VGA hardware to text mode 3 (80x25 color text) FIRST
    // This tells QEMU/hardware that the display is now initialized.
    petroleum::init_vga_text_mode_3!();

    // 2. Initialize the VgaBuffer and clear the screen AFTER mode setup
    {
        let mut lock = VGA_BUFFER.lock();
        if lock.is_none() {
            let mut vga = VgaBuffer::with_address(vga_virt_addr);
            vga.enable();
            vga.set_color(Color::Green, Color::Black);
            vga.clear_screen();
            *lock = Some(vga);
        }
    }

    let mut vga_lock = VGA_BUFFER.lock();
    let writer = vga_lock.as_mut().unwrap();

    // 3. Write welcome message to the now-initialized buffer
    petroleum::vga_write_lines!(writer,
        "Hello QEMU by FullereneOS!\n";
        "This is output directly to VGA.\n"
    );
    
    // Update cursor to ensure visibility
    petroleum::update_vga_cursor!(0);
    
    petroleum::debug_log!("VGA initialized and welcome message written");
}

