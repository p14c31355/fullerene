#![no_std]
#![no_main]

use core::{
    arch::{asm, global_asm},
    panic::PanicInfo,
};

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

const STACK_SIZE: usize = 16 * 1024;

#[cfg(fullerene_aarch64_bramble)]
const IMEM_RESTART_REASON: usize = 0x146a_b65c;

#[cfg(fullerene_aarch64_bramble)]
const IMEM_BOOTLOADER_REASON: u32 = 0x7766_5500;

#[cfg(fullerene_aarch64_bramble)]
const PS_HOLD: usize = 0x0c26_4000;

// This binary deliberately has no MMU, UART, allocator, or device-driver
// dependency. Its only job is to prove that the Android arm64 Image header,
// Bramble load address, EL transition, and Rust entry point agree. Reaching
// `probe_entry` ends in a PSCI reset, which is observable from the host when
// the phone returns to fastboot.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
         // Keep the probe position-independent: an Android bootloader may
         // choose a different 2 MiB-aligned base than the linker default.
         adr x1, probe_stack\n\
         add sp, x1, #{stack_size}\n\
         mrs x5, CurrentEL\n\
         and x5, x5, #0xc\n\
         cmp x5, #0x8\n\
         b.eq 1f\n\
         b probe_entry\n\
     1:\n\
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
         adr x5, probe_el1_entry\n\
         msr ELR_EL2, x5\n\
         mov x6, sp\n\
         msr SP_EL1, x6\n\
         isb\n\
         eret\n\
     .size _start, . - _start\n\
     .global probe_el1_entry\n\
     .type probe_el1_entry, %function\n\
     probe_el1_entry:\n\
         b probe_entry\n\
     .size probe_el1_entry, . - probe_el1_entry\n\
     .balign 16\n\
     probe_stack:\n\
         .space {stack_size}\n\
     ",
    stack_size = const STACK_SIZE,
);

#[unsafe(no_mangle)]
extern "C" fn probe_entry() -> ! {
    // Give fastboot enough time to finish the `boot` transaction before the
    // one-shot image resets the phone. Each iteration contains volatile
    // assembly, so an optimizing build cannot delete the delay.
    for _ in 0..50_000_000u32 {
        unsafe { asm!("nop", options(nomem, nostack, preserves_flags)) };
    }

    #[cfg(fullerene_aarch64_entry_halt_probe)]
    loop {
        // Deliberately remain in the loaded image. If this loop is reached,
        // the bootloader accepted the compressed Image and transferred
        // control through the AArch64 entry path; do not reset into Android.
        core::hint::spin_loop();
    }

    #[cfg(not(fullerene_aarch64_entry_halt_probe))]
    {
        // Leave an external entry marker in Qualcomm's volatile IMEM restart
        // reason before resetting.  0x77665500 is the same bootloader marker
        // used by the Linux Qualcomm restart path; unlike a UART-only marker,
        // XBL/ABL carries it into the next boot's mode/bootreason.  This is a
        // scratch register, not a partition or persistent image write.
        #[cfg(fullerene_aarch64_bramble)]
        unsafe {
            core::ptr::write_volatile(IMEM_RESTART_REASON as *mut u32, IMEM_BOOTLOADER_REASON);
            core::arch::asm!("dsb sy", "isb", options(nostack));
        }

        // Try PSCI SYSTEM_RESET first. Keeping the marker write and reset call
        // on this path makes the handoff result distinguishable from a
        // Qualcomm watchdog bite.
        unsafe {
            asm!(
                "mov w0, #9",
                "movk w0, #0x8400, lsl #16",
                "smc #0",
                out("x0") _,
                out("x1") _,
                out("x2") _,
                out("x3") _,
                options(nostack)
            );
        }

        // If firmware rejects PSCI, use the documented Qualcomm PS_HOLD
        // fallback. The Lito DT exposes this register and Linux writes zero
        // there when secure deassert-PS_HOLD is unavailable.
        #[cfg(fullerene_aarch64_bramble)]
        unsafe {
            core::ptr::write_volatile(PS_HOLD as *mut u32, 0);
        }

        // Neither reset path should return; keep the CPU parked rather than
        // falling through into arbitrary memory.
        loop {
            core::hint::spin_loop();
        }
    }
}
