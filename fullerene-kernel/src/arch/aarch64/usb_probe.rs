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
         // Hyper-bare bisection: jump straight to the bare pull-up\n\
         // sequence before the relocator and before any prelude. The\n\
         // gap it isolates is ABL/XBL-to-kernel-entry latency versus\n\
         // entry-to-Run/Stop controller cost (see usb_probe_hyper_bare).\n\
         .if {hyper_bare}\n\
             bl usb_probe_hyper_bare\n\
         .endif\n\
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
      // Target the EL1 physical timer at EL1 (CNTKCTL_EL1.T0EL1 = bit 10):\n\
      // with T0EL1 = 0 (the reset default) the timer is not delivered to\n\
      // EL1 as PPI 30 at all, regardless of the GICR programming below.\n\
      // Linux does the same in arch_timer_early_init.\n\
      mrs x5, CNTKCTL_EL1\n\
      orr x5, x5, #0x400\n\
      msr CNTKCTL_EL1, x5\n\
      isb\n\
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
          // GICv3 GICR CPU interface: IGROUPR0/ISENABLER0 cover PPI 0-15\n\
          // only (16 INTIDs per 32-bit register), so PPI 30 lives in\n\
          // IGROUPR1 (0x84) / ISENABLER1 (0x104) bit 14, and its priority\n\
          // in the dedicated GICR_IPRIORITYR30 word (0x400 + 4*30 = 0x478;\n\
          // GICv3 gives every INTID a full 32-bit priority register, unlike\n\
          // the GICv2 four-per-word packing the older offsets assumed).\n\
          ldr w9, [x8, #0x84]\n\
          orr w9, w9, #0x4000\n\
          str w9, [x8, #0x84]\n\
          mov w10, #0xa0\n\
          strb w10, [x8, #0x478]\n\
          ldr w9, [x8, #0x104]\n\
          orr w9, w9, #0x4000\n\
          str w9, [x8, #0x104]\n\
          // The standalone probe never runs the Rust GIC init, so force the\n\
          // distributor on: ABL may leave GICD_CTLR without Enable or without\n\
          // Group1Enable, in which case no PPI is delivered to this CPU\n\
          // interface at all. GICD_CTLR (Enable|Group1Enable = 0x3) sits at\n\
          // GICD_BASE + 0.\n\
          movz x9, #0x0000\n\
          movk x9, #0x17a0, lsl #16\n\
          ldr w10, [x9, #0x0]\n\
          orr w10, w10, #0x3\n\
          str w10, [x9, #0x0]\n\
          isb\n\
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
          // PSCI SYSTEM_RESET (function 7) first: the PS_HOLD release below\n\
          // sits in the PMIC/SPMI aperture the probe never clocks up; on this\n\
          // board that write can stall the CPU and mask a working SMC reset.\n\
          mov w0, #7\n\
          movk w0, #0x8400, lsl #16\n\
          mov x1, xzr\n\
          mov x2, xzr\n\
          mov x3, xzr\n\
          smc #0\n\
          movz x7, #0x4000\n\
          movk x7, #0x0c26, lsl #16\n\
          str wzr, [x7]\n\
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
    hyper_bare = const if cfg!(fullerene_aarch64_usb_hyper_bare) {
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
        // Host-visible proof that the timer PPI was delivered and this
        // handler runs: one SDIS blip before the reset. It is only visible
        // with the link ON (silent no-op before attach), so a stall-map run
        // shows the blip at ~T+15.5 in the host journal.
        usb::sdisc_blips_link_on(1);
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

/// Run the EP0 signal-probe channel. This is deliberately independent of the
/// gadget handoff result: a handoff that fails after Run/Stop still leaves a
/// physical attach whose re-attach cycles carry the diagnostic code. The
/// retained trace is unreachable while EP0 never enumerates, and the flooded
/// host journal drops disconnect lines, so the code is published by toggling
/// the pull-up: after a bounded observation window the probe performs
/// code+1 drop/re-attach cycles, and each re-attach produces a reliable
/// "new high-speed USB device" line with a timestamp in the host log.
#[cfg(all(
    fullerene_aarch64_usb_ep0_signal_probe,
    fullerene_aarch64_usb_gadget_handoff_probe
))]
fn run_ep0_signal_probe(signal_smmu_code: u32, signal_link_state: bool) -> ! {
    // lnk-nib PROBE-ENTRY probe: reset at the very first line, before the GIC
    // sweep and u0_arm_recovery. An early return (well under the ~T+37-39
    // watchdog-bite bucket) proves the probe IS entered; a ~T+37-39 return
    // proves it is NOT (the gate env / cfg is not reaching this branch).
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk-nib") {
        usb::park_for_seconds(0);
    }
    // Disarm the assembly recovery timer; the trace-quiet watchdog below
    // owns recovery in this mode.
    unsafe {
        asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
    }
    // The normal GIC sweep (disable every SPI/PPI ABL left armed) runs only
    // on the successful-handoff path, and this probe branches before it, so
    // ABL's fastboot IRQs (DWC3/PMIC/PDC) stay enabled while EL1 interrupts
    // are unmasked. Any stray interrupt (the host's post-timeout port reset
    // included) would enter the exception vectors and reboot the handset
    // mid-observation. Repeat the sweep here: the probe is pure polling and
    // owns its own bounded recovery, so no IRQ must remain armed.
    let _ = platform::gicv3::init(
        platform::bramble::GICD_BASE,
        platform::bramble::GICR_BASE,
        None,
    );
    // Self-heal the failed handoff before any diagnostic toggling: the
    // missing init tail (event ring, DCFG, DEPSTARTCFG, SETEPCONFIG, the
    // EP0 OUT SETUP arm, Run/Stop) is re-issued here in Linux's
    // soft_connect order. A success enters the normal enumeration flow
    // below; a failure schedules the APSS-WDT bite readout: the handset
    // reboots at a step-specific delay (probe entry is ~T+1-3 after
    // fastboot boot, Android returns ~20 s after the bite), so the loop's
    // RETURN TIME names the failed step:
    //   ~T+23-26: 1 = run/stop failed (bite at entry+2)
    //   ~T+27-30: 4 = DEPSTARTCFG failed (bite at entry+6)
    //   ~T+31-34: 5/6 = DEPCFG EP0-OUT/IN failed (bite at entry+10)
    //   ~T+37-39: core reached Run/Stop (0 = armed, 8 = arm pending);
    //             1234:0001 in the host log = success, -110 = core running
    //             but EP0 dead. Status 8 also emits one DCTL.SDIS blip at
    //             attach (a visible disconnect/re-attach pair) if DCTL is
    //             host-visible on this board. A ~37-39 return with -110
    //             can additionally mean the APSS WDT is not writable here
    //             (secure-owned) with the core stopped; an early bucket in
    //             any run calibrates the two apart.
    let arm_status = usb::u0_arm_recovery();
    match arm_status {
        1 => usb::u0_arm_wdt_bite(2),
        4 => usb::u0_arm_wdt_bite(6),
        5 => usb::u0_arm_wdt_bite(10),
        6 => usb::u0_arm_wdt_bite(10),
        // A visible blip at attach also proves the core is running AND the
        // HS link reached U0 (SDIS only acts on a running core, and the
        // blip is issued only at link ON): 37-39 s return + blips + -110 =
        // link up, EP0 data path dead; 37-39 s return + NO blips + -110 =
        // link never U0 (HS PHY never trained) or the core never ran.
        0 | 8 => usb::u0_arm_set_blips(1),
        _ => {}
    }
    // The diag readout must own every SDIS pair in the run: the u0-arm
    // blip at link ON (~T+11.3) would drop the link mid-enumeration and
    // the re-attach would repeat the host's SETUP sequence, contaminating
    // the trace the diag code decodes.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("diag")
        || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk3")
        || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("sof")
        // The forcehs run is an enumeration attempt: let the host perform the
        // fresh descriptor transaction without the link-ON blip resetting it.
        || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("forcehs")
    {
        usb::u0_arm_set_blips(0);
    }
    // Control: an unconditional bite tests the APSS-WDT bite path itself
    // (writable from here? does it land?) independent of the u0-arm status.
    if option_env!("FULLERENE_USB_WDT_BITE_CONTROL")
        .filter(|value| *value != "0")
        .is_some()
    {
        usb::u0_arm_wdt_bite(3);
    }
    // The self-heal put the core back in Run/Stop with the endpoint tail
    // (re)issued: the signal probe's diagnostic flow must not then break on
    // the SOF signal code and reset the handset before the host's
    // enumeration completes. Stay in the pet+poll survival loop (the same
    // one the real success path uses) and let poll() arm/serve EP0.
    let u0_armed = matches!(arm_status, 0 | 8);
    usb::trace_marker(
        usb::TRACE_PROBE_WATCHDOG,
        0x5349_4700 | (signal_smmu_code & 0xff),
    );
    if option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP")
        .filter(|value| *value != "0")
        .is_some()
    {
        // The early-drop check inside the handoff already owns the signal;
        // keep the pull-up down and recover immediately.
        usb::ep0_signal_drop_pullup();
        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x574454); // "WDT"
        reset_after_probe_failure();
    }
    if option_env!("FULLERENE_USB_SIGNAL_DIAG_PUBLISH") == Some("1") {
        // Diagnostic publish: the handoff may have failed BEFORE its own
        // Run/Stop (e.g. the pre-connect STARTTRANSFER differential), which
        // leaves no attach and makes every gate unreadable. Publish the
        // pull-up from here so the gate logic below still decides whether
        // the host sees a device.
        usb::ep0_signal_publish_pullup();
    }
    let include_raw_link = option_env!("FULLERENE_USB_SIGNAL_RAW_LINK")
        .filter(|value| *value != "0")
        .is_some();
    let frequency = probe_counter_frequency();
    let timeout_secs = option_env!("FULLERENE_USB_PROBE_TIMEOUT_SECS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(120);
    let mut deadline = probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
    let mut last_head = usb::trace_head();
    // The unknown ~17 s watchdog reboots the handset before a 10 s window
    // finishes when the attach lands ~9 s after probe entry; gate runs
    // shorten the window so the one-bit readout beats the bite.
    let observe_secs = option_env!("FULLERENE_USB_PROBE_OBSERVE_SECS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10);
    let observe_until = probe_counter().saturating_add(frequency.saturating_mul(observe_secs));
    let mut signal_code = signal_smmu_code;
    // A gate run needs the gate's own timing readout, so the arm-progress
    // latch must not short-circuit the observation window into the STAB
    // poll loop (the arm succeeds on -110 runs too, which is exactly when
    // the gate bit is most informative).
    let gate_active = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
        .filter(|value| *value != "0")
        .is_some();
    // lnk-nib readout now runs at probe entry (before u0_arm_recovery), so
    // it samples the natural failed-handoff link state and cannot be skipped
    // by the re-reset below.
    loop {
        usb::wdt_pet();
        usb::poll();
        if (usb::probe_ep0_progress() || u0_armed) && !gate_active {
            // Enumeration actually succeeded: stop signaling and continue as
            // the normal direct probe would.
            unsafe {
                asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
            }
            usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5354_4142); // "STAB"
            loop {
                usb::wdt_pet();
                usb::poll();
            }
        }
        if signal_code == 0 {
            signal_code = if include_raw_link {
                usb::ep0_raw_link_signal_code()
            } else if signal_link_state {
                usb::ep0_link_signal_code()
            } else {
                usb::ep0_signal_code()
            };
        }
        if signal_code == 0 {
            // Nothing live was observable; publish the previous attempts'
            // newest STARTTRANSFER outcome instead: the host counts
            // (status-nibble + 1) re-attach cycles, with 13 cycles naming a
            // wedged (timed-out) command and 1 cycle naming a clean
            // status-0 Start Transfer.
            let harvest = usb::harvest_last_str_code();
            if harvest != 0xFFFF_FFFF {
                signal_code = if harvest & 0x1_0000 != 0 {
                    13
                } else {
                    (harvest & 0xf) + 1
                };
            }
        }
        // A gate run must observe the WHOLE window: the link comes up (SOF /
        // first event / retired SETUP TRB) at attach, long before the SETUP
        // and data phase it is trying to diagnose. Breaking on a non-zero
        // signal_code here would evaluate the gate at attach (~T+10) instead
        // of at observe_until, turning a healthy SETUP into a false "not
        // processed". Only a non-gate run short-circuits on the signal.
        if (!gate_active && signal_code != 0) || probe_counter() >= observe_until {
            break;
        }
        let head = usb::trace_head();
        if head != last_head {
            last_head = head;
            deadline = probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
        } else if frequency != 0 && probe_counter() >= deadline {
            usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x574454); // "WDT"
            reset_after_probe_failure();
        }
    }
    if signal_code != 0 {
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x5349_4744 | (signal_code & 0xff),
        );
    }
    // "armalive" gate: one-bit readout of L1 (does the EP0 SETUP Start
    // Transfer retire). By window end the host has driven SETUP tokens
    // for ~1 s (its descriptor URB retries until the 5 s mark), so the
    // state collapses: a retired arm leaves either a pending TRB (the
    // armed flag) or a host DMA'd SETUP in the buffer (consumed); no
    // retired arm leaves neither. Bite early on either fact so the loop's
    // return time names L1 (return < T+35 = an arm retired; T+36-37 =
    // none = persistent command wedge). Both outcomes park in the STAB
    // loop; the pet path is inert once the bite is pending. Must run
    // before cmd_gate_condition_met, which does not know this value.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("armalive") {
        let state = usb::armalive_probe();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x414C_4C56 | (state & 0xff), // "ALLV"
        );
        if state != 0 {
            // N=1: the bite is scheduled at window end (~T+12.3), so the
            // 1 s delay lands it at ~T+13.3 and the loop returns ~T+32-34,
            // clear of the secure-WDT bucket (T+36-37, calibrated on the
            // last 11 runs).
            usb::u0_arm_wdt_bite(1);
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnkalive" gate (third pass): one-bit readout splitting the
    // non-U0, non-sleep link states at window end (DSTs.USBLNKST bits
    // 21:18; this core encodes U0 = 0). Pass one showed the state is
    // never 0, pass two (bite on U1/U2/U3) returned in the secure
    // bucket, so the core does not see U0 and is not in LPM sleep while
    // the host drives SETUP. The remaining question: is the FSM stuck
    // mid-transaction in the reset/resume handshake (RECOV = 8,
    // HRESET = 9, LPBK = 11, RESET = 0xe, RESUME = 0xf - the reset
    // de-assertion never finished, so the core ignores the host's
    // SETUP tokens) or parked in a link-down state (SS_DIS = 4,
    // RX_DET = 5, SS_INACT = 6, POLL = 7, CMPLY = 10 - the QSCRATCH
    // phantom, in which only the PHY answers the host's reset and
    // chirps while the core's link never comes up). Bite early on the
    // mid-transaction set; both outcomes park in the STAB loop. Same
    // timing contract as armalive (bite ~T+13.3, return ~T+32-34 =
    // stuck mid-transaction vs the secure bucket T+36-37 = link-down
    // phantom, threshold T+35).
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnkalive") {
        let lnkst = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_4B53 | (lnkst & 0xff), // "LNKS"
        );
        if lnkst == 8 || lnkst == 9 || lnkst == 11 || lnkst == 14
            || lnkst == 15
        {
            usb::u0_arm_wdt_bite(1);
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk3" gate: one-bit readout - did the core's link FSM ever enter a
    // mid-transaction state (RECOV=8, HRESET=9, LPBK=11, RESET=14,
    // RESUME=15) during the window, i.e., did it see the host's reset on
    // the UTMI RX path? YES = stop the core at eval (disconnect line in
    // the host journal while the port is still tracked) + APSS-WDT bite
    // ~T+13.5 (return ~T+33-34); NO = no line + the secure-WDT bucket
    // (T+36-42). Splits a UTMI-RX-dead core (FSM never woke) from a core
    // stuck in the reset handshake (RX alive). Must run before
    // cmd_gate_condition_met, like the other special gates.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk3") {
        let saw_mid = usb::lnk_mid_transaction_seen();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_4B33 | (saw_mid as u32 & 0xff), // "LNK3"
        );
        if saw_mid {
            let _ = usb::gate_true_stop_device();
            usb::u0_arm_wdt_bite(1);
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "sof" gate: two-bit readout at window end. Bit A (early APSS-WDT
    // bite, return ~T+33-34 vs the secure bucket T+36-42): the core
    // currently reads halted (DSTS.DEVCTRLHLT) or Run/Stop reads back
    // cleared (ep0_raw_link_nibble 16/17) - the stale-halt-readback family
    // that would void every DSTS-based latch. Bit B (usb 1-9 disconnect
    // line while the host still tracks the port): the DSTS SOF frame
    // number changed across a 100 ms sub-window at eval, i.e. the core
    // receives packets from the host at the transaction level even though
    // the link FSM never reported U0 or a mid-transaction state (lnk3).
    // Four outcomes: line+early, early only, line+late, none. Must run
    // before cmd_gate_condition_met, like the other special gates.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("sof") {
        let raw = usb::ep0_raw_link_nibble();
        let frequency = probe_counter_frequency();
        let subwindow = probe_counter();
        let sof_first = usb::dsts_sof_frame_number();
        while frequency == 0 || probe_counter().wrapping_sub(subwindow) < frequency / 10 {
            usb::poll();
        }
        let saw_sof = usb::dsts_sof_frame_number() != sof_first;
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x534F_4600 | (((raw & 0x1f) << 1) | (saw_sof as u32 & 1)), // "SOF?"
        );
        if saw_sof {
            let _ = usb::gate_true_stop_device();
        }
        if raw == 16 || raw == 17 {
            usb::u0_arm_wdt_bite(1);
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk57" gate: three outcomes from the DSTS.USBLNKST nibble at window
    // end. Early APSS-WDT bite (return ~T+33-34 vs the secure bucket
    // T+36-42) = state 7 (POLLING: the core's chirp phase is running - TX
    // and the link FSM woke - but it never hears the host, RX deaf below
    // the FSM). usb 1-9 disconnect line while the host still tracks the
    // port = state 5 (RX.DETECT: the FSM never started training - the
    // session/VbusValid stimulus is not reaching the core). Late
    // secure-bucket return with no line = any other state {4,6,10,12,13}.
    // Must run before cmd_gate_condition_met, like the other special gates.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk57") {
        let state = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_3537 | (state & 0x1f), // "LN57"
        );
        if state == 5 {
            let _ = usb::gate_true_stop_device();
        }
        if state == 7 {
            usb::u0_arm_wdt_bite(1);
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk4" gate: state-4 decision point after the broad lnk57 result. A
    // usb 1-9 disconnect line at eval proves USBLNKST == 4 (likely
    // SS.Disabled); no line means the broad else-bucket is one of {6,10,12,13}.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk4") {
        let state = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_5F34 | (state & 0x1f), // "LN_4"
        );
        if state == 4 {
            let _ = usb::gate_true_stop_device();
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk6" gate: split the remaining lnk57 else-bucket. A disconnect line
    // proves USBLNKST == 6; no line leaves {10,12,13}.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk6") {
        let state = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_5F36 | (state & 0x1f), // "LN_6"
        );
        if state == 6 {
            let _ = usb::gate_true_stop_device();
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk10" gate: continue splitting the lnk57 else-bucket after states
    // 4 and 6 tested false. A disconnect line proves USBLNKST == 10; no line
    // leaves {12,13}.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk10") {
        let state = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_5FA0 | (state & 0x1f), // "LN_A"
        );
        if state == 10 {
            let _ = usb::gate_true_stop_device();
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "lnk12" gate: the final split after {4,6,10} tested false. A
    // disconnect line proves USBLNKST == 12; no line leaves 13.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk12") {
        let state = usb::dsts_raw_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4C4E_5FC0 | (state & 0x1f), // "LN_C"
        );
        if state == 12 {
            let _ = usb::gate_true_stop_device();
        }
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "gdb" gate: read the raw GDBGLTSSM LINKSTATE nibble at window end and
    // time-encode it with the APSS-WDT bite. Delay = state + 1, so the SS
    // return timestamp directly names the physical link FSM value; this also
    // interprets the DSTS=13 observation without first guessing whether 13 is
    // a reserved core-state name or a legacy field offset. Must run before
    // cmd_gate_condition_met, like the other special gates.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("gdb")
        || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("gdbforce")
    {
        let state = usb::gdb_ltssm_link_state();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4744_4253 | (state & 0x1f), // "GDBS"
        );
        usb::u0_arm_wdt_bite(state.saturating_add(1));
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "rescue2" gate: mid-window full re-arm. Unlike "diag" (which
    // re-drives only the trace-named stuck stage) this forces the whole
    // endpoint tail after a device soft reset, whatever the trace says:
    // the host's read/64 URB retries its SETUP token until the 5 s
    // descriptor timeout, so a fresh SETUP arm landing inside the window
    // completes the enumeration. Readout = the journal outcome
    // (1234:0001 vs -110), the same contract as "diag". Must run before
    // cmd_gate_condition_met, which does not know this value.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("rescue2") {
        usb::u0_arm_set_blips(0);
        let status = usb::u0_arm_window_recovery();
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x5232_0000 | (status & 0xff), // "R2"
        );
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // "diag" gate: publish the composite readout code (see
    // usb::diag_readout_code) as SDIS blip pairs - pair count == code -
    // then park. The pairs land at ~T+15.3-16.8, before the ~T+17-18
    // secure-WDT bite; zero pairs means the core/link is not live at eval
    // time. Must run before cmd_gate_condition_met, which would parse
    // "diag" as a hex harvest value and fall through silently.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("diag") {
        // Rescue, don't report: re-drive the stuck stage of the host's
        // pending read/64 (see usb::rescue_read64) and then keep servicing
        // EP0 in the STAB loop - no park. The host journal is the readout:
        // enumeration progress = the rescue landed, -110 at the 5 s mark =
        // it did not (or the CPU died in the window before eval).
        let rescue = usb::rescue_read64();
        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5245_5343 | rescue); // "RESC"
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // Gate evaluation against THIS run's trace data. The gate fires at the
    // end of the observation window. The one-bit readout is the HOST
    // JOURNAL: TRUE stops the core while the host still tracks the device
    // (eval lands inside the host's 5 s descriptor window with
    // --connect-delay 0), which publishes a "usb 1-9: USB disconnect" line;
    // FALSE only clears the dead QSCRATCH votes and parks 90 s, so no line
    // appears. Both branches return in the WDT-bite bucket (~T+36-42,
    // boot-reason=watchdog) - the reset timing no longer distinguishes
    // them (calibrated in runs 2104483.0/2107000.0).
    if let Some(met) = usb::cmd_gate_condition_met() {
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x4741_5445 | (met as u32 & 0xff), // "GADE"
        );
        if met {
            // One-bit readout: STOP THE CORE. Run/Stop owns the physical
            // pull-up (CDLY=4 shifted the attach by exactly +4 s; the
            // stop-after-K runs never attach), and with --connect-delay 0
            // the eval lands ~1-2 s after attach - inside the host's 5 s
            // descriptor window, while the host still tracks the device -
            // so the stop publishes a real "usb 1-9: USB disconnect" line
            // in the host journal. The old timing split (immediate reset
            // ~25 s vs the WDT bite ~37 s) is dead: the PSCI/PS_HOLD reset
            // never lands on this board and both branches return in the
            // WDT bucket (calibrated in runs 2104483.0/2107000.0, both
            // ~T+41); the SDIS blips are dead too (84+ runs, zero lines).
            // Journal line present = TRUE, absent = FALSE.
            let _ = usb::gate_true_stop_device();
            usb::park_for_seconds(0);
        }
        usb::ep0_signal_drop_pullup();
        usb::park_after_gate_failure();
    }
    // code+1 visible re-attach cycles encode the diagnostic value in the
    // reliable attach-line count. The QSCRATCH session overrides control the
    // pull-up even when the core ignores DCTL, so each cycle stays short.
    let cycles = (signal_code as u64 + 1).min(16);
    for _ in 0..cycles {
        usb::ep0_signal_drop_pullup();
        let dropped = probe_counter().saturating_add(frequency.saturating_mul(3) / 2);
        while frequency == 0 || probe_counter() < dropped {
            usb::wdt_pet();
            usb::poll();
        }
        usb::ep0_signal_restore_pullup();
        let attached = probe_counter().saturating_add(frequency.saturating_mul(3) / 2);
        while frequency == 0 || probe_counter() < attached {
            usb::wdt_pet();
            usb::poll();
        }
    }
    usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x574454); // "WDT"
    reset_after_probe_failure();
}

