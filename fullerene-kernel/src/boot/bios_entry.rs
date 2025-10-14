use crate::graphics;
use crate::hlt_loop;
use crate::init::init_common;
use petroleum::kernel_log;
use petroleum::common::VgaFramebufferConfig;

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    crate::init::init_common();
    kernel_log!("Entering _start (BIOS mode)...");

    // Graphics initialization for VGA framebuffer (graphics mode)
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    kernel_log!("VGA graphics mode initialized (BIOS mode).");

    // Main loop
    crate::graphics::_print(format_args!("Hello QEMU by FullereneOS\n"));

    // Keep kernel running instead of exiting
    kernel_log!("BIOS boot complete, kernel running...");
    hlt_loop();
}
