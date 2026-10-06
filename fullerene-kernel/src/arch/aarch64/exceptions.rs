use core::arch::{asm, global_asm};

use super::uart;

/// Register state captured by an EL1 exception entry.
///
/// The first 31 words match x0..x30, followed by the EL1 return state, the
/// user stack pointer, and the user TLS register. Keeping this layout
/// explicit is the ABI boundary for SVC, page-fault, and scheduler paths;
/// assembly only moves this frame, while Rust interprets it.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub(crate) struct Aarch64TrapFrame {
    pub(crate) x: [u64; 31],
    pub(crate) elr_el1: u64,
    pub(crate) spsr_el1: u64,
    pub(crate) sp_el0: u64,
    pub(crate) esr_el1: u64,
    pub(crate) far_el1: u64,
    /// Linux AArch64 uses TPIDR_EL0 for the thread pointer.
    pub(crate) tpidr_el0: u64,
}

impl Aarch64TrapFrame {
    pub(crate) const BYTE_SIZE: usize = core::mem::size_of::<Self>();

    const LAYOUT_ASSERTIONS: () = {
        assert!(Self::BYTE_SIZE == 304);
        assert!(core::mem::offset_of!(Self, elr_el1) == 248);
        assert!(core::mem::offset_of!(Self, spsr_el1) == 256);
        assert!(core::mem::offset_of!(Self, sp_el0) == 264);
        assert!(core::mem::offset_of!(Self, esr_el1) == 272);
        assert!(core::mem::offset_of!(Self, far_el1) == 280);
        assert!(core::mem::offset_of!(Self, tpidr_el0) == 288);
    };

    #[inline]
    pub(crate) fn from_user(&self) -> bool {
        // PSTATE.M == EL0t when the exception came from an AArch64 user task.
        self.spsr_el1 & 0xf == 0
    }
}

