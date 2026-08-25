#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

mod uart;
mod usb;

const STACK_SIZE: usize = 16 * 1024;

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

// Keep this probe independent from the normal Fullerene bootstrap. In
// particular, it does not enable the MMU or allocator before touching DWC3;
// this isolates the Bramble USB handoff from unrelated architecture code.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
         adr x1, usb_probe_stack\n\
         add sp, x1, #{stack_size}\n\
         adrp x3, __bss_start\n\
         add x3, x3, :lo12:__bss_start\n\
         adrp x4, __bss_end\n\
         add x4, x4, :lo12:__bss_end\n\
     1:\n\
         cmp x3, x4\n\
         b.hs 2f\n\
         str xzr, [x3], #8\n\
         b 1b\n\
     2:\n\
         b usb_probe_entry\n\
     .size _start, . - _start\n\
     .balign 16\n\
     usb_probe_stack:\n\
         .space {stack_size}\n\
     ",
    stack_size = const STACK_SIZE,
);

#[unsafe(no_mangle)]
extern "C" fn usb_probe_entry() -> ! {
    uart::init_qcom_geni(0x0098_8000);
    uart::puts("fullerene usb probe: entry\n");

    if usb::init_usb2_only() {
        uart::puts("fullerene usb probe: gadget running\n");
        loop {
            usb::poll();
        }
    }

    uart::puts("fullerene usb probe: gadget init failed\n");
    reset_after_probe_failure();
}

fn reset_after_probe_failure() -> ! {
    // Make a failed USB handoff recoverable without another battery-cycle.
    // This is the same Qualcomm PS_HOLD path used by the entry probe, with
    // PSCI retained as the generic fallback.
    unsafe {
        core::ptr::write_volatile(0x0c26_4000usize as *mut u32, 0);
        core::arch::asm!("mov w0, #0x8400", "movk w0, #9", "smc #0", options(nostack));
    }
    loop {
        core::hint::spin_loop();
    }
}
