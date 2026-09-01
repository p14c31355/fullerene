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
         // Hyper-bare isolates ABL/XBL-to-entry latency from entry-to-Run/Stop cost.
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
      // Timer IRQ is the recovery net; gadget disables it after EP0 progress, pullup-only leaves it armed.
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
         // Recovery timer is PPI 30. Bring up Bramble GICR_BASE (full 32-bit 0x17a60000), distributor,
         // CPU interface, and IRQs; a truncated address leaves timer/USB SPIs unserviced.
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
          // GICv3 GICR SGI frame registers cover SGI/PPI INTIDs 0-31, so\n\
          // PPI 30 uses bit 30 in IGROUPR0/ISENABLER0. Priorities are one\n\
          // byte per INTID, making PPI 30's priority byte 0x400 + 30.\n\
          ldr w9, [x8, #0x80]\n\
          orr w9, w9, #0x40000000\n\
          str w9, [x8, #0x80]\n\
          mov w10, #0xa0\n\
          strb w10, [x8, #0x41e]\n\
          ldr w9, [x8, #0x100]\n\
          orr w9, w9, #0x40000000\n\
          str w9, [x8, #0x100]\n\
          // Force GICD Group1 enable; ABL may leave it disabled, blocking PPI delivery.
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
     // Keep vectors in the aligned linker section; .text.boot would shift _start away from the Image base.
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
         // PSCI SYSTEM_RESET (function 9) first: the PS_HOLD release below\n\
          // sits in the PMIC/SPMI aperture the probe never clocks up; on this\n\
          // board that write can stall the CPU and mask a working SMC reset.\n\
         mov w0, #9\n\
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
    // Keep the common IRQ symbol linkable even when all exceptions route to reset; only gadget dispatches.
    let interrupt_id: u64;
    unsafe {
        asm!(
            "mrs {interrupt_id}, ICC_IAR1_EL1",
            interrupt_id = out(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
    let interrupt = interrupt_id as u32;
    // Handle timer PPI before platform IRQ filtering; otherwise a no-host probe can stay in WFE forever.
    if interrupt == timer::TIMER_PPI {
        // One SDIS blip proves timer PPI delivery and handler execution on link-ON stall-map runs.
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
    // Publish a sync abort, but no EP0-less pull-up; host attach must imply the complete gadget handoff.
    usb::trace_marker(usb::TRACE_EXCEPTION_SYNC, 0);
    reset_after_probe_failure();
}

/// Run the independent EP0 signal channel: failed handoffs keep a physical attach, and bounded
/// observation followed by code+1 drop/re-attach cycles publishes diagnostics in the host journal.
fn cmd_gate_is(name: &str) -> bool {
    option_env!("FULLERENE_USB_SIGNAL_CMD_GATE").filter(|value| *value != "0") == Some(name)
}

fn env_flag(value: Option<&'static str>) -> bool {
    value.filter(|value| *value != "0").is_some()
}

fn trace_gate(code: u32) {
    usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, code);
}

const TRACE_WDT: u32 = 0x574454; // "WDT"
const TRACE_STAB: u32 = 0x5354_4142; // "STAB"

fn disarm_recovery_timer() {
    unsafe { asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack)) };
}

fn stable_park() -> ! {
    loop {
        usb::wdt_pet();
        usb::poll();
    }
}

fn park_without_recovery_timer() -> ! {
    disarm_recovery_timer();
    stable_park()
}

fn stable_ep0_park() -> ! {
    disarm_recovery_timer();
    trace_gate(TRACE_STAB);
    stable_park()
}

fn wait_arch_ticks(deadline: u64) {
    while usb::arch_counter_ticks() < deadline {
        usb::wdt_pet();
        usb::poll();
    }
}

fn poll_until_probe_ticks(frequency: u64, deadline: u64) {
    while frequency != 0 && probe_counter() < deadline {
        usb::wdt_pet();
        usb::poll();
    }
}

fn spin_until_probe_ticks(frequency: u64, deadline: u64) {
    while frequency != 0 && probe_counter() < deadline {
        usb::wdt_pet();
        core::hint::spin_loop();
    }
}

fn env_seconds(value: Option<&'static str>, default: u64) -> u64 {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(all(
    fullerene_aarch64_usb_ep0_signal_probe,
    fullerene_aarch64_usb_gadget_handoff_probe
))]
fn run_ep0_signal_probe(signal_smmu_code: u32, signal_link_state: bool, gadget_ready: bool) -> ! {
    // lnk-nib entry check: early return proves entry; T+37-39 means gate/cfg missed.
    if cmd_gate_is("lnk-nib") {
        usb::park_for_seconds(0);
    }
    // Disarm assembly recovery; trace-quiet watchdog owns recovery here.
    unsafe {
        asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nomem, nostack));
    }
    // Failure-stage readout: the parks survive now that the core domain is
    // powered, so park stage*15 s and let the Android return time publish
    // which handoff stage failed (1->~35 s, 4->~80 s, 7->~125 s).
    if !gadget_ready {
        let stage = usb::gadget_handoff_failure_stage().clamp(1, 12) as u64;
        usb::park_for_seconds(stage * 15);
    }
    // Re-run the GIC sweep: this polling branch bypasses the success-path sweep, and stray ABL IRQs
    // would reboot mid-observation.
    let _ = platform::gicv3::init(
        platform::bramble::GICD_BASE,
        platform::bramble::GICR_BASE,
        None,
    );
    // Re-issue the Linux soft_connect tail after a failed handoff. Success enumerates; failure schedules
    // a status-coded APSS bite: 1=+2s, 4=+6s, 5/6=+10s, 0/8=Run/Stop reached with a host-visible blip if
    // available. T+37-39/-110 can also mean a secure/unwritable WDT.
    // A SUCCESSFUL handoff must NOT be rescued: re-running the tail on a
    // live gadget wedges endpoint commands (CMDACT races on re-Run/Stop)
    // and the un-petted rescue time crosses the ~17 s unknown watchdog,
    // which resets before the gates can evaluate. Gate runs on a live
    // gadget only need the observation loop.
    let arm_status = if gadget_ready {
        0
    } else {
        usb::u0_arm_recovery()
    };
    match arm_status {
        1 => usb::u0_arm_wdt_bite(2),
        4 => usb::u0_arm_wdt_bite(6),
        5 | 6 => usb::u0_arm_wdt_bite(10),
        // A 0/8 blip proves a running core and U0; its presence/absence distinguishes dead EP0 from failed HS
        // training.
        0 | 8 => usb::u0_arm_set_blips(1),
        _ => {}
    }
    // Clear u0 blips for diag readouts so they own every SDIS pair.
    if cmd_gate_is("diag")
        || cmd_gate_is("lnk3")
        || cmd_gate_is("sof")
    // forcehs is an enumeration attempt; do not reset it with a blip.
        || cmd_gate_is("forcehs")
    {
        usb::u0_arm_set_blips(0);
    }
    // Unconditional bite tests APSS-WDT writability.
    if env_flag(option_env!("FULLERENE_USB_WDT_BITE_CONTROL")) {
        usb::u0_arm_wdt_bite(3);
    }
    // u0_armed keeps the pet+poll survival path alive for host enumeration.
    let u0_armed = matches!(arm_status, 0 | 8);
    usb::trace_marker(
        usb::TRACE_PROBE_WATCHDOG,
        0x5349_4700 | (signal_smmu_code & 0xff),
    );
    if env_flag(option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP")) {
        // Early drop is owned by handoff; keep pull-up down and reset.
        usb::ep0_signal_drop_pullup();
        trace_gate(TRACE_WDT);
        reset_after_probe_failure();
    }
    if option_env!("FULLERENE_USB_SIGNAL_DIAG_PUBLISH") == Some("1") {
        // Publish the pull-up when handoff failed before Run/Stop so gates remain readable.
        usb::ep0_signal_publish_pullup();
    }
    if cmd_gate_is("always") {
        // Immediate TRUE publish before the observation window: the ~17 s
        // unidentified biter overrides every park, so a post-observe eval can
        // lose the race and masquerade as "probe not reached". The self-test
        // only needs the host-visible stop, and earlier is strictly safer.
        trace_gate(0x4741_5445 | 1);
        let _ = usb::gate_true_stop_device();
        usb::park_for_seconds(30);
        usb::park_for_seconds(0);
    }
    let include_raw_link = env_flag(option_env!("FULLERENE_USB_SIGNAL_RAW_LINK"));
    let frequency = probe_counter_frequency();
    let timeout_secs = env_seconds(option_env!("FULLERENE_USB_PROBE_TIMEOUT_SECS"), 120);
    let mut deadline = probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
    let mut last_head = usb::trace_head();
    // Shorten gate windows to beat the unknown ~17s watchdog.
    let observe_secs = env_seconds(option_env!("FULLERENE_USB_PROBE_OBSERVE_SECS"), 10);
    let observe_until = probe_counter().saturating_add(frequency.saturating_mul(observe_secs));
    let mut signal_code = signal_smmu_code;
    // Keep gate runs in the full observation window; arm progress on -110 is still diagnostic.
    let gate_active = env_flag(option_env!("FULLERENE_USB_SIGNAL_CMD_GATE"));
    // Keepalive: the restored core domain is collapsed by RPMh ~5-8 s after
    // the attach wakes it even though the entry-time vote was accepted, and
    // apply_usb_power's state flag makes the park keepalive a no-op. Re-arm
    // the CX corner, the interconnect vote, the rails, and the GDSC on a
    // 0.5 s cadence so the SETUP window and the parks that follow it run
    // inside a live core.
    let mut next_keepalive = probe_counter().saturating_add(frequency / 2);
    // lnk-nib was sampled at entry, before the re-reset.
    loop {
        usb::wdt_pet();
        usb::poll();
        if probe_counter() >= next_keepalive {
            let _ = unsafe {
                platform::bramble::refresh_usb_domain_votes(
                    platform::bramble::UsbBusVote::Nominal,
                    true,
                )
            };
            let _ = unsafe { platform::bramble::force_enable_usb30_gdsc() };
            next_keepalive = probe_counter().saturating_add(frequency / 2);
        }
        if (usb::probe_ep0_progress() || u0_armed) && !gate_active {
            // Enumeration succeeded: stop signaling and enter the stable poll loop.
            stable_ep0_park();
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
            // Harvest newest STARTTRANSFER when no live signal exists: 13=timeout wedge, status-nibble+1=clean.
            // The timeout flag lives in bit 31; bit 16 is a healthy
            // XferRscIdx=1 completion on physical EP1, not a wedge.
            let harvest = usb::harvest_last_str_code();
            if harvest != 0xFFFF_FFFF {
                signal_code = if harvest & 0x8000_0000 != 0 {
                    13
                } else {
                    (harvest & 0xf) + 1
                };
            }
        }
        // Gate runs observe the whole window; only non-gate runs short-circuit on the first signal.
        if (!gate_active && signal_code != 0) || probe_counter() >= observe_until {
            break;
        }
        let head = usb::trace_head();
        if head != last_head {
            last_head = head;
            deadline = probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
        } else if frequency != 0 && probe_counter() >= deadline {
            // A gate run must reach its gate evaluation even when the trace
            // goes quiet (the read/64 failure stops all trace traffic). A
            // quiet-trace reset here would land every gate in the ~37 s
            // early-reset bucket and make the 60/90 s park buckets
            // unobservable; fall through to the gates instead.
            if gate_active {
                trace_gate(0x5155_4945); // "QUIE"
                break;
            }
            trace_gate(TRACE_WDT);
            reset_after_probe_failure();
        }
    }
    if signal_code != 0 {
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x5349_4744 | (signal_code & 0xff),
        );
    }
    // gdbstop: delay (state+1)*250ms, then stop the core; attach-disconnect delta names GDBGLTSSM.
    if cmd_gate_is("gdbstop") {
        let state = usb::gdb_ltssm_link_state();
        trace_gate(0x4744_4253 | (state & 0x1f));
        let target = usb::arch_counter_ticks().saturating_add(
            probe_counter_frequency().saturating_mul(state.saturating_add(1) as u64) / 4,
        );
        wait_arch_ticks(target);
        let _ = usb::gate_true_stop_device();
        park_without_recovery_timer();
    }
    // u0stat: encode u0_arm status as QSCRATCH drop/restore cycles before the generic gate.
    if cmd_gate_is("u0stat") {
        let status = usb::u0_arm_status_probe();
        let cycles = if status == 0 {
            1
        } else if matches!(status, 1 | 4 | 5 | 6 | 8) {
            status
        } else {
            16
        };
        trace_gate(0x5530_5354 | (status & 0xff));
        for _ in 0..cycles {
            usb::ep0_signal_drop_pullup();
            wait_arch_ticks(usb::window_deadline_ticks(1));
            usb::ep0_signal_restore_pullup();
            wait_arch_ticks(usb::window_deadline_ticks(1));
        }
        usb::park_for_seconds(0);
    }
    // armstat: 0=command retired (+16s bite), 8=command wedge (+1s); return time is the readout.
    if cmd_gate_is("armstat") {
        let status = usb::u0_arm_status_probe();
        trace_gate(0x4152_4D53 | (status & 0xff));
        let delay = if status == 0 { 16 } else { 1 };
        usb::u0_arm_wdt_bite(delay);
        park_without_recovery_timer();
    }
    // armalive: a retired L1 leaves pending TRB or DMA'd SETUP; bite early and park before the generic
    // gate.
    if cmd_gate_is("armalive") {
        let state = usb::armalive_probe();
        trace_gate(0x414C_4C56 | (state & 0xff));
        if state != 0 {
            // Bite +1s after window end to stay clear of the secure-WDT bucket.
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // ep1status publishes the retained EP1 STARTTRANSFER status nibble through
    // the existing APSS-WDT timing readout. This is a readout only: it does
    // not reissue or reconfigure the EP1 command, so it cannot turn a
    // diagnostic result into enumeration. A secure-owned WDT falls back to
    // the normal secure-watchdog return bucket, which is itself the negative
    // result for this channel.
    if cmd_gate_is("ep1status") {
        let raw = usb::ep1_start_status_probe();
        let delay = if raw == 0xFFFF_FFFF {
            6
        } else {
            (((raw >> 12) & 0xf) + 1).min(6)
        };
        trace_gate(0x4550_3153 | ((raw & 0xf000) >> 4)); // "EP1S" + status nibble
        usb::u0_arm_wdt_bite(delay);
        park_without_recovery_timer();
    }
    // lnkalive: states {8,9,11,14,15} mean reset/resume is stuck; states {4,5,6,7,10} mean the QSCRATCH
    // link-down phantom. Bite early for the former.
    if cmd_gate_is("lnkalive") {
        let lnkst = usb::dsts_raw_link_state();
        trace_gate(0x4C4E_4B53 | (lnkst & 0xff));
        if lnkst == 8 || lnkst == 9 || lnkst == 11 || lnkst == 14 || lnkst == 15 {
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // lnk3: stop + bite when a mid-transaction state was seen, separating UTMI-RX-dead from reset-handshake
    // cases.
    if cmd_gate_is("lnk3") {
        let saw_mid = usb::lnk_mid_transaction_seen();
        trace_gate(0x4C4E_4B33 | (saw_mid as u32 & 0xff));
        if saw_mid {
            let _ = usb::gate_true_stop_device();
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // sof: bit A is a stale halted/Run-Stop readback (raw 16/17); bit B is SOF change in 100ms. Bite early
    // for stale readback and stop when SOF was seen.
    if cmd_gate_is("sof") {
        let raw = usb::ep0_raw_link_nibble();
        let frequency = probe_counter_frequency();
        let subwindow = probe_counter();
        let sof_first = usb::dsts_sof_frame_number();
        while frequency != 0 && probe_counter().wrapping_sub(subwindow) < frequency / 10 {
            usb::poll();
        }
        let saw_sof = usb::dsts_sof_frame_number() != sof_first;
        trace_gate(0x534F_4600 | (((raw & 0x1f) << 1) | (saw_sof as u32 & 1)));
        if saw_sof {
            let _ = usb::gate_true_stop_device();
        }
        if raw == 16 || raw == 17 {
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // lnk57: state 7 = polling/TX alive, state 5 = RX_DETECT/no training; other values remain the else
    // bucket.
    if cmd_gate_is("lnk57") {
        let state = usb::dsts_raw_link_state();
        trace_gate(0x4C4E_3537 | (state & 0x1f));
        if state == 5 {
            let _ = usb::gate_true_stop_device();
        }
        if state == 7 {
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // Per-state bisection: a gate name selects the expected DSTS state; a
    // match stops the core while the host tracks it, publishing a line.
    let state_splits = [
        ("lnk4", 0x4C4E_5F34, 4),
        ("lnk6", 0x4C4E_5F36, 6),
        ("lnk10", 0x4C4E_5FA0, 10),
        ("lnk12", 0x4C4E_5FC0, 12),
        ("lnk13", 0x4C4E_5FD0, 13),
    ];
    if let Some((_, code, expected)) = state_splits
        .into_iter()
        .find(|(name, _, _)| cmd_gate_is(name))
    {
        let state = usb::dsts_raw_link_state();
        trace_gate(code | (state & 0x1f));
        if state == expected {
            let _ = usb::gate_true_stop_device();
        }
        park_without_recovery_timer();
    }
    // lnkraw: publish the exact DSTS state through a host-visible stop delay.
    // The APSS watchdog return bucket is not readable on every Bramble boot,
    // while DCTL Run/Stop is the one proven host-visible disconnect primitive.
    if cmd_gate_is("lnkraw") {
        let state = usb::dsts_raw_link_state();
        trace_gate(0x4C4E_5257 | (state & 0x1f));
        // 250 ms per raw state value keeps all valid DWC3 states inside the
        // host's first descriptor window. The disconnect timestamp relative
        // to HS attach identifies the captured nibble without depending on
        // secure watchdog ownership.
        let delay_ticks = probe_counter_frequency()
            .saturating_mul(u64::from(state.saturating_add(1)))
            / 4;
        wait_arch_ticks(usb::arch_counter_ticks().saturating_add(delay_ticks));
        let _ = usb::gate_true_stop_device();
        park_without_recovery_timer();
    }
    // lnkstate: publish/blip any state outside the tested set and encode the exact state in the bite delay.
    if cmd_gate_is("lnkstate") {
        disarm_recovery_timer();
        let deadline = usb::window_deadline_ticks(1);
        let mut latched: u32 = 0xffff;
        while usb::arch_counter_ticks() < deadline {
            let state = usb::dsts_raw_link_state();
            if state != latched {
                latched = state;
                usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x4C4E_5354 | state); // "LN_ST"
            }
            if !matches!(state, 0 | 1 | 2 | 3 | 8 | 9 | 11 | 14 | 15) {
                trace_gate(0x4C4E_5355 | state);
                usb::u0_arm_set_blips(1);
                usb::u0_arm_wdt_bite(state.saturating_add(1));
                stable_park();
            }
            usb::wdt_pet();
            usb::poll();
        }
        // Fixed 16s bite names the last tested state.
        trace_gate(0x4C4E_534E | (latched & 0x1f));
        usb::u0_arm_wdt_bite(16);
        stable_park();
    }
    // gdb: bite at state+1 to time-encode the raw GDBGLTSSM LINKSTATE nibble.
    if cmd_gate_is("gdb") || cmd_gate_is("gdbforce") {
        let state = usb::gdb_ltssm_link_state();
        trace_gate(0x4744_4253 | (state & 0x1f));
        usb::u0_arm_wdt_bite(state.saturating_add(1));
        park_without_recovery_timer();
    }
    // voteflip: disconnect/re-attach proves glue votes own USB2 pull-up; silence proves they are inert.
    if cmd_gate_is("voteflip") {
        trace_gate(0x564F_5446);
        usb::flip_utmi_pipe_clock();
        wait_arch_ticks(usb::window_deadline_ticks(1));
        trace_gate(0x564F_5452);
        usb::restore_usb2_session_votes();
        stable_park();
    }
    // voteflip2 uses raw QSCRATCH_GENERAL_CFG writes; same disconnect-line readout as voteflip.
    if cmd_gate_is("voteflip2") {
        trace_gate(0x564F_5447);
        usb::flip_utmi_pipe_clock_raw();
        trace_gate(0x564F_5453);
        usb::restore_usb2_session_votes();
        stable_park();
    }
    // haltbit: early return means DSTS.DEVCTRLHLT is set despite the host-visible attach.
    if cmd_gate_is("haltbit") {
        let halted = usb::dsts_device_ctrl_halted();
        trace_gate(0x4841_4C54 | (halted as u32));
        if halted {
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // dctlbit: early return means Run/Stop is clear; a set bit contradicts the state field.
    if cmd_gate_is("dctlbit") {
        let running = usb::dctl_run_stop_set();
        trace_gate(0x4443_544C | (running as u32));
        if !running {
            usb::u0_arm_wdt_bite(1);
        }
        park_without_recovery_timer();
    }
    // lnkrawdb: time-encode the raw DSTS word and its link nibble without re-reading.
    if cmd_gate_is("lnkrawdb") {
        let dsts = usb::dsts_word_snapshot();
        let nibble = (dsts >> 16) & 0xf;
        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x4C52_4144 | (dsts & 0xffff));
        usb::u0_arm_wdt_bite(nibble.saturating_add(1));
        park_without_recovery_timer();
    }
    // lnkmask: sample DSTS states for 1s, then encode the low/high mask nibble in two companion runs.
    if cmd_gate_is("lnkmasklo") || cmd_gate_is("lnkmaskhi") {
        disarm_recovery_timer();
        let deadline = usb::window_deadline_ticks(1);
        let mut mask: u32 = 0;
        while usb::arch_counter_ticks() < deadline {
            let state = usb::dsts_raw_link_state();
            mask |= 1 << (state & 0xf);
            usb::wdt_pet();
            usb::poll();
        }
        let nibble = if cmd_gate_is("lnkmaskhi") {
            (mask >> 4) & 0xf
        } else {
            mask & 0xf
        };
        trace_gate(0x4C4D_534B | (mask & 0xffff));
        usb::u0_arm_wdt_bite(nibble.saturating_add(1));
        loop {
            usb::wdt_pet();
            usb::poll();
        }
    }
    // rescue2 forces the full endpoint tail after soft reset; journal enumeration is the readout. Run it
    // before the generic gate.
    if cmd_gate_is("rescue2") {
        usb::u0_arm_set_blips(0);
        let status = usb::u0_arm_window_recovery();
        trace_gate(0x5232_0000 | (status & 0xff));
        park_without_recovery_timer();
    }
    // diag rescues the stuck read/64 stage; journal enumeration, not a park, is the readout.
    if cmd_gate_is("diag") {
        // The full mid-window rescue: device soft reset + complete endpoint
        // re-arm clears a stale DSTS.DEVCTRLHLT (which rescue_read64 bails
        // on) while the host's descriptor URB is still retrying its SETUP
        // token inside the 5 s timeout. Then keep servicing the re-armed
        // endpoint for the rest of the URB window so the retries land.
        let rescue = usb::u0_arm_window_recovery();
        usb::trace_marker(usb::TRACE_PROBE_WATCHDOG, 0x5245_5343 | rescue); // "RESC"
        let frequency = probe_counter_frequency();
        let service_until = probe_counter().saturating_add(frequency.saturating_mul(4));
        while probe_counter() < service_until {
            usb::wdt_pet();
            usb::poll();
        }
        park_without_recovery_timer();
    }
    // Generic gate reads THIS run's trace. TRUE stops the core while the host tracks the device (journal
    // disconnect line); FALSE drops the pull-up and parks without one. Both return in the WDT bucket.
    // TRUE additionally parks 30 s so the Android-return time separates a
    // TRUE readout (~65-75 s) from an early probe reset (~40 s) even when
    // the device never attached and no disconnect line can be published.
    if cmd_gate_is("pub") {
        // Composite enumeration-progress readout: the parks survive now that
        // the core domain is powered, so park 10 s + 12 s per diag code and
        // let the Android return time publish the code (1 = no SETUP ever
        // reached EP0 ... 6 = the read/64 data TRB was fetched). Stop the
        // core first: a live gadget attached to the host keeps the domain's
        // collapse timer armed, and the park would be cut ~7 s into the
        // wait (the always-TRUE stop-then-park flow is the proven-surviving
        // sequence).
        let code = usb::diag_readout_code().clamp(1, 6) as u64;
        let _ = usb::gate_true_stop_device();
        usb::park_for_seconds(10 + code * 12);
    }
    // pubd publishes the composite diag code as host-visible attach lines.
    // The post-attach collapse resets the handset ~5.5-8 s after the attach,
    // which cuts every park, but each pull-up drop/restore cycle that lands
    // before the reset re-attaches and the host prints one "new high-speed
    // USB device" line per connect. A/B result: the QSCRATCH, DCTL, and
    // VBUSVLDEXT0 drop primitives are all electrically inert on this
    // revision (no host-visible disconnect ever appears), so the cycle
    // count cannot be read. The gate stays as a record of that negative
    // result; the park-based `pub` readout plus the rail refresh keepalive
    // is the surviving diag channel.
    if cmd_gate_is("pubd") {
        let frequency = probe_counter_frequency();
        let sample_until = probe_counter().saturating_add(frequency / 2);
        poll_until_probe_ticks(frequency, sample_until);
        let code = usb::diag_readout_code().clamp(1, 6);
        trace_gate(0x5055_4244 | (code & 0xff)); // "PUBD" + code
        park_without_recovery_timer();
    }
    // spin: park in a PURE spin loop (wdt pet only, no usb::poll, no MMIO)
    // for 30 s, then PSCI-reset. This isolates the ~5.6 s post-attach reset:
    // a full 30 s survival (return ~55 s) proves the reset needs our MMIO
    // traffic into a collapsing domain (NOC error), while the usual ~42 s
    // return proves the collapse resets the handset by itself.
    if cmd_gate_is("spin") {
        let deadline = probe_counter().saturating_add(frequency.saturating_mul(30));
        while probe_counter() < deadline {
            usb::wdt_pet();
            core::hint::spin_loop();
        }
        usb::park_for_seconds(0);
    }
    // dstat publishes the composite diag code as host-visible attach lines.
    // DCTL Run/Stop is the one disconnect primitive the host actually sees
    // (Run/Stop owns the physical pull-up; the QSCRATCH/VBUS bits proved
    // inert). At eval the diag code names how far the first enumeration
    // window got (1 = no SETUP reached DRAM ... 6 = XferNotReady on the
    // data phase); `code` short stop/run cycles then re-attach that many
    // times, and every re-attach both prints one "new high-speed USB
    // device" line and starts a fresh enumeration attempt with EP0
    // re-initialized by the bus reset. The cycles all land before the
    // ~5.6 s post-attach reset, so the attach-line count minus the first
    // attach IS the code.
    if cmd_gate_is("dstat") {
        let code = usb::diag_readout_code().clamp(1, 6) as u64;
        trace_gate(0x4453_5441 | ((code & 0xff) as u32)); // "DSTA" + code
        for _ in 0..code {
            let _ = usb::gate_true_stop_device();
            let dropped = probe_counter().saturating_add(frequency / 4);
            poll_until_probe_ticks(frequency, dropped);
            let _ = usb::gate_true_run_device();
            let attached = probe_counter().saturating_add(frequency * 3 / 10);
            poll_until_probe_ticks(frequency, attached);
        }
        park_without_recovery_timer();
    }
    if let Some(met) = usb::cmd_gate_condition_met() {
        trace_gate(0x4741_5445 | (met as u32 & 0xff));
        if met {
            // Run/Stop owns the physical pull-up, so stopping at eval with --connect-delay 0 publishes the TRUE
            // line. The old reset/SDIS timing splits are dead; journal presence is the one-bit readout.
            let _ = usb::gate_true_stop_device();
            usb::park_for_seconds(30);
            usb::park_for_seconds(0);
        }
        // FALSE: the QSCRATCH/DCTL/VBUS drop primitives are all electrically
        // inert on this revision, and the ~5.5 s post-attach reset cuts both
        // the TRUE (30 s) and FALSE (90 s) parks into the same ~42 s Android
        // return, which made every one-bit gate unreadable. Schedule an
        // explicit +1 s APSS bite instead: a FALSE readout returns at
        // eval+1 s + Android boot (~24 s from fastboot boot), clearly
        // separated from the TRUE/death bucket (~42 s).
        usb::u0_arm_wdt_bite(1);
        usb::park_for_seconds(90);
    }
    // code+1 attach cycles encode the diagnostic value; QSCRATCH overrides stay short.
    let cycles = (signal_code as u64 + 1).min(16);
    for _ in 0..cycles {
        usb::ep0_signal_drop_pullup();
        let dropped = probe_counter().saturating_add(frequency.saturating_mul(3) / 2);
        poll_until_probe_ticks(frequency, dropped);
        usb::ep0_signal_restore_pullup();
        let attached = probe_counter().saturating_add(frequency.saturating_mul(3) / 2);
        poll_until_probe_ticks(frequency, attached);
    }
    // detach: after the post-arm sample, drop the pull-up for good (the
    // QSCRATCH session overrides here plus the PHY's VBUSVLDEXT0 via the
    // --signal-drop-vbusvld env; the PHY latch owns the pull-up past DCTL
    // and the QSCRATCH bits) and then publish the diag code as code*1.5 s
    // before the reset. The host aborts the enumeration on the disconnect
    // and stops issuing the port resets whose bus reset kills the session
    // at attach+5.5 s, so the whole ladder escapes the death window.
    if cmd_gate_is("detach") {
        let code = usb::diag_readout_code().clamp(1, 10);
        trace_gate(0x4454_4143 | (code & 0xff)); // "DTAC" + code
        usb::ep0_signal_drop_pullup();
        let wait_until =
            probe_counter().saturating_add(frequency.saturating_mul(u64::from(code)) * 3 / 2);
        while probe_counter() < wait_until {
            usb::wdt_pet();
        }
        trace_gate(TRACE_WDT);
        reset_after_probe_failure();
    }
    // sdis: stop the core (the host-visible disconnect the always gate uses)
    // right after the post-arm sample, then publish the diag code as
    // code*1.5 s of petted wait before the reset. The host aborts the
    // in-flight control transfer on the disconnect (-ENODEV) and never
    // issues the port reset whose bus reset kills the session at
    // attach+5.5 s, so the whole 1..9 ladder escapes the death window and
    // lands 1.5 s apart - far beyond the +-1 s Android boot jitter.
    if cmd_gate_is("sdis") {
        let code = usb::diag_readout_code().clamp(1, 10);
        trace_gate(0x5344_4953 | (code & 0xff)); // "SDIS" + code
        let _ = usb::gate_true_stop_device();
        let wait_until =
            probe_counter().saturating_add(frequency.saturating_mul(u64::from(code)) * 3 / 2);
        while probe_counter() < wait_until {
            usb::wdt_pet();
            usb::poll();
        }
        trace_gate(TRACE_WDT);
        reset_after_probe_failure();
    }
    // The tail reset is the one reliable PSCI readout left (the APSS bite is
    // not writable from EL1 and every park is this reset's victim). Two
    // stages:
    //   1. A 0.4 s poll-serviced settle: the eval fires on the FIRST event
    //      (the read/64 SETUP), so the data-phase arm and - in a working
    //      system - its transfer completion both land inside the settle. The
    //      EP1 state sampled afterwards is a post-mortem, not a race.
    //   2. Publish the composite diag code as 450 ms of poll-serviced wait
    //      per code before the reset, so the reset time (the Android return
    //      time) names the code 1..9. The 450 ms step keeps code 9's reset
    //      (eval + 0.4 + 9*0.45 = +4.45 s) inside the ~5 s post-attach death
    //      window that cuts every park and the stable-park path.
    // The old stable-park short-circuit is folded into the sample: the data
    // phase in the current regime always arms before the eval, so
    // probe_ep0_progress() was true in every tail iteration and the reset -
    // the only working readout - never fired. Park here only when the data
    // phase completed with success and the host actually holds the bytes.
    let settle_until = probe_counter().saturating_add(frequency * 2 / 5);
    while probe_counter() < settle_until {
        usb::wdt_pet();
        usb::poll();
    }
    if usb::ep1_data_phase_complete() {
        stable_ep0_park();
    }
    let code = usb::diag_readout_code().clamp(1, 10) as u64;
    trace_gate(0x5245_4144 | ((code & 0xff) as u32)); // "MRAD" + code
    let wait_until = probe_counter().saturating_add(frequency.saturating_mul(code) * 9 / 20);
    while probe_counter() < wait_until {
        usb::wdt_pet();
        usb::poll();
    }
    trace_gate(TRACE_WDT);
    reset_after_probe_failure();
}

/// Fire the bare pull-up before relocator/prelude; the measured attach isolates ABL/XBL-to-entry latency
/// plus bare-sequence cost. Touches only const MMIO, zeroed BSS, and the arch counter, so it is safe
/// before relocations. The armed secure WDT provides self-recovery.
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
    // Disable the secure WDT first; XBL/ABL leaves it biting ~17s and killing enumeration.
    // FULLERENE_USB_SWDD_SKIP=1 isolates the SMC cost and lets it remain armed.
    if option_env!("FULLERENE_USB_SWDD_SKIP").is_none() {
        usb::secure_wdt_disable();
    }
    // Only SMC gate runs need the multi-SMC diagnostics; those don’t need attach. Other gates must keep
    // the single attaching SMC.
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
    // stall-map arms timer at entry+15s, after link ON. The handler SDIS blip proves PPI/handler; ~36s
    // means SMC reset works, ~38s means SMC is dead or PS_HOLD stalled.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("stall-map") {
        timer::arm_ms(15_000);
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    // Read the previous boot's enumeration progress while its records are
    // still intact (before the cursor reset below): the retained .usb_trace
    // region is NOLOAD and survives the warm reset, so this code publishes
    // how far the previous boot got. It rides the attach-delay channel with
    // the PON code (+1 s per step) and is readable in the host journal's
    // attach timestamp.
    let prev_boot_code = usb::prev_boot_progress_code();
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    usb::trace_probe_begin();
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    // Reset trace cursor once per boot; Android may scribble retained warm-reset headers.
    usb::trace_reset_head_for_boot();
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    // Previous-boot trace gate: 1 = attach only when the previous boot's
    // trace reached a SETUP (code >= 2), 2 = attach only when it did not,
    // 3 = attach only when the previous trace was verifiable but held no
    // SETUP (code == 1) - which separates a surviving trace from a lost or
    // scribbled one. The suppressed path resets before any pull-up publish,
    // so the host journal's attach-line presence is the one-bit readout -
    // immune to the bootloader jitter that swamps the attach-delay ladder.
    if let Some(mode) = option_env!("FULLERENE_USB_PREV_TRACE_GATE") {
        let attach_wanted = match mode {
            "1" => prev_boot_code >= 2,
            "2" => prev_boot_code < 2,
            "3" => prev_boot_code == 1,
            _ => true,
        };
        usb::trace_marker(
            usb::TRACE_PROBE_WATCHDOG,
            0x5056_5447 | (prev_boot_code & 0xff), // "PVTG" + previous code
        );
        if !attach_wanted {
            reset_after_probe_failure();
        }
    }
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    // Reset trace cursor once per boot; Android may scribble retained warm-reset headers.
    usb::trace_reset_head_for_boot();

    // Bare probe stays below the DMA/trace boundary; invalid retained DRAM must not mask controller-only
    // results.
    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    {
        usb::clear_dma_memory();
        usb::trace_marker(usb::TRACE_BOOT_USB_ENTRY, 0);
    }
    // Normal entry prepares Type-C role; gadget probe skips SPMI to separate PMIC from controller faults.
    #[cfg(all(
        fullerene_aarch64_bramble,
        not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_bare_pullup_probe
        ))
    ))]
    {
        usb::trace_marker(usb::TRACE_TYPEC_BEGIN, 0);
        let _typec_state = unsafe { platform::bramble::prepare_usb_device_role() };
        if let Some(typec) = _typec_state {
            usb::set_typec_orientation(typec.orientation_reverse);
        }
        usb::trace_marker(usb::TRACE_TYPEC_DONE, 0);
    }

    #[cfg(not(any(
        fullerene_aarch64_usb_bare_pullup_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    )))]
    {
        uart::init_qcom_geni(0x0098_8000);
        uart::puts("fullerene usb probe: entry\n");
    }
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
        // Use the non-destructive Type-C observer. USB2 does not need orientation; the SPMI skip is an
        // A/B test for the ~11s pre-attach scan delay.
        if option_env!("FULLERENE_USB_SKIP_TYPEC_SPMI") != Some("1") {
            let _ = usb::observe_typec_handoff();
        }
    }
    // Read the previous PON reset reason before USB activity can trigger another
    // recovery. Delay the physical attach by (code + 1) * 300 ms so the host
    // timestamp publishes both a successful read and its reason bucket. The
    // previous boot's retained-trace progress code rides the same channel at
    // +4 s per step - wide enough to survive the +-1-2 s bootloader attach
    // jitter that swamps 1 s steps.
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    let pon_delay_ms = {
        // FULLERENE_USB_PON_READOUT selects what rides the attach-delay
        // channel as (steps) * 300 ms:
        //   seq (default)      the previous reset-reason bucket, +1 step
        //   imem               the IMEM restart-reason cookie: 1 step for
        //                      the recovery chain's 0x77665500, 2 steps for
        //                      a zero cookie, else 2 + low 5 bits
        //   forceN             N steps, a fixed attach shift for the
        //                      entry-vs-attach reference experiments
        //   s1|s2|ctl|wd2|     one raw PON register byte: (byte + 1) steps
        //   warm|soft          capped at 32 steps (9.6 s)
        // A failed read stays at 0 ms.
        let pon_ms = match option_env!("FULLERENE_USB_PON_READOUT") {
            None | Some("seq") => {
                match unsafe { platform::bramble::read_pm8150_pon_reset_code() } {
                    Some(code) => (u64::from(code) + 1) * 300,
                    None => 0,
                }
            }
            Some("imem") => {
                let cookie = platform::bramble::read_imem_restart_reason();
                let steps = if cookie == 0x7766_5500 {
                    1
                } else if cookie == 0 {
                    2
                } else {
                    2 + u64::from(cookie & 0x1f)
                };
                steps * 300
            }
            Some(source) if source.starts_with("force") => source["force".len()..]
                .parse::<u64>()
                .map_or(0, |steps| steps * 300),
            Some(register) => {
                let register: u16 = match register {
                    "s1" => platform::bramble::PON_WD_S1_TIMER,
                    "s2" => platform::bramble::PON_WD_S2_TIMER,
                    "ctl" => platform::bramble::PON_WD_S2_CTL,
                    "warm" => platform::bramble::PON_WARM_RESET_REASON1,
                    "soft" => platform::bramble::PON_SOFT_RESET_REASON1,
                    _ => platform::bramble::PON_WD_S2_CTL2,
                };
                match unsafe { platform::bramble::read_pm8150_pon_register(register) } {
                    Some(value) => (u64::from(value.min(31)) + 1) * 300,
                    None => 0,
                }
            }
        };
        pon_ms + u64::from(prev_boot_code) * 4000
    };
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    let pon_delay_ms = 0;
    // Read SMMU before pull-up publish so secure/clock-gated aperture gives a distinct pre-attach outcome.
    let _signal_smmu_code = if cfg!(fullerene_aarch64_usb_ep0_signal_probe)
        && option_env!("FULLERENE_USB_SIGNAL_SMMU_STATE")
            .filter(|value| *value != "0")
            .is_some()
    {
        usb::probe_smmu_stream_state()
    } else {
        0
    };
    let _signal_link_state = cfg!(fullerene_aarch64_usb_ep0_signal_probe)
        && option_env!("FULLERENE_USB_SIGNAL_LINK_STATE")
            .filter(|value| *value != "0")
            .is_some();
    // Gate runs must survive the ~17 s unidentified biter (bootreason=watchdog)
    // that resets the handset while the handoff is still inside its bounded
    // waits. The signal-probe prologue (CNTP disarm + GIC sweep) provably
    // neutralizes it - the failed-handoff is-runs park 90 s past the bite -
    // but that prologue only runs AFTER the handoff returns, which loses the
    // race on a successful handoff. Move both to BEFORE the handoff for gate
    // runs: recovery then belongs to the probe-internal head-stall and park
    // paths, and the handoff can take its bounded time safely.
    #[cfg(all(
        fullerene_aarch64_usb_ep0_signal_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    ))]
    {
        // The fastboot handoff leaves the USB30 core domain collapsed: the
        // physical attach is carried by the QSCRATCH/PHY session alone while
        // every DWC3 core register reads dead (the long-standing "endpoint
        // command wedge" and the ~17 s post-attach reset are both symptoms).
        // Restore the rails, GDSC, clock sources/branches and reset lines
        // BEFORE the handoff so the core answers MMIO, the endpoint commands
        // retire, and the first host SETUP can be armed and served.
        unsafe {
            // Vote the FULL rail set (super_speed=true includes the QMP
            // core rail pm8150_l9): the USB30 GDSC's parent supply is not
            // otherwise held, and RPMh collapses it ~7 s after the attach
            // wakes the domain, killing the core mid-park. The QMP PHY
            // itself stays unused; only the rail keeps the GDSC powered.
            let _ = platform::bramble::apply_usb_power(true, true);
            let _ = platform::bramble::force_enable_usb30_gdsc();
            let _ = platform::bramble::usb_clock::configure_usb_clocks(
                platform::bramble::UsbBusVote::Nominal,
            );
            let _ = platform::bramble::enable_usb_clock_branches();
            let _ = platform::bramble::usb_reset::reset_usb_blocks(false);
        }
        // Settle: the one surviving 30 s park (the run that attached at
        // +14 s) had a 4 s quiet gap between this power sequence and the
        // handoff; every immediate handoff since failed DEPSTARTCFG in the
        // reuse path and fell to the flaky fallback. The GDSC ramp, the RCG
        // switch and the post-reset PLL need the quiet window before the
        // first endpoint command. Pure spin, no MMIO.
        let settle_frequency = probe_counter_frequency();
        let settle_until = probe_counter().saturating_add(settle_frequency.saturating_mul(3));
        while probe_counter() < settle_until {
            core::hint::spin_loop();
        }
        let pon_until = settle_until + settle_frequency * pon_delay_ms / 1000;
        while probe_counter() < pon_until {
            core::hint::spin_loop();
        }
    }
    let gadget_ready = if cfg!(any(
        fullerene_aarch64_usb_gadget_handoff_probe,
        fullerene_aarch64_usb_pullup_probe
    )) {
        // Retry only ownership races; gate runs use one attempt so evaluation lands before watchdog bite.
        let attempt_limit = if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1")
            || option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1")
        {
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
                    // Direct path exercises the normal non-destructive handoff with probe watchdog/recovery;
                    // its established fallback runs on failure.
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
    // The signal channel owns only failed post-init timelines; successful enumeration must not be reset
    // by diagnostics.
    #[cfg(all(
        fullerene_aarch64_usb_ep0_signal_probe,
        fullerene_aarch64_usb_gadget_handoff_probe
    ))]
    {
        // Failed handoff or diagnostic gate: observe the bounded window, then continue/park as decided.
        let gate_active = env_flag(option_env!("FULLERENE_USB_SIGNAL_CMD_GATE"));
        // PROBE-CHECK: early park proves cfg block reached; ~37-39 means absent or unreached. stall-map
        // uses ~31s as its init-crossed signature.
        if cmd_gate_is("lnk-nib")
            || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("stall-map")
        {
            usb::park_for_seconds(0);
        }
        if !gadget_ready || gate_active {
            run_ep0_signal_probe(_signal_smmu_code, _signal_link_state, gadget_ready);
        }
    }
    if gadget_ready {
        // lnk-nib success-path readout: settle 1s, bucket raw USBLNKST/halt/runstop, then reset 1s per
        // bucket (code/4 capped at 4). Missing readout returns ~T+38-39.
        if cmd_gate_is("lnk-nib") {
            let frequency = probe_counter_frequency();
            let settle = probe_counter().saturating_add(frequency.saturating_mul(1));
            spin_until_probe_ticks(frequency, settle);
            let code = usb::ep0_raw_link_nibble();
            let bucket = (code / 4).min(4);
            trace_gate(0x4C4E_4942 | (code & 0xff) | (bucket << 16));
            let reset_at = probe_counter()
                .saturating_add(frequency.saturating_mul(1u64.saturating_add(u64::from(bucket))));
            spin_until_probe_ticks(frequency, reset_at);
            usb::park_for_seconds(0);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        {
            // Host-visible link-ON readout for the success path: the signal
            // probe queues its own blip only when the handoff failed, so
            // without this the direct branch could never show the SDIS pair.
            usb::arm_blip_queue();
            if usb::gadget_handoff_stage_probe_enabled() {
                // Stage probes publish EP0-less pull-up; keep attach long enough for xHCI, then recover.
                let frequency = probe_counter_frequency();
                let deadline = probe_counter().saturating_add(frequency.saturating_mul(10));
                spin_until_probe_ticks(frequency, deadline);
                reset_after_probe_failure();
            }
        }
        // Keep recovery timer armed until first EP0 DATA/STATUS so invisible success returns to Fastboot.
        #[cfg(not(any(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_pullup_probe
        )))]
        unsafe {
            asm!("msr CNTP_CTL_EL0, xzr", "isb", options(nostack));
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        {
            // Exercise post-controller GIC/USB-SPI ownership like normal Bramble entry.
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
            // Self-recovery net: secure WDT disable may succeed and strand bare probe, so schedule a 16s
            // APSS bite. wdt_pet cannot cancel a pending bite.
            usb::u0_arm_wdt_bite(16);

            // The bare probe intentionally never reads the event/DMA path;
            // keep only the physical pull-up state alive while testing the
            // controller MMIO sequence itself.
            loop {
                usb::wdt_pet();
                core::hint::spin_loop();
            }
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
            // A quiet successful init is still diagnostic failure; trace activity extends the deadline.
            let frequency = probe_counter_frequency();
            // The IRQ-enabled probe may legitimately go quiet after the
            // initial enumeration. Give that path enough time to separate a
            // real IRQ/controller failure from the diagnostic recovery reset.
            let timeout_secs = env_seconds(option_env!("FULLERENE_USB_PROBE_TIMEOUT_SECS"), 120);
            let mut deadline =
                probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
            let mut last_head = usb::trace_head();
            if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
                // Direct handoff uses polling to serialize USB RESET/first SETUP. Trace activity extends the
                // deadline, but an absolute ceiling guarantees recovery if watchdogs are dead. Enumerating
                // runs enter STAB and never see this bound.
                let loop_start = probe_counter();
                let abs_secs = option_env!("FULLERENE_USB_ABS_RESET_SECS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0);
                let abs_deadline = abs_secs
                    .map(|secs| loop_start.saturating_add(frequency.saturating_mul(secs)))
                    .unwrap_or(u64::MAX);
                deadline = deadline.min(abs_deadline);
                loop {
                    if usb::mmio_quiet_active() {
                        // Zero-MMIO park: no controller/watchdog access; assembly timer owns exit.
                        core::hint::spin_loop();
                        continue;
                    }
                    usb::wdt_pet();
                    usb::poll();
                    if usb::probe_ep0_progress() {
                        stable_ep0_park();
                    }
                    let head = usb::trace_head();
                    if head != last_head {
                        last_head = head;
                        let extended =
                            probe_counter().saturating_add(frequency.saturating_mul(timeout_secs));
                        deadline = extended.min(abs_deadline);
                    } else if frequency != 0 && probe_counter() >= deadline {
                        trace_gate(TRACE_WDT);
                        reset_after_probe_failure();
                    }
                }
            }
            loop {
                // IRQ path drains the event ring; polling here would corrupt EVENT_OFFSET/GEVNTCOUNT order.
                usb::wdt_pet();
                unsafe { asm!("wfe", options(nomem, nostack)) };
                usb::service_deferred_platform();
                if usb::probe_ep0_progress() {
                    disarm_recovery_timer();
                    trace_gate(TRACE_STAB);
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
                    trace_gate(TRACE_WDT);
                    reset_after_probe_failure();
                }
            }
        }
        #[cfg(fullerene_aarch64_usb_pullup_probe)]
        {
            // Polling deadline is a second recovery path; firmware-owned GIC can leave timer PPI masked.
            let frequency = probe_counter_frequency();
            let deadline = probe_counter().saturating_add(frequency.saturating_mul(60));
            loop {
                // Pullup-only never owns the DWC3 event ring; leave stale Fastboot event state untouched.
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
        // Do not publish pull-up after failed init; host descriptor timeout would hide the real boundary.
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
    #[cfg(any(
        fullerene_aarch64_usb_halt_probe,
        fullerene_aarch64_usb_cold_halt_probe
    ))]
    loop {
        // Preserve the selected failed handoff for host-side observation.
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
    // PSCI reset first; PS_HOLD can stall in an unclocked PMIC/SPMI aperture and remains fallback only.
    unsafe {
        core::arch::asm!(
            "mov w0, #9",
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