// AArch64 exception vectors are 16 slots of 128 bytes and the table must be
// 2048-byte aligned. The vector itself branches to a common frame-building
// stub; the Rust side receives the same typed frame for every exception class.
global_asm!(
    ".section .text.exception_vectors,\"ax\"\n\
     .balign 2048\n\
     .global aarch64_exception_vectors\n\
     .type aarch64_exception_vectors, %function\n\
     aarch64_exception_vectors:\n\
     .rept 4\n\
         b aarch64_exception_sync_entry\n\
         .space 124\n\
         b aarch64_exception_irq_entry\n\
         .space 124\n\
         b aarch64_exception_sync_entry\n\
         .space 124\n\
         b aarch64_exception_sync_entry\n\
         .space 124\n\
     .endr\n\
     .size aarch64_exception_vectors, . - aarch64_exception_vectors\n\
\
     .global aarch64_exception_irq_entry\n\
     .type aarch64_exception_irq_entry, %function\n\
     aarch64_exception_irq_entry:\n\
         sub sp, sp, #304\n\
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
         mrs x1, ELR_EL1\n\
         str x1, [sp, #248]\n\
         mrs x1, SPSR_EL1\n\
         str x1, [sp, #256]\n\
         mrs x1, SP_EL0\n\
         str x1, [sp, #264]\n\
         mrs x1, ESR_EL1\n\
         str x1, [sp, #272]\n\
         mrs x1, FAR_EL1\n\
         str x1, [sp, #280]\n\
         mrs x1, TPIDR_EL0\n\
         str x1, [sp, #288]\n\
         mov x0, sp\n\
         bl aarch64_exception_irq\n\
         ldr x1, [sp, #248]\n\
         msr ELR_EL1, x1\n\
         ldr x1, [sp, #256]\n\
         msr SPSR_EL1, x1\n\
         ldr x1, [sp, #264]\n\
         msr SP_EL0, x1\n\
         ldr x1, [sp, #288]\n\
         msr TPIDR_EL0, x1\n\
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
         add sp, sp, #304\n\
         eret\n\
     .size aarch64_exception_irq_entry, . - aarch64_exception_irq_entry\n\
\
     .global aarch64_exception_sync_entry\n\
     .type aarch64_exception_sync_entry, %function\n\
     aarch64_exception_sync_entry:\n\
         sub sp, sp, #304\n\
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
         mrs x1, ELR_EL1\n\
         str x1, [sp, #248]\n\
         mrs x1, SPSR_EL1\n\
         str x1, [sp, #256]\n\
         mrs x1, SP_EL0\n\
         str x1, [sp, #264]\n\
         mrs x1, ESR_EL1\n\
         str x1, [sp, #272]\n\
         mrs x1, FAR_EL1\n\
         str x1, [sp, #280]\n\
         mrs x1, TPIDR_EL0\n\
         str x1, [sp, #288]\n\
         mov x0, sp\n\
         bl aarch64_exception_sync\n\
         ldr x1, [sp, #248]\n\
         msr ELR_EL1, x1\n\
         ldr x1, [sp, #256]\n\
         msr SPSR_EL1, x1\n\
         ldr x1, [sp, #264]\n\
         msr SP_EL0, x1\n\
         ldr x1, [sp, #288]\n\
         msr TPIDR_EL0, x1\n\
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
         add sp, sp, #304\n\
         eret\n\
     .size aarch64_exception_sync_entry, . - aarch64_exception_sync_entry\n"
);

// Load a prepared frame and return to the privilege level encoded in SPSR.
// This is the only assembly needed to start a user task; all register values
// are produced by Rust from Aarch64TrapFrame.
global_asm!(
    ".text\n\
     .global aarch64_enter_user\n\
     .type aarch64_enter_user, %function\n\
     aarch64_enter_user:\n\
         ldr x1, [x0, #248]\n\
         msr ELR_EL1, x1\n\
         ldr x1, [x0, #256]\n\
         msr SPSR_EL1, x1\n\
         ldr x1, [x0, #264]\n\
         msr SP_EL0, x1\n\
         ldr x1, [x0, #288]\n\
         msr TPIDR_EL0, x1\n\
         ldp x1, x2, [x0, #8]\n\
         ldp x3, x4, [x0, #24]\n\
         ldp x5, x6, [x0, #40]\n\
         ldp x7, x8, [x0, #56]\n\
         ldp x9, x10, [x0, #72]\n\
         ldp x11, x12, [x0, #88]\n\
         ldp x13, x14, [x0, #104]\n\
         ldp x15, x16, [x0, #120]\n\
         ldp x17, x18, [x0, #136]\n\
         ldp x19, x20, [x0, #152]\n\
         ldp x21, x22, [x0, #168]\n\
         ldp x23, x24, [x0, #184]\n\
         ldp x25, x26, [x0, #200]\n\
         ldp x27, x28, [x0, #216]\n\
         ldp x29, x30, [x0, #232]\n\
         ldr x0, [x0, #0]\n\
         eret\n\
     .size aarch64_enter_user, . - aarch64_enter_user\n"
);

unsafe extern "C" {
    static aarch64_exception_vectors: u8;
}

pub fn install() {
    let address = core::ptr::addr_of!(aarch64_exception_vectors) as u64;
    unsafe {
        asm!("msr VBAR_EL1, {address}", "isb", address = in(reg) address, options(nostack));
    }
}

pub fn current_el() -> u8 {
    let value: u64;
    unsafe { asm!("mrs {value}, CurrentEL", value = out(reg) value, options(nomem, nostack)) };
    ((value >> 2) & 0x3) as u8
}

pub fn enable_irqs() {
    unsafe { asm!("msr DAIFClr, #2", "isb", options(nostack)) };
}

unsafe extern "C" {
    fn aarch64_enter_user(frame: *const Aarch64TrapFrame) -> !;
}

pub(crate) fn enter_user(frame: &Aarch64TrapFrame) -> ! {
    unsafe { aarch64_enter_user(frame as *const Aarch64TrapFrame) }
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_exception_sync(frame: *mut Aarch64TrapFrame) {
    let frame = unsafe { &mut *frame };
    // SError arrives through this same entry (all slots share one stub). Its
    // syndrome lives in DISR_EL1 while ESR_EL1 keeps its previous value, so
    // an SError presents as esr=0 with an ordinary instruction at elr. The
    // write-back clears the pending interrupt and its sticky valid bit.
    let disr: u64;
    unsafe {
        // DISR_EL1 by encoding (S3_0_C12_C1_1); LLVM rejects the symbolic
        // name at this arch level.
        asm!("mrs {disr}, S3_0_C12_C1_1", disr = out(reg) disr, options(nomem, nostack));
    }
    uart::put_hex("sync: disr=", disr);
    if disr & (1 << 31) != 0 {
        // DISR_EL1.A (bit 31) is the SError-valid flag.
        uart::puts("aarch64 exception: serror (deferred interrupt)\n");
        unsafe {
            asm!("msr S3_0_C12_C1_1, {disr}", disr = in(reg) disr, options(nostack));
        }
    }
    #[cfg(fullerene_aarch64_bramble)]
    if super::usb::early_handoff_in_progress()
        && option_env!("FULLERENE_AARCH64_DEBUG_RETURN") == Some("1")
    {
        // A synchronous abort during the early handoff must not fall into
        // the normal WFE halt: that leaves a physical phone on the Google
        // logo with no way to run the next RAM-only A/B. Return through the
        // same volatile IMEM marker used by the explicit diagnostic command.
        // Preserve the fault first: the SMC below resets the temporary image,
        // so without this retained record the normal Android-init path gives
        // no evidence that it failed inside USB MMIO ownership.
        super::usb::trace_sync_exception(frame.esr_el1, frame.far_el1, frame.elr_el1);
        super::usb::return_to_boot_chain();
    }
    if frame.from_user()
        && ((frame.esr_el1 >> 26) & 0x3f) == 0x15
        && super::syscall::dispatch(frame)
    {
        super::task::deliver_pending_linux_signal(frame);
        return;
    }
    if frame.from_user()
        && is_lower_el_abort(frame.esr_el1)
        && super::task::handle_user_fault(frame)
    {
        return;
    }
    #[cfg(fullerene_aarch64_bramble)]
    // Bracket stage 12 (selector `usb2-live-pollbracket`): a synchronous
    // exception was taken. Emitted before the retained-trace marker, before the
    // UART dump and before `halt()` - all three of which can consume the rest of
    // the boot without ever returning (there is no reachable UART on this unit,
    // so the dump path parks and the apps watchdog resets the handset). A fired
    // stage 12 therefore means the handoff's stop was an exception, not a wait,
    // and the faulting access is what the handoff's own markers cannot see. See
    // the bracket contract in `usb/poll.rs`.
    super::usb::probe_bracket_mark(12);
    #[cfg(fullerene_aarch64_bramble)]
    super::usb::trace_marker(super::usb::TRACE_EXCEPTION_SYNC, 0);
    #[cfg(fullerene_aarch64_bramble)]
    super::usb::dump_trace();
    uart::puts("aarch64 exception: synchronous fault\n");
    uart::put_hex("exception: frame_size=", Aarch64TrapFrame::BYTE_SIZE as u64);
    uart::put_hex("exception: from_user=", frame.from_user() as u64);
    report_exception_state(frame);
    halt()
}

fn is_lower_el_abort(esr: u64) -> bool {
    matches!((esr >> 26) & 0x3f, 0x20 | 0x21 | 0x24 | 0x25)
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_exception_irq(frame: *mut Aarch64TrapFrame) {
    let frame = unsafe { &mut *frame };
    let interrupt_id: u64;
    #[cfg(fullerene_aarch64_ane)]
    {
        // The ANE's GIC-400 is memory-mapped only: `mrs ICC_IAR1_EL1` is an
        // undefined instruction there, and the first timer IRQ of stepmarks16
        // died exactly on it. Acknowledge through GICC_IAR.
        // stepmarks18: trace the first IRQ only, so the last printed line
        // localises an abort inside the handler.
        use core::sync::atomic::{AtomicU32, Ordering};
        static IRQ_TRACE: AtomicU32 = AtomicU32::new(0);
        let irq_index = IRQ_TRACE.fetch_add(1, Ordering::Relaxed);
        if irq_index < 3 {
            uart::puts("irq: entered, acking\n");
        }
        interrupt_id = unsafe {
            core::ptr::read_volatile((super::platform::ane::GICC_BASE + 0x0c) as *const u32) as u64
        };
        if irq_index < 3 {
            uart::put_hex("irq: id=", interrupt_id);
        }
    }
    #[cfg(not(fullerene_aarch64_ane))]
    unsafe {
        asm!(
            "mrs {interrupt_id}, ICC_IAR1_EL1",
            interrupt_id = out(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
    #[cfg(fullerene_aarch64_bramble)]
    let usb_irq = super::platform::bramble::is_usb_irq(interrupt_id as u32);
    #[cfg(not(fullerene_aarch64_bramble))]
    let usb_irq = false;
    if !usb_irq && interrupt_id as u32 != super::timer::TIMER_PPI {
        // DWC3's IRQ path must stay free of UART MMIO: a host SETUP can
        // arrive immediately after Connect Done, and the UART transaction
        // is much slower than draining the event buffer. Keep diagnostics
        // for unexpected non-timer interrupts only.
        //
        // The spurious id (0x3FF) arrives in storms (tens of thousands per
        // second), and a UART write per hit both saturates the durable log
        // and wastes most of the interrupt budget. Log each new small id
        // once, the first few hits of anything, then a rate sample.
        use core::sync::atomic::{AtomicU32, Ordering};
        static IRQ_LOG_SEEN: AtomicU32 = AtomicU32::new(0);
        static IRQ_LOG_TOTAL: AtomicU32 = AtomicU32::new(0);
        let id = interrupt_id as u32;
        let total = IRQ_LOG_TOTAL.fetch_add(1, Ordering::Relaxed);
        let bit = if id < 32 { 1u32 << id } else { 0 };
        let is_new = bit != 0 && (IRQ_LOG_SEEN.fetch_or(bit, Ordering::Relaxed) & bit) == 0;
        if is_new || total < 8 || total % 65536 == 0 {
            uart::put_hex("aarch64 exception: irq id=", interrupt_id);
        }
    }
    #[cfg(fullerene_aarch64_bramble)]
    if usb_irq {
        let controller_irq = interrupt_id as u32 == super::platform::bramble::usb_controller_irq();
        if !controller_irq {
            super::usb::handle_platform_irq(interrupt_id as u32);
            if interrupt_id as u32 == super::platform::bramble::usb_typec_parent_irq() {
                unsafe {
                    super::platform::common::gicv3::disable_spis(
                        super::platform::bramble::GICD_BASE,
                        &[interrupt_id as u32],
                    );
                }
            }
        } else {
            // Auxiliary Qualcomm IRQs are platform notifications. Drain the
            // DWC3 event ring only for the controller SPI; deferred Type-C
            // work runs from the normal polling context after eret.
            #[cfg(not(fullerene_aarch64_usb_probe_timer_event_poll))]
            super::usb::poll();
        }
    }
    if interrupt_id as u32 == super::timer::TIMER_PPI {
        super::timer::arm_ms(1);
        let _ = super::task::wake_event_timeouts(super::timer::uptime_us());
        super::fs::fire_timers(super::timer::uptime_us().saturating_mul(1_000));
        #[cfg(fullerene_aarch64_ane)]
        {
            // Survival beacon: one line roughly every ten seconds of uptime.
            // The last beacon in the log tail says whether a silent stop was
            // the loader's fallback timer (beacons run to ~the window time)
            // or an internal stall (they stop early).
            use core::sync::atomic::{AtomicU32, Ordering};
            static HEARTBEATS: AtomicU32 = AtomicU32::new(0);
            let beats = HEARTBEATS.fetch_add(1, Ordering::Relaxed);
            // Take over the loader's SP805 boot watchdog: feed it every 512th
            // timer interrupt (tens of milliseconds now; about half a second
            // once the counter frequency is corrected). A wedged kernel stops
            // feeding and the loader's fallback still runs.
            if beats % 512 == 0 {
                super::platform::ane::watchdog_kick();
            }
            if beats != 0 && beats % 262_144 == 0 {
                // (was % 10_000: the fixed log window is ~7 KiB and the
                // beacons alone cycled it every couple of minutes, so a
                // fresh boot's lines vanished before they could be read
                // back overnight. 262144 beats leaves the boot section
                // readable for well over an hour at the measured rate.)
                uart::put_hex("hb: count=", beats as u64);
                uart::put_hex(" hb: secs=", super::timer::uptime_us() / 1_000_000);
                // The SP805's live countdown: a value that resets between
                // beats proves the feed is reaching the watchdog.
                let wd = super::platform::ane::watchdog_state();
                uart::put_hex(" wd: ctrl=", wd.control as u64);
                uart::put_hex(" wd: value=", wd.value as u64);
            }
        }
        #[cfg(all(
            fullerene_aarch64_bramble,
            any(
                feature = "aarch64-android-init",
                fullerene_aarch64_usb_probe_timer_event_poll
            )
        ))]
        super::usb::poll_from_timer_irq();
    }
    if frame.from_user() {
        super::task::deliver_pending_linux_signal(frame);
    }
    if usb_irq {
        // Make the event-count acknowledgement visible before deasserting a
        // level-sensitive SPI. This mirrors the readl/writel ordering in the
        // Linux DWC3 interrupt path and prevents an avoidable IRQ retrigger.
        unsafe { asm!("dsb sy", options(nostack)) };
    }
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        use core::sync::atomic::{AtomicU32, Ordering};
        static EOI_TRACE: AtomicU32 = AtomicU32::new(0);
        let eoi_index = EOI_TRACE.fetch_add(1, Ordering::Relaxed);
        if eoi_index < 3 {
            uart::puts("irq: body done\n");
        }
        core::ptr::write_volatile(
            (super::platform::ane::GICC_BASE + 0x10) as *mut u32,
            interrupt_id as u32,
        );
        if eoi_index < 3 {
            uart::puts("irq: eoi done\n");
        }
    }
    #[cfg(not(fullerene_aarch64_ane))]
    unsafe {
        asm!(
            "msr ICC_EOIR1_EL1, {interrupt_id}",
            interrupt_id = in(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
}

fn report_exception_state(frame: &Aarch64TrapFrame) {
    uart::put_hex("esr: ", frame.esr_el1);
    uart::put_hex("elr: ", frame.elr_el1);
    uart::put_hex("far: ", frame.far_el1);
}

fn halt() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
