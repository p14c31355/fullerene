#![no_std]
#![no_main]

use core::{
    arch::{asm, global_asm},
    panic::PanicInfo,
};

#[path = "../../platform/mod.rs"]
mod platform;
mod timer;
mod uart;
mod usb;
mod usb_protocol;
mod usb_regs;

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

const LINK_ENTRY: usize = 0x8008_0040;

#[inline]
fn probe_counter() -> u64 {
    let value: u64;
    unsafe {
        asm!("mrs {value}, CNTPCT_EL0", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[inline]
fn probe_counter_frequency() -> u64 {
    let value: u64;
    unsafe {
        asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

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
         // The bare pull-up probe must isolate USB MMIO from the optional\n\
         // GIC/timer setup below. Some firmware-owned redistributors reject\n\
         // these accesses before Rust has a chance to test the controller.\n\
         .if {minimal}\n\
             adr x7, _start\n\
             sub sp, sp, #16\n\
             str x0, [sp]\n\
             mov x0, x7\n\
             bl aarch64_usb_probe_apply_relocations\n\
             ldr x0, [sp]\n\
             add sp, sp, #16\n\
             b usb_probe_entry\n\
         .endif\n\
         // Keep a failed MMIO handoff from leaving the phone permanently\n\
         // disconnected. The vector table above turns this timer IRQ into a\n\
         // PS_HOLD reset; the normal gadget path disables it after EP0\n\
         // progress, while pullup-only diagnostics leave it armed so a\n\
         // failed physical handoff cannot strand the handset.\n\
         mrs x5, CNTFRQ_EL0\n\
         // Leave enough time for Qualcomm MMIO synchronization, GENI UART\n\
         // diagnostics, and the DWC3 reset handshake before the recovery\n\
         // timer can reboot the handset.\n\
         mov x6, #60\n\
         mul x5, x5, x6\n\
         msr CNTP_TVAL_EL0, x5\n\
         mov w5, #1\n\
         msr CNTP_CTL_EL0, x5\n\
         isb\n\
         // The recovery timer is the EL1 physical-timer PPI (INTID 30).
         // This standalone probe does not run the normal Rust GIC init, so
         // bring up the Bramble redistributor and CPU interface before
         // unmasking IRQs. Bound the firmware-owned redistributor wait.
         // Bramble GICR_BASE is 0x17a60000. Keep the full 32-bit address\n\
         // here; 0x017a6000 targets an unrelated unmapped window and leaves\n\
         // the recovery timer and USB SPI interrupts unserviced.\n\
         movz x8, #{gicr_0}\n\
         movk x8, #{gicr_1}, lsl #16\n\
         movk x8, #{gicr_2}, lsl #32\n\
         movk x8, #{gicr_3}, lsl #48\n\
         ldr w9, [x8, #0x14]\n\
         bic w9, w9, #2\n\
         str w9, [x8, #0x14]\n\
         mov w10, #0xffff\n\
         movk w10, #1, lsl #16\n\
     4:\n\
         ldr w9, [x8, #0x14]\n\
         tst w9, #4\n\
         b.eq 5f\n\
         subs w10, w10, #1\n\
         b.ne 4b\n\
         b usb_probe_exception_reset\n\
     5:\n\
         add x8, x8, #0x10000\n\
         ldr w9, [x8, #0x80]\n\
         mov w10, #0x40000000\n\
         orr w9, w9, w10\n\
         str w9, [x8, #0x80]\n\
         mov w10, #0xa0\n\
         strb w10, [x8, #0x41e]\n\
         mov w10, #0x40000000\n\
         str w10, [x8, #0x100]\n\
         mov x10, #1\n\
         msr ICC_SRE_EL1, x10\n\
         isb\n\
         mov x10, #0xff\n\
         msr ICC_PMR_EL1, x10\n\
         mov x10, #1\n\
         msr ICC_IGRPEN1_EL1, x10\n\
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
     // Keep the exception table in the linker script's dedicated aligned\n\
     // section. If it stays in .text.boot, its 2 KiB alignment raises the\n\
     // whole boot section's address and shifts _start away from the Image\n\
     // payload/linker base used by the Android loader and PIE relocation\n\
     // bootstrap.\n\
     .section .text.exception_vectors,\"ax\"\n\
     .balign 2048\n\
     // A probe has no normal exception subsystem yet. Catch synchronous\n\
     // aborts from secure-owned Qualcomm MMIO apertures and reboot instead\n\
     // of parking forever with the phone disconnected from USB.\n\
     usb_probe_vectors:\n\
     .rept 4\n\
         .if {gadget_exception}\n\
             b usb_probe_exception_fallback\n\
         .else\n\
             b usb_probe_exception_reset\n\
         .endif\n\
         .space 124\n\
         .if {gadget_exception}\n\
             b usb_probe_irq_entry\n\
         .else\n\
             b usb_probe_exception_reset\n\
         .endif\n\
         .space 124\n\
         .if {gadget_exception}\n\
             b usb_probe_exception_fallback\n\
         .else\n\
             b usb_probe_exception_reset\n\
         .endif\n\
         .space 124\n\
         .if {gadget_exception}\n\
             b usb_probe_exception_fallback\n\
         .else\n\
             b usb_probe_exception_reset\n\
         .endif\n\
         .space 124\n\
     .endr\n\
     .type usb_probe_irq_entry, %function\n\
     usb_probe_irq_entry:\n\
         sub sp, sp, #256\n\
         stp x0, x1, [sp, #0]\n\
         stp x2, x3, [sp, #16]\n\
         stp x4, x5, [sp, #32]\n\
         stp x6, x7, [sp, #48]\n\
         stp x8, x9, [sp, #64]\n\
         stp x10, x11, [sp, #80]\n\
         stp x12, x13, [sp, #96]\n\
         stp x14, x15, [sp, #112]\n\
         stp x16, x17, [sp, #128]\n\
         stp x18, x19, [sp, #144]\n\
         stp x20, x21, [sp, #160]\n\
         stp x22, x23, [sp, #176]\n\
         stp x24, x25, [sp, #192]\n\
         stp x26, x27, [sp, #208]\n\
         stp x28, x29, [sp, #224]\n\
         str x30, [sp, #240]\n\
         bl usb_probe_irq\n\
         ldr x30, [sp, #240]\n\
         ldp x28, x29, [sp, #224]\n\
         ldp x26, x27, [sp, #208]\n\
         ldp x24, x25, [sp, #192]\n\
         ldp x22, x23, [sp, #176]\n\
         ldp x20, x21, [sp, #160]\n\
         ldp x18, x19, [sp, #144]\n\
         ldp x16, x17, [sp, #128]\n\
         ldp x14, x15, [sp, #112]\n\
         ldp x12, x13, [sp, #96]\n\
         ldp x10, x11, [sp, #80]\n\
         ldp x8, x9, [sp, #64]\n\
         ldp x6, x7, [sp, #48]\n\
         ldp x4, x5, [sp, #32]\n\
         ldp x2, x3, [sp, #16]\n\
         ldp x0, x1, [sp, #0]\n\
         add sp, sp, #256\n\
         eret\n\
     .size usb_probe_irq_entry, . - usb_probe_irq_entry\n\
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
    gicr_0 = const (platform::bramble::GICR_BASE & 0xffff),
    gicr_1 = const ((platform::bramble::GICR_BASE >> 16) & 0xffff),
    gicr_2 = const ((platform::bramble::GICR_BASE >> 32) & 0xffff),
    gicr_3 = const ((platform::bramble::GICR_BASE >> 48) & 0xffff),
    gadget_exception = const if cfg!(fullerene_aarch64_usb_gadget_handoff_probe) {
        1
    } else {
        0
    },
    minimal = const if cfg!(fullerene_aarch64_usb_bare_pullup_probe) {
        1
    } else {
        0
    },
);

#[unsafe(no_mangle)]
extern "C" fn usb_probe_irq() {
    // The assembly vector contains this common entry point even for probe
    // variants that route every exception to the reset stub. Keep the symbol
    // linkable for those variants; only the gadget-handoff vector actually
    // dispatches here.
    let interrupt_id: u64;
    unsafe {
        asm!(
            "mrs {interrupt_id}, ICC_IAR1_EL1",
            interrupt_id = out(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
    let interrupt = interrupt_id as u32;
    // INTID 30 is the EL1 physical-timer PPI armed by the standalone probe.
    // It is not part of the Qualcomm USB IRQ resource table, so handle it
    // before the platform IRQ filter; otherwise a no-host probe can remain
    // forever in WFE after the handoff fails.
    if interrupt == timer::TIMER_PPI {
        reset_after_probe_failure();
    }
    if platform::bramble::is_usb_irq(interrupt) {
        let controller_irq = interrupt == platform::bramble::usb_controller_irq();
        if !controller_irq {
            usb::handle_platform_irq(interrupt);
            if interrupt == platform::bramble::usb_typec_parent_irq() {
                unsafe {
                    platform::gicv3::disable_spis(platform::bramble::GICD_BASE, &[interrupt]);
                }
            }
        } else {
            // Auxiliary Qualcomm IRQs only notify the platform layer. Keep
            // DWC3 event-ring consumption on the controller IRQ; the WFE
            // loop services deferred Type-C work after eret.
            usb::poll();
        }
        unsafe { asm!("dsb sy", options(nostack)) };
    }
    unsafe {
        asm!(
            "msr ICC_EOIR1_EL1, {interrupt_id}",
            interrupt_id = in(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
#[unsafe(no_mangle)]
extern "C" fn usb_probe_exception_fallback() -> ! {
    // Keep a synchronous abort during the experimental EP0 path observable,
    // but do not publish an EP0-less pull-up. The host must see an attach only
    // after the complete gadget handoff has succeeded; otherwise an abort is
    // indistinguishable from a broken descriptor/EP0 path.
    usb::trace_marker(usb::TRACE_EXCEPTION_SYNC, 0);
    reset_after_probe_failure();
}

#[unsafe(no_mangle)]
extern "C" fn usb_probe_entry() -> ! {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    usb::trace_probe_begin();

    // The bare pull-up probe deliberately stays below the DMA/trace boundary:
    // an invalid retained DRAM aperture must not mask a controller-only
    // handoff result. The gadget probe clears its DMA objects only after the
    // DWC3 handoff stop boundary, where the USB module owns the old Fastboot
    // DMA lifetime. The preserve-core differential uses the halted controller
    // boundary without asserting CSFTRST.
    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    usb::clear_dma_memory();
    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    usb::trace_marker(usb::TRACE_BOOT_USB_ENTRY, 0);
    // The normal Bramble entry prepares the PMIC Type-C role here.  The
    // gadget-handoff probe intentionally skips that SPMI access: if the
    // standalone probe resets before reaching DWC3, this keeps the probe
    // useful for separating a PMIC aperture fault from a controller fault.
    #[cfg(all(
        fullerene_aarch64_bramble,
        not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_bare_pullup_probe
        ))
    ))]
    usb::trace_marker(usb::TRACE_TYPEC_BEGIN, 0);
    #[cfg(all(
        fullerene_aarch64_bramble,
        not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_bare_pullup_probe
        ))
    ))]
    let _typec_state = unsafe { platform::bramble::prepare_usb_device_role() };
    #[cfg(all(
        fullerene_aarch64_bramble,
        not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_bare_pullup_probe
        ))
    ))]
    if let Some(typec) = _typec_state {
        usb::set_typec_orientation(typec.orientation_reverse);
    }
    #[cfg(all(
        fullerene_aarch64_bramble,
        not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_bare_pullup_probe
        ))
    ))]
    usb::trace_marker(usb::TRACE_TYPEC_DONE, 0);

    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    uart::init_qcom_geni(0x0098_8000);
    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    uart::puts("fullerene usb probe: entry\n");

    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_halt_probe
    )))]
    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe,
        fullerene_aarch64_usb_halt_probe
    )))]
    uart::puts("fullerene usb probe: DMA region cleared\n");
    #[cfg(all(
        fullerene_aarch64_bramble,
        fullerene_aarch64_usb_gadget_handoff_probe,
        any(
            fullerene_aarch64_usb_probe_irq_typec,
            fullerene_aarch64_usb_probe_irq_typec_role
        )
    ))]
    {
        // The Type-C IRQ variant is a platform comparison, so initialize the
        // PMIC role/child interrupt contract before exposing the parent SPI.
        // Other standalone probes intentionally skip SPMI discovery.
        usb::note_platform_powered();
        if let Some(typec) = unsafe { platform::bramble::prepare_usb_device_role() } {
            usb::install_typec_state(typec);
            usb::set_typec_orientation(typec.orientation_reverse);
            usb::note_typec_attached(typec.attached);
            #[cfg(fullerene_aarch64_usb_probe_irq_typec)]
            let _ = unsafe { platform::bramble::configure_typec_irq(&typec) };
        }
    }
    #[cfg(all(
        fullerene_aarch64_bramble,
        fullerene_aarch64_usb_gadget_handoff_probe,
        not(any(
            fullerene_aarch64_usb_probe_irq_typec,
            fullerene_aarch64_usb_probe_irq_typec_role
        ))
    ))]
    {
        // The default probe uses the non-destructive observer rather than
        // rewriting PMIC role bits. This supplies the orientation and the
        // initial runtime state that Android's role-switch path would have
        // installed before binding the UDC.
        let _ = usb::observe_typec_handoff();
    }
    let gadget_ready = if cfg!(any(
        fullerene_aarch64_usb_gadget_handoff_probe,
        fullerene_aarch64_usb_pullup_probe
    )) {
        // Fastboot may still be completing its controller teardown when the
        // temporary Image starts. Retry only this handoff boundary: a single
        // early DWC3/PHY ownership race should not turn into a false negative,
        // while the bounded count still makes a real failure recoverable.
        let mut ready = false;
        for attempt in 0..3u32 {
            if attempt != 0 {
                for _ in 0..250_000u32 {
                    unsafe {
                        core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
                    }
                }
            }
            usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5254_0000 | attempt); // "RT"
            let result = if cfg!(fullerene_aarch64_usb_gadget_handoff_probe) {
                usb::init_usb2_gadget_handoff()
            } else {
                usb::init_usb2_pullup_handoff()
            };
            if result {
                ready = true;
                break;
            }
        }
        ready
    } else if cfg!(fullerene_aarch64_usb_bare_pullup_probe) {
        usb::init_usb2_bare_pullup_handoff()
    } else if cfg!(fullerene_aarch64_usb_cold_halt_probe) {
        usb::init()
    } else if cfg!(fullerene_aarch64_usb_halt_probe) {
        usb::init_usb2_pullup_handoff()
    } else {
        usb::init_usb2_handoff()
    };
    if gadget_ready {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if usb::gadget_handoff_stage_probe_enabled() {
            // A stage probe intentionally publishes an EP0-less pull-up, so
            // it cannot enter the normal EP0-progress watchdog. Keep the
            // electrical attach up long enough for xHCI to log it, then use
            // the same automatic reset/recovery path as a failed handoff.
            let frequency = probe_counter_frequency();
            let deadline = probe_counter().saturating_add(frequency.saturating_mul(10));
            while frequency == 0 || probe_counter() < deadline {
                core::hint::spin_loop();
            }
            reset_after_probe_failure();
        }
        // Keep the assembly recovery timer armed until the first EP0 DATA or
        // STATUS transfer. If the controller reports init success but never
        // becomes host-visible, the timer IRQ returns the handset to
        // Fastboot instead of leaving it stuck with no USB device.
        #[cfg(not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_pullup_probe
        )))]
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        {
            // Run the same post-controller GIC/USB-SPI ownership boundary as
            // the normal Bramble entry. The standalone probe otherwise
            // exercises only polling, so it cannot distinguish a controller
            // problem from an IRQ-path teardown.
            let _ = platform::gicv3::init(
                platform::bramble::GICD_BASE,
                platform::bramble::GICR_BASE,
                Some(platform::bramble::USB_DWC3_IRQ),
            );
            #[cfg(fullerene_aarch64_usb_probe_irq_power)]
            unsafe {
                platform::gicv3::enable_spis(
                    platform::bramble::GICD_BASE,
                    &[platform::bramble::USB_PWR_EVENT_IRQ],
                );
            }
            #[cfg(any(
                fullerene_aarch64_usb_probe_irq_typec,
                fullerene_aarch64_usb_probe_irq_typec_role
            ))]
            unsafe {
                platform::gicv3::enable_spis(
                    platform::bramble::GICD_BASE,
                    &[platform::bramble::usb_typec_parent_irq()],
                );
            }
            #[cfg(fullerene_aarch64_usb_probe_irq_pdc)]
            unsafe {
                let _ = platform::bramble::configure_usb_pdc_irqs();
                platform::gicv3::enable_spis_with_triggers(
                    platform::bramble::GICD_BASE,
                    &[
                        (platform::bramble::USB_PDC_DP_HS_PARENT_IRQ, true),
                        (platform::bramble::USB_PDC_SS_PARENT_IRQ, false),
                        (platform::bramble::USB_PDC_DM_HS_PARENT_IRQ, true),
                    ],
                );
            }
            #[cfg(fullerene_aarch64_usb_probe_irq_smmu)]
            unsafe {
                let resources = platform::bramble::usb_resources();
                platform::gicv3::enable_spis(
                    platform::bramble::GICD_BASE,
                    &resources.smmu_context_irqs[..resources.smmu_context_irq_count],
                );
                platform::gicv3::enable_spis(
                    platform::bramble::GICD_BASE,
                    &[resources.smmu_global_irq],
                );
            }
            unsafe {
                asm!("msr DAIFClr, #2", "isb", options(nostack));
            }
        }
        #[cfg(fullerene_aarch64_usb_bare_pullup_probe)]
        loop {
            // The bare probe intentionally never reads the event/DMA path;
            // keep only the physical pull-up state alive while testing the
            // controller MMIO sequence itself.
            core::hint::spin_loop();
        }
        #[cfg(not(any(
            fullerene_aarch64_usb_bare_pullup_probe,
            fullerene_aarch64_usb_gadget_handoff_probe
        )))]
        uart::puts(if cfg!(fullerene_aarch64_usb_pullup_probe) {
            "fullerene usb probe: physical pull-up running\n"
        } else {
            "fullerene usb probe: gadget running\n"
        });
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        {
            // A successful init with no host-visible attach is still a
            // failed diagnostic from the user's perspective. Keep the trace
            // across a warm reset, but do not leave the handset permanently
            // outside Fastboot when the cable/session never produces an
            // event. Activity extends the deadline, so a real enumeration is
            // not interrupted by this recovery path.
            let frequency = probe_counter_frequency();
            // The IRQ-enabled probe may legitimately go quiet after the
            // initial enumeration. Give that path enough time to separate a
            // real IRQ/controller failure from the diagnostic recovery reset.
            let timeout_secs = option_env!("FULLERENE_USB_PROBE_TIMEOUT_SECS")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(120);
            let mut deadline =
                probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
            let mut last_head = usb::trace_head();
            loop {
                // The IRQ-enabled probe drains the DWC3 ring from
                // usb_probe_irq_entry(). Polling here as well would allow an
                // interrupt to re-enter the same ring consumer and corrupt
                // EVENT_OFFSET/GEVNTCOUNT ordering.
                unsafe { asm!("wfe", options(nomem, nostack)) };
                usb::service_deferred_platform();
                if usb::probe_ep0_progress() {
                    unsafe {
                        asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
                    }
                    // An idle descriptor-only gadget is healthy after one
                    // EP0 transfer has been accepted. Do not mistake the
                    // absence of further host traffic for a failed handoff;
                    // retain the no-host watchdog only until EP0 makes this
                    // first progress boundary.
                    usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5354_4142); // "STAB"
                    loop {
                        unsafe { asm!("wfe", options(nomem, nostack)) };
                        usb::service_deferred_platform();
                    }
                }
                let head = usb::trace_head();
                if head != last_head {
                    last_head = head;
                    deadline =
                        probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
                } else if frequency != 0 && probe_counter() >= deadline {
                    usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x574454); // "WDT"
                    reset_after_probe_failure();
                }
            }
        }
        #[cfg(fullerene_aarch64_usb_pullup_probe)]
        {
            // Do not rely solely on the EL1 physical-timer PPI for recovery.
            // The pullup-only mode deliberately avoids the normal IRQ path,
            // and a firmware-owned GIC can leave that PPI masked even though
            // the generic counter itself is running. Keep a polling deadline
            // as a second recovery path so a failed physical handoff cannot
            // strand the handset after the bootloader disconnects.
            let frequency = probe_counter_frequency();
            let deadline = probe_counter().saturating_add(frequency.saturating_mul(60));
            loop {
                // Pullup-only deliberately never owns the DWC3 event ring.
                // Calling usb::poll() here would consume Fastboot's stale
                // GEVNTCOUNT/EVENTS state and turn this physical-only probe
                // into an accidental DMA/event-ring probe. The recovery
                // deadline needs only the architectural counter; leave all
                // controller event handling out of this mode.
                core::hint::spin_loop();
                if frequency != 0 && probe_counter() >= deadline {
                    usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x50554c4c); // "PULL"
                    reset_after_probe_failure();
                }
            }
        }
        #[cfg(not(fullerene_aarch64_usb_pullup_probe))]
        loop {
            usb::poll();
        }
    }

    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    uart::puts("fullerene usb probe: gadget init failed\n");
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    {
        // Do not publish a pull-up after gadget initialization failed. The
        // old fallback exposed an EP0-less device, making the host's
        // descriptor timeout indistinguishable from an EP0/DMA failure after
        // a successful init. Only the success path is allowed to advertise
        // the device.
        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x4641_494c); // "FAIL"
        // Encode the first failing handoff boundary as reset latency. This
        // keeps the diagnostic observable without reintroducing the broken
        // EP0-less pull-up that previously produced misleading -110 errors.
        let stage = usb::gadget_handoff_failure_stage().clamp(1, 7);
        let frequency = probe_counter_frequency();
        let delay = frequency.saturating_mul((stage as u64) * 3);
        let deadline = probe_counter().saturating_add(delay);
        while frequency != 0 && probe_counter() < deadline {
            core::hint::spin_loop();
        }
        reset_after_probe_failure();
    }
    #[cfg(fullerene_aarch64_usb_halt_probe)]
    loop {
        // Preserve the failed handoff for host-side observation instead of
        // immediately rebooting into Android or Fastboot.
        usb::poll();
    }
    #[cfg(fullerene_aarch64_usb_cold_halt_probe)]
    loop {
        // Preserve the cold PHY/clock handoff for host-side observation.
        usb::poll();
    }
    #[cfg(not(any(
        fullerene_aarch64_usb_gadget_handoff_probe,
        fullerene_aarch64_usb_halt_probe,
        fullerene_aarch64_usb_cold_halt_probe
    )))]
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
            out("x0") _,
            out("x1") _,
            out("x2") _,
            out("x3") _,
            options(nostack)
        );
    }
    loop {
        core::hint::spin_loop();
    }
}
