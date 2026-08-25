#![no_std]
#![no_main]

use core::{
    arch::{asm, global_asm},
    panic::PanicInfo,
};

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
         // Android-style AArch64 bootloaders may hand off at EL2. Normalize\n\
         // that path to EL1h before touching the platform peripherals.\n\
         mrs x5, CurrentEL\n\
         and x5, x5, #0xc\n\
         cmp x5, #0x8\n\
         b.eq 3f\n\
         b usb_probe_el1_entry\n\
     3:\n\
         mov x5, #(1 << 31)\n\
         msr HCR_EL2, x5\n\
         msr CPTR_EL2, xzr\n\
         mov x5, #9\n\
         msr ICC_SRE_EL2, x5\n\
         isb\n\
         mov x5, #3\n\
         msr CNTHCTL_EL2, x5\n\
         msr CNTVOFF_EL2, xzr\n\
         mov x5, #0x3c5\n\
         msr SPSR_EL2, x5\n\
         adrp x5, usb_probe_el1_entry\n\
         add x5, x5, :lo12:usb_probe_el1_entry\n\
         msr ELR_EL2, x5\n\
         mov x6, sp\n\
         msr SP_EL1, x6\n\
         isb\n\
         eret\n\
     .size _start, . - _start\n\
     .balign 16\n\
     usb_probe_stack:\n\
         .space {stack_size}\n\
     ",
    stack_size = const STACK_SIZE,
);

#[cfg(fullerene_aarch64_bramble)]
const LINK_ENTRY: usize = 0x8008_0040;

// The Android bootloader may relocate the Image before jumping to it. Keep
// this routine in assembly because the Rust/GOT addresses are not usable
// until the dynamic relative relocations have been applied.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global usb_probe_el1_entry\n\
     .type usb_probe_el1_entry, %function\n\
     usb_probe_el1_entry:\n\
         adrp x5, usb_probe_vectors\n\
         add x5, x5, :lo12:usb_probe_vectors\n\
         msr VBAR_EL1, x5\n\
         isb\n\
         // Keep a failed MMIO handoff from leaving the phone permanently\n\
         // disconnected. The vector table above turns this timer IRQ into a\n\
         // PS_HOLD reset; the watchdog is disabled once gadget setup returns.\n\
         mrs x5, CNTFRQ_EL0\n\
         msr CNTP_TVAL_EL0, x5\n\
         mov w5, #1\n\
         msr CNTP_CTL_EL0, x5\n\
         isb\n\
         msr DAIFClr, #2\n\
         adr x7, _start\n\
         sub sp, sp, #16\n\
         str x0, [sp]\n\
         mov x0, x7\n\
         bl aarch64_usb_probe_apply_relocations\n\
         ldr x0, [sp]\n\
         add sp, sp, #16\n\
         b usb_probe_entry\n\
     .size usb_probe_el1_entry, . - usb_probe_el1_entry\n\
     .balign 4\n\
     .global aarch64_usb_probe_apply_relocations\n\
     .type aarch64_usb_probe_apply_relocations, %function\n\
     aarch64_usb_probe_apply_relocations:\n\
         movz x11, #{entry_0}\n\
         movk x11, #{entry_1}, lsl #16\n\
         movk x11, #{entry_2}, lsl #32\n\
         movk x11, #{entry_3}, lsl #48\n\
         sub x10, x0, x11\n\
         adr x8, __rela_dyn_start\n\
         adr x9, __rela_dyn_end\n\
     1:\n\
         cmp x8, x9\n\
         b.hs 2f\n\
         ldr x11, [x8]\n\
         ldr w12, [x8, #8]\n\
         cmp w12, #0x403\n\
         b.eq 3f\n\
         cmp w12, #0x101\n\
         b.ne 4f\n\
     3:\n\
         ldr x13, [x8, #16]\n\
         add x14, x11, x10\n\
         add x13, x13, x10\n\
         str x13, [x14]\n\
     4:\n\
         add x8, x8, #24\n\
         b 1b\n\
     2:\n\
         ret\n\
     .size aarch64_usb_probe_apply_relocations, . - aarch64_usb_probe_apply_relocations\n\
     .balign 2048\n\
     // A probe has no normal exception subsystem yet. Catch synchronous\n\
     // aborts from secure-owned Qualcomm MMIO apertures and reboot instead\n\
     // of parking forever with the phone disconnected from USB.\n\
     usb_probe_vectors:\n\
     .rept 16\n\
         b usb_probe_exception_reset\n\
         .space 124\n\
     .endr\n\
     .type usb_probe_exception_reset, %function\n\
     usb_probe_exception_reset:\n\
         movz x7, #0x4000\n\
         movk x7, #0x0c26, lsl #16\n\
         str wzr, [x7]\n\
         mov w0, #9\n\
         movk w0, #0x8400, lsl #16\n\
         smc #0\n\
     5:\n\
         wfe\n\
         b 5b\n\
     .size usb_probe_exception_reset, . - usb_probe_exception_reset\n\
     ",
    entry_0 = const (LINK_ENTRY & 0xffff),
    entry_1 = const ((LINK_ENTRY >> 16) & 0xffff),
    entry_2 = const ((LINK_ENTRY >> 32) & 0xffff),
    entry_3 = const ((LINK_ENTRY >> 48) & 0xffff),
);

#[unsafe(no_mangle)]
extern "C" fn usb_probe_entry() -> ! {
    uart::init_qcom_geni(0x0098_8000);
    uart::puts("fullerene usb probe: entry\n");

    usb::clear_dma_memory();
    uart::puts("fullerene usb probe: DMA region cleared\n");
    if usb::init_usb2_handoff() {
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
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
        core::arch::asm!(
            "mov w0, #9",
            "movk w0, #0x8400, lsl #16",
            "smc #0",
            options(nostack)
        );
    }
    loop {
        core::hint::spin_loop();
    }
}