/// Hyper-bare bisection probe: fire the bare pull-up sequence as the very
/// first instruction after EL1 entry, before the dynamic relocator runs and
/// before any prelude (secure-WDT SMC, APSS pet, recovery timer, park
/// watchdog). The host-visible attach time measured here is therefore the
/// ABL/XBL-to-kernel-entry latency plus the bare sequence cost alone; an
/// attach still at T+10-11 with this variant means the gap is on the
/// bootloader side and cannot be shortened from the kernel.
///
/// Safe before relocations: the whole chain touches only MMIO at const
/// physical addresses (BRAMBLE_USB_RESOURCES is const-initialized static
/// data, accessed PC-relative), BSS that _start already zeroed (also
/// accessed PC-relative), and the architectural counter for delays. No
/// absolute data pointers are read or written. The secure WDT is left
/// armed, so a hung chain self-recovers at the ~17 s secure bite.
#[cfg(fullerene_aarch64_usb_hyper_bare)]
#[unsafe(no_mangle)]
extern "C" fn usb_probe_hyper_bare() -> ! {
    unsafe { usb::init_usb2_bare_pullup_handoff() };
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "C" fn usb_probe_entry() -> ! {
    // Deactivate the SECURE watchdog first: XBL/ABL arm it for the
    // `fastboot boot` path and its bite reboots the handset ~17 s into every
    // probe (bootreason=watchdog), killing host enumeration mid-flight.
    // FULLERENE_USB_SWDD_SKIP=1 omits this SMC itself: a timing experiment
    // that isolates the SMC instruction cost (trap routing) from the rest
    // of the prelude. In that variant the secure WDT stays armed and bites
    // at ~17 s, which is harmless to the attach/-110 readouts.
    if option_env!("FULLERENE_USB_SWDD_SKIP").is_none() {
        usb::secure_wdt_disable();
    }
    // The extended SMCCC diagnostics issue several SMCs at entry; that
    // multi-SMC sequence wedges the fastboot handoff (no attach). They are
    // only consumed by the scm-*/std-*/mdcr-*/el* gates, which evaluate at
    // probe entry and do NOT need the device to attach. USB gates (setup,
    // darm, ep1-*, addr, ...) need the device to attach and enumerate, so
    // they must run on the single attaching SMC alone. Run the probes only
    // for the SMC gates; keep every other run on the minimal entry SMC.
    #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
    {
        let gate = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE").unwrap_or("");
        let needs_smc_probes = matches!(
            gate,
            "scm-answ"
                | "scm-avail"
                | "scm-noimpl"
                | "scm-dead"
                | "std-ok"
                | "std-dead"
                | "mdcr-trap"
                | "mdcr-clean"
                | "el1"
                | "el2"
        );
        if needs_smc_probes {
            usb::secure_wdt_probes();
        }
    }
    // Pet the apps watchdog next: it may also have been left counting.
    usb::wdt_pet();
    // stall-map: shorten the 60 s assembly recovery timer so it fires at
    // entry+15 s, AFTER the T+10-11 HS attach, where the core is running
    // and the link is ON. The handler's SDIS blip is then host-visible
    // (a disconnect/re-attach pair in the journal at ~T+15.5), proving the
    // timer PPI is delivered and the handler runs regardless of what the
    // subsequent PSCI SMC does: a ~T+36 return proves the SMC reset works,
    // a ~T+38 return means the SMC is dead and the PS_HOLD write in the
    // PMIC/SPMI aperture stalled the CPU (secure WDT ends the boot).
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("stall-map") {
        timer::arm_ms(15_000);
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    usb::trace_probe_begin();
    // The trace region survives warm resets; between two `fastboot boot`
    // runs Android scribbles the page unpredictably, and a surviving header
    // would make the in-boot harvest gates count the PREVIOUS run's records.
    // Start every boot's cursor at zero (once, before the first attempt).
    usb::trace_reset_head_for_boot();

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
        //
        // The USB2 handoff never consumes the orientation (only the SS QMP
        // PHY path does), so the flag-gated skip is an A/B test for the
        // ~11 s pre-attach delay: the 512-APID SPMI flat-table scan costs
        // one MMIO read per APID and each read can stall on a slow or
        // clock-gated SPMI arbiter.
        if option_env!("FULLERENE_USB_SKIP_TYPEC_SPMI") != Some("1") {
            let _ = usb::observe_typec_handoff();
        }
    }
    // The signal probe reads the Apps-SMMU stream state BEFORE the gadget
    // handoff publishes the pull-up: if the aperture is secure-owned or
    // clock-gated, the abort reboots pre-attach and the host sees no device
    // at all, which is a distinct diagnostic outcome from any timed drop.
    let signal_smmu_code = if cfg!(fullerene_aarch64_usb_ep0_signal_probe)
        && option_env!("FULLERENE_USB_SIGNAL_SMMU_STATE")
            .filter(|value| *value != "0")
            .is_some()
    {
        usb::probe_smmu_stream_state()
    } else {
        0
    };
    let signal_link_state = cfg!(fullerene_aarch64_usb_ep0_signal_probe)
        && option_env!("FULLERENE_USB_SIGNAL_LINK_STATE")
            .filter(|value| *value != "0")
            .is_some();
    let gadget_ready = if cfg!(any(
        fullerene_aarch64_usb_gadget_handoff_probe,
        fullerene_aarch64_usb_pullup_probe
    )) {
        // Fastboot may still be completing its controller teardown when the
        // temporary Image starts. Retry only this handoff boundary: a single
        // early DWC3/PHY ownership race should not turn into a false negative,
        // while the bounded count still makes a real failure recoverable.
        // A gate readout run limits itself to ONE attempt: the ~17 s
        // watchdog bite lands before the 3-attempt + fallback sequence
        // finishes, and the gate must evaluate inside the silence window.
        let attempt_limit = if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1") {
            1u32
        } else {
            3u32
        };
        let mut ready = false;
        for attempt in 0..attempt_limit {
            if attempt != 0 {
                for _ in 0..250_000u32 {
                    unsafe {
                        core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
                    }
                }
            }
            usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5254_0000 | attempt); // "RT"
            let result = if cfg!(fullerene_aarch64_usb_gadget_handoff_probe) {
                if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
                    // Exercise the same non-destructive handoff used by the
                    // normal Fullerene entry, but keep the probe watchdog and
                    // automatic recovery around it. If the direct path
                    // fails, init_usb2_handoff() performs the established
                    // diagnostic fallback in the same boot attempt.
                    usb::init_usb2_handoff()
                } else {
                    usb::init_usb2_gadget_handoff()
                }
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
    // The signal channel owns the post-init timeline ONLY when the handoff
    // failed: a successful handoff publishes a live pull-up that may be in
    // the middle of host enumeration, and the probe's diagnostic toggles or
    // recovery reboot would silently kill it (the host sees -ENODEV, which
    // it does not even log). Failed handoffs keep the full diagnostic
    // behavior: gate readouts, pull-up toggles, bounded recovery.
    #[cfg(all(
        fullerene_aarch64_usb_ep0_signal_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    ))]
    {
        // The signal channel owns the post-init timeline when the handoff
        // failed OR when a diagnostic gate needs this run's real trace data:
        // it observes for a bounded window (the enumeration flows normally
        // during it), then evaluates the gate and either continues into the
        // normal poll loop (gate true / no gate) or parks (gate false).
        let gate_active = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
            .filter(|value| *value != "0")
            .is_some();
        // PROBE-CHECK: reset HERE (inside the cfg block, before the call) to
        // determine whether this block is compiled in AND reached. An early
        // return proves both; a ~T+37-39 return proves the block is absent
        // (cfg not set) or the code path is not reached. stall-map reuses
        // this as its "init completed" signature: a ~31 s return means the
        // kernel crossed the cfg-block boundary.
        if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk-nib")
            || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("stall-map")
        {
            usb::park_for_seconds(0);
        }
        if !gadget_ready || gate_active {
            run_ep0_signal_probe(signal_smmu_code, signal_link_state);
        }
    }
    if gadget_ready {
        // lnk-nib readout on the SUCCESS path (the signal probe is never
        // entered, so this is the only reliable place to sample the core's
        // link state in the post-attach failed-handoff condition). Settle 1 s
        // (let the host finish its HS attach + port reset), read the raw
        // USBLNKST / halted / runstop state, BUCKET it, then reset 1 s per
        // bucket so EVERY code resets before the ~T+17 secure-WDT bite:
        //   bucket 0 = code 0-3   (U0=0, U3=3)        return ~T+33
        //   bucket 1 = code 4-7   (Rx.Detect=5, Polling=7)  ~T+34
        //   bucket 2 = code 8-11  (uncommon)           ~T+35
        //   bucket 3 = code 12-15 (Reset=13)           ~T+36
        //   bucket 4 = code 16-17 (halted / RUN_STOP cleared = phantom attach) ~T+37
        // A ~T+38-39 return means the readout never ran (handoff failed or
        // the gate value is not lnk-nib).
        if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("lnk-nib") {
            let frequency = probe_counter_frequency();
            let settle = probe_counter().saturating_add(frequency.saturating_mul(1));
            while frequency == 0 || probe_counter() < settle {
                usb::wdt_pet();
                core::hint::spin_loop();
            }
            let code = usb::ep0_raw_link_nibble();
            let bucket = if code <= 3 {
                0u32
            } else if code <= 7 {
                1
            } else if code <= 11 {
                2
            } else if code <= 15 {
                3
            } else {
                4
            };
            usb::trace_marker(
                usb::TRACE_PROBE_WATCHDOG,
                0x4C4E_4942 | (code & 0xff) | (bucket << 16), // "LNIB"
            );
            let reset_at = probe_counter()
                .saturating_add(frequency.saturating_mul(1u64.saturating_add(u64::from(bucket))));
            while frequency == 0 || probe_counter() < reset_at {
                usb::wdt_pet();
                core::hint::spin_loop();
            }
            usb::park_for_seconds(0);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        // Host-visible link-ON readout for the success path: the signal
        // probe queues its own blip only when the handoff failed, so without
        // this the direct branch could never show the SDIS pair.
        usb::arm_blip_queue();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if usb::gadget_handoff_stage_probe_enabled() {
            // A stage probe intentionally publishes an EP0-less pull-up, so
            // it cannot enter the normal EP0-progress watchdog. Keep the
            // electrical attach up long enough for xHCI to log it, then use
            // the same automatic reset/recovery path as a failed handoff.
            let frequency = probe_counter_frequency();
            let deadline = probe_counter().saturating_add(frequency.saturating_mul(10));
            while frequency == 0 || probe_counter() < deadline {
                usb::wdt_pet();
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
                // Direct handoff owns the event ring from the polling loop;
                // enabling the DWC3 SPI here would race that consumer.
                if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
                    None
                } else {
                    Some(platform::bramble::USB_DWC3_IRQ)
                },
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
        {
            // Self-recovery net: the secure watchdog is NOT a guaranteed
            // recovery channel - the disable SMC at entry can succeed on an
            // individual run, which leaves the bare probe parked forever
            // with both USB ports down (the handset then needs a physical
            // power cycle). A scheduled 16 s APSS-WDT bite makes every bare
            // run end on its own; when the secure watchdog bites first
            // (the normal case) this bite never lands and the readouts are
            // unchanged. wdt_pet() below cannot cancel it (the pending bite
            // owns recovery).
            usb::u0_arm_wdt_bite(16);
        }
        #[cfg(fullerene_aarch64_usb_bare_pullup_probe)]
        loop {
            // The bare probe intentionally never reads the event/DMA path;
            // keep only the physical pull-up state alive while testing the
            // controller MMIO sequence itself.
            usb::wdt_pet();
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
            if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
                // The direct handoff intentionally uses polling rather than
                // the IRQ-backed path. This keeps USB RESET and the first
                // SETUP on one serialized event-ring consumer.
                // Absolute reset bound: trace activity (e.g. a live SOF
                // stream) keeps moving the trace head, which re-extends the
                // activity deadline forever. If BOTH watchdogs are dead
                // (secure WDT disabled by the new SMC, APSS WDT not
                // functional) nothing would otherwise ever reboot the probe
                // and the handset would stay stuck outside Fastboot. Cap the
                // activity deadline at an absolute ceiling from loop entry so
                // recovery to Fastboot is guaranteed. A real enumeration
                // enters the STAB loop above and never sees this bound.
                let loop_start = probe_counter();
                let abs_deadline = option_env!("FULLERENE_USB_ABS_RESET_SECS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                    .map(|secs| loop_start.saturating_add(frequency.saturating_mul(secs)))
                    .unwrap_or(u64::MAX);
                if deadline > abs_deadline {
                    deadline = abs_deadline;
                }
                loop {
                    if usb::mmio_quiet_active() {
                        // Full zero-MMIO park: the reboot-cause bisect needs
                        // a window with NO controller OR watchdog access at
                        // all. The 60 s assembly recovery timer still owns
                        // the exit.
                        core::hint::spin_loop();
                        continue;
                    }
                    usb::wdt_pet();
                    usb::poll();
                    if usb::probe_ep0_progress() {
                        unsafe {
                            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
                        }
                        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5354_4142); // "STAB"
                        loop {
                            usb::wdt_pet();
                            usb::poll();
                        }
                    }
                    let head = usb::trace_head();
                    if head != last_head {
                        last_head = head;
                        let extended =
                            probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
                        deadline = if extended < abs_deadline {
                            extended
                        } else {
                            abs_deadline
                        };
                    } else if frequency != 0 && probe_counter() >= deadline {
                        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x574454); // "WDT"
                        reset_after_probe_failure();
                    }
                }
            }
            loop {
                // The IRQ-enabled probe drains the DWC3 ring from
                // usb_probe_irq_entry(). Polling here as well would allow an
                // interrupt to re-enter the same ring consumer and corrupt
                // EVENT_OFFSET/GEVNTCOUNT ordering.
                usb::wdt_pet();
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
                        usb::wdt_pet();
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
                usb::wdt_pet();
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
        let stage = usb::gadget_handoff_failure_stage().clamp(1, 12);
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

pub fn reset_after_probe_failure() -> ! {
    // Make a failed USB handoff recoverable without another battery-cycle.
    // PSCI SYSTEM_RESET first: the PS_HOLD release sits in the PMIC/SPMI
    // aperture that the probe path never clocks up, and on this board that
    // write can stall the CPU (every pre-fix run returned ~37-39 s from the
    // secure watchdog instead of the ~31 s PSCI reset). The PS_HOLD release
    // remains as the rejected-SMC fallback.
    unsafe {
        core::arch::asm!(
            "mov w0, #7",
            "movk w0, #0x8400, lsl #16",
            "mov x1, xzr",
            "mov x2, xzr",
            "mov x3, xzr",
            "smc #0",
            out("x0") _,
            out("x1") _,
            out("x2") _,
            out("x3") _,
            options(nostack)
        );
        core::ptr::write_volatile(0x0c26_4000usize as *mut u32, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}
