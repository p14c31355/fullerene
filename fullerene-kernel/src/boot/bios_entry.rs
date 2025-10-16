use crate::graphics;

use petroleum::common::VgaFramebufferConfig;

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    crate::init::init_common();
    petroleum::serial::serial_init();
    log::info!("Entering _start (BIOS mode)...");

    // Graphics initialization for VGA framebuffer (graphics mode)
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
