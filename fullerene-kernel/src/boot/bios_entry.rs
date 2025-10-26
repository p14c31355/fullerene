use crate::graphics;

use petroleum::common::VgaFramebufferConfig;

use x86_64;

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    crate::init::init_common(x86_64::VirtAddr::new(0));
    log::info!("Entering _start (BIOS mode)...");



    // Main loop
    crate::graphics::_print(format_args!("Hello QEMU by FullereneOS\n"));

    // Keep kernel running instead of exiting
    log::info!("BIOS boot complete, kernel running...");

    // Enter the main kernel scheduler loop
    crate::scheduler::scheduler_loop();
}
