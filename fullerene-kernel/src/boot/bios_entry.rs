use crate::graphics;

use petroleum::common::VgaFramebufferConfig;

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    // Integrated init_common functionality
    petroleum::init_log!("init_common: About to init VGA");
    crate::vga::init_vga();
    petroleum::init_log!("VGA init done");
    petroleum::serial::serial_init();
    log::info!("Entering _start (BIOS mode)...");

    // Graphics initialization for VGA framebuffer (graphics mode)
    // Note: Traditional VGA only supports up to 8-bit, but we set to 32 for consistency
    // In practice, graphics operations will still be limited to 256 colors
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    log::info!("VGA graphics mode initialized (BIOS mode).");

    // Main loop
    crate::graphics::_print(format_args!("Hello QEMU by FullereneOS\n"));

    // Keep kernel running instead of exiting
    log::info!("BIOS boot complete, kernel running...");
    petroleum::halt_loop();
}
