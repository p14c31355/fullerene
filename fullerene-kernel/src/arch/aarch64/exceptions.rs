use core::arch::{asm, global_asm};

use super::uart;

/// World-readable tick counter (the IRQ heartbeat count). The boot and vfs
/// instrument marks stamp this into the durable ring, so the tick value at
/// the last mark says when the world died. A constant value across repeated
/// boots of one image = a fixed-time trigger; a varying one = state-driven.
pub(crate) static TICK_NOW: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

// ── IRQ stage trace ────────────────────────────────────────────────

/// Ordered stages of the IRQ handler.
///
/// The byte values are the step letters written into the durable ring: a
/// wedged handler leaves its last stage's letter as the final ring byte, and
/// `LastStage` (also mirrorable to the panel) names the same stage. Keeping
/// the letters identical preserves what previous builds already proved
/// (A, B, C reached; nothing after).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum IrqStage {
    Entered = b'A',
    AckPainted = b'B',
    IarRead = b'C',
    UnexpectedIdLogged = b'D',
    TimerEntered = b'E',
    TimerArmed = b'F',
    ProbesPainted = b'G',
    TimersFired = b'H',
    S2Painted = b'I',
    El0Checked = b'J',
    S3Painted = b'K',
    ResponderPumped = b'L',
    ElrPainted = b'M',
    PersistSliced = b'N',
    S4Painted = b'O',
    Heartbeat = b'P',
    TimerDone = b'Q',
    SignalDelivered = b'T',
    BodyDone = b'U',
    EoiWritten = b'V',
    EoiDone = b'W',
    Complete = b'Z',
}

/// Last IRQ-handler stage reached (queried by diagnostics).
#[cfg(fullerene_aarch64_ane)]
pub(crate) static IRQ_LAST_STAGE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0);

/// v224: the interrupted PC of the most recent IRQ - the boot thread's
/// suspended PC whenever it is parked (see aarch64_exception_irq).
/// Readable via the 256-beat re-stamp ("PC:<16 hex>").
pub static LAST_IRQ_PC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// v228: the most recent IRQ's saved fault/status registers. If the parked
/// thread is really looping through a faulting access, ESR_EL1/FAR_EL1 will
/// keep the last fault's status and address - readable via the re-stamp.
pub(crate) static LAST_IRQ_ESR: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
pub(crate) static LAST_IRQ_FAR: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
pub(crate) static LAST_IRQ_SPSR: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// v230: a rotating trail of the last 8 interrupted PCs. One restamp shows
/// whether the sampled PC is a single parked instruction (all slots equal)
/// or the thread is moving through several (slots differ).
pub(crate) static PC_TRAIL: [core::sync::atomic::AtomicU64; 8] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 8];
pub(crate) static PC_TRAIL_NEXT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// v241: the PC the last restamp printed; identical parked blocks are
/// suppressed so the 2 KB CNXN window can carry the real markers.
pub(crate) static LAST_PRINTED_PC: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// v242: accumulated IRQ-handler time (us) and IRQ count, printed as the
/// IQ: line; acc/n approximates the per-IRQ cost.
pub(crate) static IRQ_US_ACC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub(crate) static IRQ_US_N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// v267: DWC2 SPI (159) delivery counter. The one-shot ring marker falls out
/// of the 2 KB CNXN window within seconds, so the count is restamped in the
/// IQ: line instead - a nonzero value proves the enabled line is actually
/// reaching the CPU.
#[cfg(fullerene_aarch64_ane)]
pub(crate) static USB_SPI_HITS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

#[cfg(fullerene_aarch64_ane)]
pub(crate) fn usb_spi_hits() -> &'static core::sync::atomic::AtomicU32 {
    &USB_SPI_HITS
}

/// v252: per-stage worst cost in counter-us. `stage()` stamps the boundary;
/// the next `stage()` (or the handler tail) attributes the elapsed time to
/// the stage that just ran. The IQ: line prints the worst stage letter +
/// its cost, so one collection names the phase eating the 1.4 s/IRQ instead
/// of a blind A/B. Indexed by stage byte - 'A'..'Z' fits 26 slots.
#[cfg(fullerene_aarch64_ane)]
pub(crate) static IRQ_STAGE_US: [core::sync::atomic::AtomicU32; 26] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 26];
#[cfg(fullerene_aarch64_ane)]
static IRQ_STAGE_T0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(fullerene_aarch64_ane)]
static IRQ_STAGE_CUR: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Set the current IRQ stage: mirror its letter into the durable ring (bounded
/// so a spurious-id storm cannot wash the log) and remember it.
#[cfg(fullerene_aarch64_ane)]
pub(crate) fn stage(s: IrqStage) {
    use core::sync::atomic::{AtomicU32, Ordering};
    static STAGE_BUDGET: AtomicU32 = AtomicU32::new(0);
    IRQ_LAST_STAGE.store(s as u8, Ordering::Relaxed);
    // v252: attribute the time since the previous stage boundary to that
    // stage. worst-cost-per-stage turns the 1.4 s/IRQ mystery into a named
    // phase; the counter keeps running across the whole handler.
    {
        let now = super::timer::uptime_us();
        let t0 = IRQ_STAGE_T0.swap(now, Ordering::Relaxed);
        let prev = IRQ_STAGE_CUR.load(Ordering::Relaxed);
        if prev != 0 {
            let slot = ((prev - b'A') as usize) % 26;
            let dt = now.saturating_sub(t0).min(u32::MAX as u64) as u32;
            let cur = IRQ_STAGE_US[slot].load(Ordering::Relaxed);
            if dt > cur {
                IRQ_STAGE_US[slot].store(dt, Ordering::Relaxed);
            }
        }
        IRQ_STAGE_CUR.store(s as u8, Ordering::Relaxed);
    }
    // v194: budget 1024 -> 128. At ~18 letters per IRQ the old budget spent
    // ~2300 bytes writing the first ~56 IRQs' letters - a third of the 2 KiB
    // CNXN ring tail. 128 letters (~7 IRQs) still covers early-boot hang
    // bisection (the last letter = the stage that hung), while the live
    // state is readable from IRQ_LAST_STAGE anyway.
    if STAGE_BUDGET.fetch_add(1, Ordering::Relaxed) < 128 {
        uart::putc(s as u8);
    }
}
#[cfg(not(fullerene_aarch64_ane))]
pub(crate) fn stage(_s: IrqStage) {}

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

#[cfg(fullerene_aarch64_ane)]
static ANE_TICKN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Timer ticks served so far. ANE panel instruments use this both to prove
/// tick liveness and to stamp the pre-ERET moment (compare with the frozen
/// S marks to see which tick the world stopped on).
#[cfg(fullerene_aarch64_ane)]
pub(crate) fn tick_count() -> u32 {
    ANE_TICKN.load(core::sync::atomic::Ordering::Relaxed)
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
    #[cfg(fullerene_aarch64_ane)]
    {
        // v120: first-instruction sync trace. Painted before any other work so
        // a wedged handler still leaves this line on the panel.
        super::platform::ane_fb::paint_hex_at(31, 300, b"SYC", frame.esr_el1);
        super::platform::ane_fb::flush_row(31);
    }
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
        #[cfg(fullerene_aarch64_ane)]
        {
            use core::sync::atomic::{AtomicU32, Ordering};
            static SERR_COUNT: AtomicU32 = AtomicU32::new(0);
            let n = SERR_COUNT.fetch_add(1, Ordering::Relaxed);
            super::platform::ane_fb::paint_hex_at(34, 300, b"SER", n as u64);
            super::platform::ane_fb::flush_row(34);
        }
        unsafe {
            asm!("msr S3_0_C12_C1_1, {disr}", disr = in(reg) disr, options(nostack));
        }
        // An SError is deferred by nature: the interrupted context is
        // intact, so resume it. Falling through here used to run the
        // SVC/abort checks against the STALE ESR and end in the halt path,
        // which kills the tick loop (watchdog reset ~100 s later) - the
        // exact "frozen from a certain moment" signature on the panel.
        return;
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
        // Once launchd entered EL0 there is no wait loop left to pump the
        // fastboot responder, and every bench read goes dark even though the
        // USB link stays enumerated. The syscall boundary is the user task's
        // guaranteed crossing point, so pump there (the log argument is a
        // no-op: the ring must carry boot markers, not USB chatter).
        #[cfg(fullerene_aarch64_ane)]
        super::ane_service_pump();
        #[cfg(fullerene_aarch64_ane)]
        {
            // Count the syscall and remember the last service number; the
            // panel instruments repaint from these on the IRQ beat batch.
            use core::sync::atomic::Ordering;
            let nr = (frame.x[8] & 0xffff) as u32;
            super::platform::ane_fb::SVC_LAST.store(nr, Ordering::Relaxed);
            let c = super::platform::ane_fb::SVC_COUNT.fetch_add(1, Ordering::Relaxed);
            // Immediate paint on crossings (first + rate-limited), so proof of
            // EL0 progress does not depend on the IRQ refresh staying alive.
            if c % 16 == 0 {
                super::platform::ane_fb::paint_hex_at(
                    20,
                    300,
                    b"SVC",
                    ((c as u64) << 16) | nr as u64,
                );
                super::platform::ane_fb::flush_row(20);
            }
        }
        return;
    }
    if frame.from_user()
        && is_lower_el_abort(frame.esr_el1)
        && super::task::handle_user_fault(frame)
    {
        return;
    }
    #[cfg(fullerene_aarch64_ane)]
    {
        // Panel trace for the last non-SVC synchronous exception: count and
        // the fault triple, painted before any halt so a photo can read it.
        use core::sync::atomic::{AtomicU32, Ordering};
        static EXC_COUNT: AtomicU32 = AtomicU32::new(0);
        let e = EXC_COUNT.fetch_add(1, Ordering::Relaxed);
        let _ = e;
        super::platform::ane_fb::LAST_FLT.store(frame.esr_el1, Ordering::Relaxed);
        super::platform::ane_fb::LAST_FAR.store(frame.far_el1, Ordering::Relaxed);
        super::platform::ane_fb::paint_hex_at(23, 300, b"FLT", frame.esr_el1);
        super::platform::ane_fb::flush_panel();
        super::platform::ane_fb::paint_hex_at(24, 300, b"FAR", frame.far_el1);
        super::platform::ane_fb::flush_panel();
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
    // v224: remember the interrupted PC. If the boot thread "freezes" while
    // the IRQ keeps running, its ELR keeps pointing at the last instruction
    // that retired there - the 256-beat re-stamp prints it into the ring,
    // so a collection shows exactly where the thread is parked.
    LAST_IRQ_PC.store(frame.elr_el1, core::sync::atomic::Ordering::Relaxed);
    // v228: keep the fault/status side of the frame too - if the parked
    // thread loops through a faulting access, ESR/FAR hold the last fault.
    LAST_IRQ_ESR.store(frame.esr_el1, core::sync::atomic::Ordering::Relaxed);
    LAST_IRQ_FAR.store(frame.far_el1, core::sync::atomic::Ordering::Relaxed);
    LAST_IRQ_SPSR.store(frame.spsr_el1, core::sync::atomic::Ordering::Relaxed);
    // v242: start of the IRQ-time accounting (printed as IQ: in the ring).
    let irq_t0 = super::timer::uptime_us();
    // v230: append to the PC trail (plain load/store - the v227 LL/SC
    // lesson; single core, diagnostics only).
    {
        let n = PC_TRAIL_NEXT.load(core::sync::atomic::Ordering::Relaxed);
        PC_TRAIL[(n as usize) & 7].store(frame.elr_el1, core::sync::atomic::Ordering::Relaxed);
        PC_TRAIL_NEXT.store(n.wrapping_add(1), core::sync::atomic::Ordering::Relaxed);
    }
    crate::blackbox::set_irq_context(true);
    let interrupt_id: u64;
    #[cfg(fullerene_aarch64_ane)]
    {
        // The ANE's GIC-400 is memory-mapped only: `mrs ICC_IAR1_EL1` is an
        // undefined instruction there, and the first timer IRQ of stepmarks16
        // died exactly on it. Acknowledge through GICC_IAR.
        //
        // v154 step letters: one byte per IRQ-handler stage, written straight
        // into the durable ring. Whatever the LAST letter in the log tail is,
        // that stage is the one that hung; earlier versions could not tell a
        // hang on the ACK paint from one inside the IAR read or the tick body.
        use core::sync::atomic::{AtomicU32, Ordering};
        static IRQ_TRACE: AtomicU32 = AtomicU32::new(0);
        let irq_index = IRQ_TRACE.fetch_add(1, Ordering::Relaxed);
        stage(IrqStage::Entered);
        // Arrival trace painted BEFORE the GIC ack read so a hang on that
        // read still leaves the last arrival count on the panel.
        // v243: the paint is no longer free on the ANE - the v242 IQ meter
        // measured the whole handler at ~75 ms (~100% duty at 13.3/s), which
        // starves the boot thread into the "crawl" the desktop probes kept
        // hitting. Gate the every-IRQ repaint behind IRQ_QUIET (the desktop
        // raises it) and only flush every 16th IRQ otherwise - the ACK row
        // is still visible, the budget returns to the boot thread.
        let quiet = super::platform::ane_fb::IRQ_QUIET.load(core::sync::atomic::Ordering::Relaxed);
        if !quiet && irq_index % 16 == 0 {
            super::platform::ane_fb::paint_hex_at(25, 300, b"ACK", irq_index as u64);
            super::platform::ane_fb::flush_row(25);
        }
        stage(IrqStage::AckPainted);
        if irq_index < 3 {
            uart::puts("irq: entered, acking\n");
        }
        interrupt_id = unsafe {
            core::ptr::read_volatile((super::platform::ane::GICC_BASE + 0x0c) as *const u32) as u64
        };
        stage(IrqStage::IarRead);
        if irq_index < 3 {
            uart::put_hex32("irq: id=", interrupt_id as u32);
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
        stage(IrqStage::UnexpectedIdLogged);
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
    #[cfg(fullerene_aarch64_ane)]
    if interrupt_id as u32 == super::platform::ane::USB_DWC2_SPI {
        // v264: the event-driven responder. The DWC2 asserts this SPI exactly
        // when GINTSTS has a masked event, so servicing here is real work, not
        // a poll. The one-shot marker falls out of the 2 KB window, so the
        // hit count is restamped in the IQ: line (S=) as well.
        {
            use core::sync::atomic::Ordering;
            let n = usb_spi_hits().fetch_add(1, Ordering::Relaxed);
            if n < 4 {
                crate::blackbox::text_puts("IRQ: USBDWC2\n");
            }
        }
        stage(IrqStage::ResponderPumped);
        super::ane_service_pump();
    }
    if interrupt_id as u32 == super::timer::TIMER_PPI {
        stage(IrqStage::TimerEntered);
        // v246: re-arm FIRST and 10 ms out. The v242/v244/v245 IQ meter
        // showed the handler eating exactly one 64.2 ms period per IRQ
        // (arm_ms(1) + a handler that outlives the deadline = the next
        // interrupt fires immediately on ERET = back-to-back IRQs = the
        // boot thread starved). arming before the body plus a real gap
        // returns the budget to the boot thread regardless of body cost.
        // Regression isolation: the 8e era armed 1 ms here. Restore that
        // interval under a compile-time gate (const str compare is not
        // stable, so resolve through is_some() which folds per build).
        if option_env!("FULLERENE_ANE_ARM_MS_1").is_some() {
            super::timer::arm_ms(1);
        } else if option_env!("FULLERENE_ANE_ARM_LONG").is_some() {
            // v274: the body costs about as much as the 100 ms arm (v269/v272
            // both measured ~0.97 s reported, i.e. roughly one period once the
            // inflation is divided out), which leaves the boot thread ~0%. The
            // v246b sweep stopped at 100 ms, so push the interval an order of
            // magnitude out: at 1 s the responder still answers well inside the
            // host's 5 s control timeout while the boot thread owns ~90% of the
            // wall clock. If the SMK trail fills in this build, the starvation
            // was the duty cycle, not the body's size.
            super::timer::arm_ms(1000);
        } else {
            super::timer::arm_ms(100); // v246b: 10ms still lost to the 45.6ms body; try 100ms
        }
        stage(IrqStage::TimerArmed);
        // Regression isolation: the 8e-era pump ran in the beats block
        // after the IRQ diagnostics; restore that placement under a
        // compile-time gate so both shapes are reproducible.
        //
        // v264: the pump is dropped only in the SPI_ONLY (pure event-driven)
        // build; the default USB_SPI build keeps it as the control so a
        // still-enumerating run proves the SPI's delivery.
        if option_env!("FULLERENE_ANE_PUMP_LATE") != Some("1")
            && option_env!("FULLERENE_ANE_USB_SPI_ONLY").is_none()
            // v324: the timer-tick pump is the stack-collision suspect
            // (the v320-v323 corrupted ELR; the v159 "CNXN froze the
            // remaining stack at the stall depth"). NO_PUMP now gates it;
            // the responder stays alive via the USB SPI IRQ path, the SVC
            // boundary after launchd, and the wait loop.
            && option_env!("FULLERENE_ANE_NO_PUMP").is_none()
        {
            // USB control traffic has a host-side descriptor timeout. Service
            // the ANE responder before diagnostic canvas/probe work.
            #[cfg(fullerene_aarch64_ane)]
            super::ane_service_pump();
        }
        #[cfg(fullerene_aarch64_ane)]
        let tickn = {
            use core::sync::atomic::Ordering;
            ANE_TICKN.fetch_add(1, Ordering::Relaxed)
        };
        #[cfg(fullerene_aarch64_ane)]
        {
            // v269: LOW_DUTY thins the per-tick diagnostic paints (v244
            // measured ~64 ms/IRQ for the row flushes) so the boot thread
            // owns the interval when the link is idle. The instruments keep
            // updating, just at 1/16 the rate - enough for a photo or a
            // collection window.
            let paint_now = option_env!("FULLERENE_ANE_LOW_DUTY").is_none() || tickn % 16 == 0;
            if paint_now {
                super::platform::ane_fb::paint_hex_at(26, 300, b"S1", tickn as u64);
                super::platform::ane_fb::flush_row(26);
            }
            // Per-tick canvas probe: the first pixel of the user block (400,620)
            // and its last pixel (509,719). The tick these flip to 0xffffffff
            // is the tick the EL0 store landed on; staying 0xff000000 up to a
            // stop proves the stores never reached the real canvas.
            if paint_now {
                let first = super::platform::ane_fb::canvas_probe(400, 620) as u64;
                super::platform::ane_fb::paint_hex_at(22, 300, b"UMK", first);
                super::platform::ane_fb::flush_row(22);
                let last = super::platform::ane_fb::canvas_probe(509, 719) as u64;
                super::platform::ane_fb::paint_hex_at(35, 300, b"UM2", last);
                super::platform::ane_fb::flush_row(35);
            }
        }
        stage(IrqStage::ProbesPainted);
        let _ = super::task::wake_event_timeouts(super::timer::uptime_us());
        super::fs::fire_timers(super::timer::uptime_us().saturating_mul(1_000));
        stage(IrqStage::TimersFired);
        #[cfg(fullerene_aarch64_ane)]
        {
            super::platform::ane_fb::paint_hex_at(27, 300, b"S2", tickn as u64);
            super::platform::ane_fb::flush_row(27);
        }
        stage(IrqStage::S2Painted);
        #[cfg(fullerene_aarch64_ane)]
        {
            // Definitive EL0 check: an IRQ taken from the user task proves the
            // eret actually landed there (and the EL0 code is running).
            if frame.from_user() {
                let n = super::platform::ane_fb::EL0_IRQS.fetch_add(1, Ordering::Relaxed);
                // Immediate proof of EL0-sourced ticks: every tick paints its
                // count with only a row-sized flush, so a single EL0 tick is
                // visible even if the world stops right after it.
                super::platform::ane_fb::paint_hex_at(21, 300, b"EL0TICK", n as u64);
                super::platform::ane_fb::flush_row(21);
            }
            stage(IrqStage::El0Checked);
            // Survival beacon: one line roughly every ten seconds of uptime.
            // The last beacon in the log tail says whether a silent stop was
            // the loader's fallback timer (beacons run to ~the window time)
            // or an internal stall (they stop early).
            use core::sync::atomic::{AtomicU32, Ordering};
            static HEARTBEATS: AtomicU32 = AtomicU32::new(0);
            let beats = HEARTBEATS.fetch_add(1, Ordering::Relaxed);
            // v330 recovery: the pump checks the boot thread's heartbeat
            // (see ane_usb_dev::boot_recovery_check) and schedules the
            // loader reset when the boot thread wedges.
            #[cfg(fullerene_aarch64_ane)]
            super::platform::ane_usb_dev::boot_recovery_check(beats);
            TICK_NOW.store(beats.wrapping_add(1), Ordering::Relaxed);
            // ANE desktop clock: solvent reads this as its monotonic time
            // source (nominal milliseconds; set_tsc_per_ms(1000) keeps the
            // unit consistent). Cheap enough for every tick.
            solvent::ANE_TICKS.store(super::timer::uptime_us() / 1000, Ordering::Relaxed);
            // v195: tick-budget watchdog. The uptime clock races (6-18x
            // early), so the us-based ProgressWatchdog's reset stays
            // disabled; a boot that never reaches EL0 (the launcher) or
            // wedges after it sits FOREVER and needs a physical power-hold.
            // Reset on the beat count instead (clock-immune):
            //   (a) no EL0 tick within the first 4096 beats = the launcher
            //       never ran = unrecoverable here;
            //   (b) 16384 beats overall (~1-3 h real) = long cap, the
            //       diagnostics have had ample time.
            // panic! -> the v63 loader chain -> the armed engine reflashes.
            // v196: rule (a) turned out to fire on healthy boots - beats
            // run 3-10/s real, so 4096 beats is only ~7-20 min and the
            // observed post-v195 lifetimes (2-3 min) collapsed. The EL0
            // rule was guessing about the launcher too early. Keep only
            // the long cap (b); the engine's stall rescue covers stuck
            // enumerated boots and the Љ/failed walks still end via the
            // cap at the hour scale.
            let _el0 = super::platform::ane_fb::EL0_IRQS.load(Ordering::Relaxed);
            // v236: under the USB attach churn the beats race hard and the
            // old 16384 cap landed in ~25-35 s real, cutting boots down
            // mid-dance (the -71/-75 redraws never got their 3rd draw).
            // 0x200000 keeps the "forever wedged" protection at hour scale
            // while letting the attach dance and a crawling boot finish.
            if beats > 0x20_0000 {
                crate::blackbox::text_puts("WD: TICK RESET\n");
                panic!("watchdog: tick budget exhausted");
            }
            // v203 self-heal, v205 gate. Once the kernel has raised the
            // pull-up but this boot NEVER reached the real
            // SET_CONFIGURATION(1) - including a first descriptor request
            // that never produced SETUP_SEEN - keep cycling the USB pull-up
            // while the host has a chance to enumerate. After a bounded
            // number of failed draws, panic -> LK -> automatic reflash is
            // the no-button escape hatch. v205: the gate also requires
            // config-1 to have NEVER been seen (the host's driver-rebind
            // dance opened a false config_value==0 window minutes into
            // healthy boots and the reconnect there broke a collection with
            // ENODEV).
            //
            // v285: the enumeration watchdog becomes opt-out per boot. The
            // v275 NO_PUMP boot proved the boot thread reaches fs::init's
            // second pass when the pump is out of the way, but the
            // enumeration watchdog then killed that boot in ~100 s. When the
            // boot thread itself closes the pump window (PUMP_WINDOW), it is
            // explicitly saying "the handoff is over - do not wait for
            // enumeration"; the watchdog's premise no longer holds, so the
            // boot thread disarms it through the same window flag. The
            // watchdog stays fully armed whenever the window is open or
            // PUMP_WINDOW is not built.
            #[cfg(fullerene_aarch64_ane)]
            {
                // Keep failed USB boots recoverable without waiting for the
                // hour-scale global tick cap.  A healthy boot exits this
                // state permanently at SET_CONFIGURATION(1), so the shorter
                // window only applies to a boot that never enumerated.
                // The historical 8e-era constants travel with the late-pump
                // gate (const str comparison is not stable; option_env! is
                // resolved in main.rs's feature selection, so read the same
                // compile-time shape through a bool the build always folds).
                let pump_late = option_env!("FULLERENE_ANE_PUMP_LATE").is_some();
                let reconnect_wait_beats: u32 = if pump_late { 1024 } else { 256 };
                let reconnect_max: u32 = if pump_late { 6 } else { 3 };
                static RETRIES: AtomicU32 = AtomicU32::new(0);
                static RC_T0: AtomicU32 = AtomicU32::new(0);
                static RC_STATE: AtomicU32 = AtomicU32::new(0); // 0 idle, 1 off
                static S0: AtomicU32 = AtomicU32::new(0); // beats at attach/retry
                static WAIT_LOGGED: core::sync::atomic::AtomicBool =
                    core::sync::atomic::AtomicBool::new(false);
                // v287: fresh-SETUP immunity. The reconnect draws D+ down; if
                // the host is mid-enumeration that KILLS its live attempts
                // (measured 15:2x: -71 on every draw, boot dead in ~40 s).
                // Track the beat of the last SETUP-seen delta; while one is
                // younger than the reconnect interval, the watchdog stands
                // down (the boot thread's predesktop wait owns the exchange).
                static LAST_SETUPS: AtomicU32 = AtomicU32::new(0);
                static LAST_SETUPS_BEAT: AtomicU32 = AtomicU32::new(0);
                // v332r: "the host has talked to us" is the whole wire, not
                // SETUPs alone. A host stuck retrying an enumeration it cannot
                // finish sends resets (and the occasional ENUMDONE) with no
                // SETUP between them; watching SETUPs only made that host look
                // silent, and the watchdog's answer to silence - drop D+
                // mid-retry - is precisely what keeps such a host from ever
                // finishing.
                let setups_now = super::platform::ane_usb_dev::host_events();
                if setups_now != LAST_SETUPS.swap(setups_now, Ordering::Relaxed) {
                    LAST_SETUPS_BEAT.store(beats, Ordering::Relaxed);
                }
                let host_talking = beats.wrapping_sub(LAST_SETUPS_BEAT.load(Ordering::Relaxed))
                    <= reconnect_wait_beats;
                let watchdog_applies = super::platform::ane_usb_dev::usb_attached()
                    && !super::platform::ane_usb_dev::usb_config_was_1()
                    && !super::platform::ane_fb::hold_active()
                    // v285: honor the boot thread's closed duty window - it
                    // explicitly said the handoff is over, so do not wait
                    // for enumeration on its behalf.
                    && super::platform::ane::usb_pump_on()
                    && !host_talking;
                if watchdog_applies {
                    if S0.load(Ordering::Relaxed) == 0 {
                        S0.store(beats, Ordering::Relaxed);
                    }
                    if !WAIT_LOGGED.swap(true, Ordering::Relaxed) {
                        crate::blackbox::text_puts("WD: USB WAIT\n");
                    }
                    if RC_STATE.load(Ordering::Relaxed) == 1 {
                        if beats.wrapping_sub(RC_T0.load(Ordering::Relaxed)) > 10 {
                            super::platform::ane_usb_dev::usb_reconnect_end();
                            RC_STATE.store(0, Ordering::Relaxed);
                            S0.store(beats, Ordering::Relaxed); // restart the wait
                        }
                    } else if beats.wrapping_sub(S0.load(Ordering::Relaxed)) > reconnect_wait_beats
                    {
                        let r = RETRIES.load(Ordering::Relaxed);
                        if r >= reconnect_max {
                            crate::blackbox::text_puts("WD: NO ENUM\n");
                            panic!("watchdog: enumeration never configured");
                        }
                        RETRIES.store(r + 1, Ordering::Relaxed);
                        RC_T0.store(beats, Ordering::Relaxed);
                        RC_STATE.store(1, Ordering::Relaxed);
                        crate::blackbox::text_puts("WD: RECONNECT\n");
                        super::platform::ane_usb_dev::usb_reconnect_begin();
                    }
                }
                // v300: garbled-enumeration self-heal. A boot can reach
                // config-1 with a MANGLED string stage (the one-behind EP0
                // state corrupts exactly the string reads: the host logs
                // Product "Љ", Manufacturer "ANE fastboot", Serial
                // "Fullerene"). config-1 latched, so the enumeration
                // watchdog above never fires again - and the host never
                // talks to that garbled identity, leaving the boot wedged
                // until the tick cap. If the boot claims configured but no
                // string transfer was EVER served, cycle D+ (bounded by the
                // same RETRIES budget). A clean enumeration always reads
                // langid+product strings first, so this cannot false-fire
                // on a healthy link mid-conversation.
                //
                // PREMISE TO VERIFY ON HARDWARE: that the garbled boot
                // leaves this counter at zero. The host-side log names
                // several string indices, so if the corruption is at the
                // transfer level (not the request level) the count may be
                // non-zero and this detector will not fire. First test after
                // flashing: does "WD: GARBLE RECONNECT" appear (or not) on a
                // garbled boot, versus a healthy one.
                //
                // v305 CORRECTION (measured 2026-10-13): the premise is FALSE.
                // The one-behind corruption ROTATES strings between slots -
                // the host still receives (wrong) string data, so
                // STRINGS_SERVED counts up and this branch never fired on a
                // garbled boot. The measured host signature of a garbled
                // enumeration is instead: strings served AND config-1
                // REACHED, but the identity the host latched is wrong
                // (rotated), so the host never OPENS a fastboot conversation
                // (no EP1 traffic, no getvar) yet the responder sees zero
                // subsequent SETUP traffic. The corrected detector: strings
                // served + config-1, and NO new SETUP for the full
                // reconnect wait = the host stopped talking to us, cycle D+.
                // (A healthy parked boot also goes quiet, but it is already
                // covered by the enumeration watchdog; this branch only adds
                // the post-config quiet case.)
                static GARBLE_RETRIES: AtomicU32 = AtomicU32::new(0);
                static GARBLE_T0: AtomicU32 = AtomicU32::new(0);
                static LAST_SETUP_COUNT: AtomicU32 = AtomicU32::new(0);
                // v307: a served EP1 conversation (fastboot command or ADB
                // frame) proves the host's identity latch is good - the
                // opposite of the garbled state this detector hunts. Stand
                // down permanently once one conversation happened: the
                // detector's SETUP-quiet premise is false for a healthy
                // bulk-only session (no control traffic after config-1),
                // so without this latch it kills the very boots that work.
                let had_conversation = super::platform::ane_usb_dev::conversations_served() > 0;
                if !had_conversation
                    && super::platform::ane_usb_dev::usb_config_was_1()
                    && super::platform::ane_usb_dev::strings_served() > 0
                {
                    // v305: snapshot the SETUP counter each check pass.
                    let now_setups = super::platform::ane_usb_dev::setups_seen();
                    let prev = LAST_SETUP_COUNT.swap(now_setups, Ordering::Relaxed);
                    let quiet = now_setups == prev;
                    let g0 = GARBLE_T0.load(Ordering::Relaxed);
                    if g0 == 0 || !quiet {
                        GARBLE_T0.store(beats, Ordering::Relaxed);
                    } else if beats.wrapping_sub(g0) > reconnect_wait_beats {
                        let g = GARBLE_RETRIES.load(Ordering::Relaxed);
                        if g >= reconnect_max {
                            crate::blackbox::text_puts("WD: GARBLE STUCK\n");
                            panic!("watchdog: garbled enumeration never recovered");
                        }
                        GARBLE_RETRIES.store(g + 1, Ordering::Relaxed);
                        GARBLE_T0.store(beats, Ordering::Relaxed);
                        crate::blackbox::text_puts("WD: GARBLE RECONNECT\n");
                        super::platform::ane_usb_dev::usb_reconnect_begin();
                    }
                }
            }
            // v315: PARK_RECOVERY. Every v306+ boot parks the boot thread
            // forever inside usb_reenumerate's settle spin (LAST_IRQ_PC
            // pinned at the spin, ESR/FAR=0, conversations keep being
            // served by this pump). The boot's remaining work - the desktop
            // bring-up - must therefore run HERE, in the pump, once the
            // park is detected. One-shot; the guard makes it re-entrancy
            // safe (the timer IRQ cannot nest here).
            {
                use core::sync::atomic::AtomicBool;
                static PARK_DONE: AtomicBool = AtomicBool::new(false);
                static PARK_SEEN_AT: AtomicU32 = AtomicU32::new(0);
                if !PARK_DONE.load(Ordering::Relaxed)
                    && super::platform::ane_usb_dev::usb_config_was_1()
                {
                    const PARK_PC_LO: u64 = 0x005C_1700;
                    const PARK_PC_HI: u64 = 0x005C_1900;
                    let pc = LAST_IRQ_PC.load(Ordering::Relaxed);
                    let in_park = (PARK_PC_LO..=PARK_PC_HI).contains(&pc);
                    if in_park {
                        let seen = PARK_SEEN_AT.load(Ordering::Relaxed);
                        if seen == 0 {
                            PARK_SEEN_AT.store(beats, Ordering::Relaxed);
                        } else if beats.wrapping_sub(seen) > 512 {
                            PARK_DONE.store(true, Ordering::Relaxed);
                            crate::blackbox::text_puts("WD: PARK RECOVERY\n");
                            super::platform::ane_fb::IRQ_QUIET.store(false, Ordering::Relaxed);
                            crate::ane_park_desktop();
                        }
                    } else {
                        PARK_SEEN_AT.store(0, Ordering::Relaxed);
                    }
                }
            }
            // Forward-progress watchdog: the boot thread narrates its work
            // through the console, so a stall in the counter means the boot
            // thread has stopped making progress. Warn, then reset into the
            // loader so the unmanned engine can reflash.
            // v208: re-stamp the deepest launchd mark every 256 beats so a
            // later collection can see how far the bootstrap got (the
            // one-shot marks fall out of the 2 KB piggyback window within
            // seconds of IRQ noise).
            #[cfg(fullerene_aarch64_ane)]
            if beats != 0 && beats % 1024 == 0 {
                // v241: only restamp when the suspended PC moved (or on a
                // 1-in-16 heartbeat); a parked boot otherwise repeats the
                // same block and crowds the window the desktop needs.
                // v242: accumulated IRQ time and count since boot.
                {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    use core::sync::atomic::Ordering;
                    crate::blackbox::text_puts("IQ: us=");
                    let acc = IRQ_US_ACC.load(Ordering::Relaxed);
                    for sh in (0..16u32).rev() {
                        let nib = ((acc >> (sh * 4)) & 0xf) as usize;
                        crate::blackbox::text_byte(HEX[nib]);
                    }
                    crate::blackbox::text_puts(" n=");
                    let n = IRQ_US_N.load(Ordering::Relaxed);
                    for sh in (0..8u32).rev() {
                        let nib = ((n >> (sh * 4)) & 0xf) as usize;
                        crate::blackbox::text_byte(HEX[nib]);
                    }
                    // v252: the worst stage + its cost. One line names the
                    // phase that owns the 1.4 s/IRQ (armms1 finding) instead
                    // of a blind A/B on the pump placement.
                    //
                    // v267: also the DWC2 SPI hit count (S=), which survives
                    // the 2 KB window where the one-shot marker does not.
                    {
                        crate::blackbox::text_puts(" S=");
                        let s = USB_SPI_HITS.load(Ordering::Relaxed);
                        for sh in (0..8u32).rev() {
                            let nib = ((s >> (sh * 4)) & 0xf) as usize;
                            crate::blackbox::text_byte(HEX[nib]);
                        }
                        // v270: the routing readback. grp = group assignment,
                        // en = the enable bit, tgt = target CPU byte, plus the
                        // low bits of GICD_CTLR/GICC_CTLR. A silent line with
                        // en=1 and tgt=1 means the assignment or the interface
                        // enable is wrong; en=0 means the write did not stick.
                        let (a, b, grp, en, tgt) = super::platform::ane::usb_spi_gic_state();
                        let gs = (a & 0x3) | ((b & 0x3) << 2) | (grp << 4) | (en << 5) | (tgt << 6);
                        crate::blackbox::text_puts(" GS=");
                        for sh in (0..8u32).rev() {
                            let nib = ((gs >> (sh * 4)) & 0xf) as usize;
                            crate::blackbox::text_byte(HEX[nib]);
                        }
                    }
                    {
                        let mut worst_slot = 0usize;
                        let mut worst_us = 0u32;
                        for (i, slot) in IRQ_STAGE_US.iter().enumerate() {
                            let v = slot.load(Ordering::Relaxed);
                            if v > worst_us {
                                worst_us = v;
                                worst_slot = i;
                            }
                        }
                        // v320: the boot-tail stage byte rides on every IQ
                        // line, so any 2 KB window names the current stage.
                        crate::blackbox::text_puts(" G=");
                        let stg = super::platform::ane_fb::STG_STAGE.load(Ordering::Relaxed);
                        crate::blackbox::text_byte(if stg == 0 { b'.' } else { stg });
                        crate::blackbox::text_puts(" W=");
                        crate::blackbox::text_byte(b'A' + worst_slot as u8);
                        let w = worst_us as u64;
                        for sh in (0..8u32).rev() {
                            let nib = ((w >> (sh * 4)) & 0xf) as usize;
                            crate::blackbox::text_byte(HEX[nib]);
                        }
                    }
                    crate::blackbox::text_byte(b'\n');
                }
                let pc_now = LAST_IRQ_PC.load(core::sync::atomic::Ordering::Relaxed);
                let moved = pc_now != LAST_PRINTED_PC.load(core::sync::atomic::Ordering::Relaxed);
                if moved || (beats / 1024) % 16 == 0 {
                    LAST_PRINTED_PC.store(pc_now, core::sync::atomic::Ordering::Relaxed);
                    // v240: restamp cadence relaxed 256 -> 1024 beats so the
                    // 2048-byte CNXN collection window covers ~4x more time and
                    // the desktop markers (TM:/PP:/PR:/RENDER:) are not pushed
                    // out by restamp spam before the collection lands.
                    #[cfg(feature = "aarch64-user-launchd")]
                    {
                        let m = super::launchd::last_mark();
                        if !m.is_empty() {
                            crate::blackbox::text_puts("mark: ");
                            for &b in m {
                                crate::blackbox::text_byte(b);
                            }
                            crate::blackbox::text_byte(b'\n');
                        }
                    }
                    // v214/v215: dump the VFS step-mark trail from its private
                    // location (0x348de800: count@+0, bytes@+4) into the ring.
                    // The CNXN piggyback then carries it within the single
                    // conversation; a separate fetch would be conversation #2
                    // and race the magic reset (measured: the 40 s fetch died
                    // with no OKAY on boot c6).
                    let cnt = unsafe { core::ptr::read_volatile(0x348d_e800u64 as *const u32) };
                    // v252: the trail grew to 32 slots (fs.rs vfs_step); the
                    // old <= 16 reader cut the launchd-era markers off.
                    if cnt > 0 && cnt <= 32 {
                        const HEX: &[u8; 16] = b"0123456789abcdef";
                        crate::blackbox::text_puts("SMK:");
                        for i in 0..cnt {
                            let b = unsafe {
                                core::ptr::read_volatile((0x348d_e804u64 + i as u64) as *const u8)
                            };
                            crate::blackbox::text_byte(HEX[(b >> 4) as usize]);
                            crate::blackbox::text_byte(HEX[(b & 0xf) as usize]);
                        }
                        crate::blackbox::text_byte(b'\n');
                    }
                    // v224: the boot thread's suspended PC (see LAST_IRQ_PC).
                    {
                        const HEX: &[u8; 16] = b"0123456789abcdef";
                        let pc = LAST_IRQ_PC.load(core::sync::atomic::Ordering::Relaxed);
                        crate::blackbox::text_puts("PC:");
                        for sh in (0..16u32).rev() {
                            let nib = ((pc >> (sh * 4)) & 0xf) as usize;
                            crate::blackbox::text_byte(HEX[nib]);
                        }
                        crate::blackbox::text_byte(b'\n');
                        // v230: the 8-deep PC trail (low 32 bits each, hex).
                        crate::blackbox::text_puts("PT:");
                        for slot in 0..8usize {
                            let v =
                                PC_TRAIL[slot].load(core::sync::atomic::Ordering::Relaxed) as u32;
                            for sh in (0..8u32).rev() {
                                let nib = ((v >> (sh * 4)) & 0xf) as usize;
                                crate::blackbox::text_byte(HEX[nib]);
                            }
                            if slot != 7 {
                                crate::blackbox::text_byte(b',');
                            }
                        }
                        crate::blackbox::text_byte(b'\n');
                        // v228: the last fault's status/address, if any (0 when
                        // the interrupted context never faulted).
                        let esr = LAST_IRQ_ESR.load(core::sync::atomic::Ordering::Relaxed);
                        let far = LAST_IRQ_FAR.load(core::sync::atomic::Ordering::Relaxed);
                        let spsr = LAST_IRQ_SPSR.load(core::sync::atomic::Ordering::Relaxed);
                        if esr != 0 || far != 0 {
                            for (label, v) in [(b"ESR:", esr), (b"FAR:", far), (b"SPS:", spsr)] {
                                unsafe {
                                    crate::blackbox::text_puts(core::str::from_utf8_unchecked(
                                        label,
                                    ));
                                }
                                for sh in (0..16u32).rev() {
                                    let nib = ((v >> (sh * 4)) & 0xf) as usize;
                                    crate::blackbox::text_byte(HEX[nib]);
                                }
                                crate::blackbox::text_byte(b'\n');
                            }
                        }
                    }
                    // v226: the VFS static's recorded identity at init ("VA") and
                    // at the read site ("VB"); printed once each marker is set.
                    for (label, slot) in [(b"VA:", 0x348d_e840usize), (b"VB:", 0x348d_e850usize)] {
                        unsafe {
                            if core::ptr::read_volatile((slot + 8) as *const u8) != 0 {
                                const HEX: &[u8; 16] = b"0123456789abcdef";
                                let v = core::ptr::read_volatile(slot as *const u64);
                                crate::blackbox::text_puts(core::str::from_utf8_unchecked(label));
                                for sh in (0..16u32).rev() {
                                    let nib = ((v >> (sh * 4)) & 0xf) as usize;
                                    crate::blackbox::text_byte(HEX[nib]);
                                }
                                crate::blackbox::text_byte(b'\n');
                            }
                        }
                    }
                }
            }
            crate::blackbox::watchdog_check(super::timer::uptime_us());
            // Delayed deliberate reset (the ADB cycle magic): fires the panic
            // ~2 s after the request so the reply reaches the host first.
            crate::blackbox::check_scheduled_reset(super::timer::uptime_us());
            // Take over the loader's SP805 boot watchdog: feed it every 512th
            // timer interrupt (tens of milliseconds now; about half a second
            // once the counter frequency is corrected). A wedged kernel stops
            // feeding and the loader's fallback still runs.
            // Take over the loader's SP805 boot watchdog: feed it every 8th
            // timer interrupt. v170: was %512, chosen when the tick rate
            // looked like ~1 kHz - the measured rate is a handful per second,
            // so %512 meant one feed every ~2 minutes and the SP805's own
            // window (tens of seconds) always won, resetting the board mid-
            // boot. %8 feeds every ~1.5-8 s real: comfortably inside the
            // loader's window.
            if beats % 8 == 0 {
                super::platform::ane::watchdog_kick();
            }
            #[cfg(fullerene_aarch64_ane)]
            {
                super::platform::ane_fb::paint_hex_at(28, 300, b"S3", tickn as u64);
                super::platform::ane_fb::flush_row(28);
            }
            stage(IrqStage::S3Painted);
            // 8e-era pump placement: in the beats block, after diagnostics.
            #[cfg(fullerene_aarch64_ane)]
            if option_env!("FULLERENE_ANE_PUMP_LATE") == Some("1") {
                // v272: LOW_DUTY also bounds the pump's cadence. The v260 meter
                // priced one DWC2 MMIO access at up to 3.6 s, so a pump that
                // runs on every tick can hold the CPU 100% while the link is
                // busy - that is the starvation. At 1-in-4 with a 100 ms arm
                // the host still sees a reply well inside its 5 s control
                // timeout, while the boot thread owns the other three ticks.
                let pump_now = (option_env!("FULLERENE_ANE_LOW_DUTY").is_none() || beats % 4 == 0)
                    && option_env!("FULLERENE_ANE_NO_PUMP").is_none()
                    && super::platform::ane::usb_pump_on();
                if pump_now {
                    super::ane_service_pump();
                }
            }
            if beats % 16 == 0 {
                // Right-column instruments repaint: persists across the
                // left-column repaints that clear overlapping columns.
                super::platform::ane_fb::refresh_instruments(beats as u64);
                // v180: uptime instrument (in seconds). The watchdog's stall
                // thresholds are measured in this clock, and the on-screen
                // evidence (a 20 s stall warning appearing within single
                // digit real seconds of a fresh boot) says this clock runs
                // far faster than real time at least early in the boot.
                // Photography of this row against real time pins the rate.
                super::platform::ane_fb::paint_hex_at(
                    31,
                    300,
                    b"UT",
                    super::timer::uptime_us() / 1_000_000,
                );
                super::platform::ane_fb::flush_row(31);
                // v181: SP805 live state. v183 repack: VALUE full 32 bits,
                // then the post-kick readbacks - lock_rb (expected 0; nonzero
                // = the unlock key never took) and control (expected 03) -
                // then LOAD low 16. Two photos decide whether kicks land.
                let wd = super::platform::ane::watchdog_state();
                super::platform::ane::set_watchdog_lock_rb(wd.lock);
                let lock_rb = super::platform::ane::watchdog_lock_rb();
                super::platform::ane_fb::paint_hex_at(
                    32,
                    300,
                    b"WD",
                    ((wd.value as u64 & 0xffff_ffff) << 32)
                        | ((lock_rb as u64 & 0xff) << 24)
                        | ((wd.control as u64 & 0xff) << 16)
                        | (wd.load as u64 & 0xffff),
                );
                super::platform::ane_fb::flush_row(32);
                // v184: DWC2 EP1 state (DIEPCTL1<<32 | DTXFSTS1) - the
                // endpoint's armed/wedged state while host reads EIO.
                // v186: only after enumeration (pre-enum reads break it).
                if super::platform::ane_usb_dev::usb_enumerated() {
                    super::platform::ane_fb::paint_hex_at(
                        34,
                        300,
                        b"DS",
                        super::platform::ane_usb_dev::ep1_diag(),
                    );
                    super::platform::ane_fb::flush_row(34);
                }
                if beats == 16 {
                    // One-shot tail: the boot's ring narration stays put for
                    // photos. The live channel is the append-only event
                    // ledger (ane_fb::event) - the periodic tail rewrite was
                    // too fast to photograph (v163).
                    super::platform::ane_fb::paint_log_tail();
                }
            }
            stage(IrqStage::ResponderPumped);
            // CPU locator: the exact PC/state the timer interrupted. PSR 0 =
            // EL0t (user), 0x5 = EL1h (kernel). Painted every tick so a dead
            // stop still shows where the core sat.
            // v244: the per-tick ELR/PSR repaint was the remaining ~64 ms/IRQ
            // (6 rows x ~11 ms each; the v242 IQ meter caught the handler at
            // ~100% duty). Paint only every 16th IRQ while the instruments
            // are live and never while the desktop owns the canvas - the
            // locator's job is done once the desktop is up.
            let quiet =
                super::platform::ane_fb::IRQ_QUIET.load(core::sync::atomic::Ordering::Relaxed);
            if !quiet && beats % 16 == 0 {
                super::platform::ane_fb::paint_hex_at(36, 300, b"ELR", frame.elr_el1);
                super::platform::ane_fb::flush_row(36);
                super::platform::ane_fb::paint_hex_at(37, 300, b"PSR", frame.spsr_el1);
                super::platform::ane_fb::flush_row(37);
            }
            stage(IrqStage::ElrPainted);
            // eMMC persistence lane: one rotating 8 KiB ring slice every 8192
            // beats (~0.7% duty at the measured rate). A frozen or reset world
            // still leaves the ring readable via `oem dump` on any later boot.
            // Gated on the pre-ERET marker: before the eret the boot thread
            // owns the SDHCI for its own storage reads, and the single shared
            // controller state machine cannot tolerate a second master.
            // v201: the IRQ-time persist lane is DISABLED. Every
            // non-completing boot of 2026-10-07 evening died at ~beat 8192
            // (~4 m 45 s) - exactly this lane's first write; clean boots
            // included. The same deadlock was first seen at v124 ("stall
            // at the very first beat%8192"). The on-demand `oem ringsave`
            // path is the reliable save and stays. Do not re-enable
            // without a heartbeat witness on the SDHCI side.
            stage(IrqStage::PersistSliced);
            #[cfg(fullerene_aarch64_ane)]
            {
                super::platform::ane_fb::paint_hex_at(
                    29,
                    300,
                    b"S4",
                    super::platform::ane_usb_dev::last_send_state(),
                );
                super::platform::ane_fb::flush_row(29);
            }
            stage(IrqStage::S4Painted);
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
            stage(IrqStage::Heartbeat);
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
    stage(IrqStage::TimerDone);
    if frame.from_user() {
        super::task::deliver_pending_linux_signal(frame);
    }
    stage(IrqStage::SignalDelivered);
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
        stage(IrqStage::BodyDone);
        if interrupt_id as u32 == super::timer::TIMER_PPI {
            super::platform::ane_fb::paint_hex_at(
                30,
                300,
                b"S5",
                ANE_TICKN.load(Ordering::Relaxed) as u64,
            );
            super::platform::ane_fb::flush_row(30);
        }
        core::ptr::write_volatile(
            (super::platform::ane::GICC_BASE + 0x10) as *mut u32,
            interrupt_id as u32,
        );
        stage(IrqStage::EoiWritten);
        if eoi_index < 3 {
            uart::puts("irq: eoi done\n");
        }
        stage(IrqStage::EoiDone);
        if interrupt_id as u32 == super::timer::TIMER_PPI {
            super::platform::ane_fb::paint_hex_at(
                33,
                300,
                b"E5",
                ANE_TICKN.load(Ordering::Relaxed) as u64,
            );
            super::platform::ane_fb::flush_row(33);
        }
        stage(IrqStage::Complete);
        // v252: close the last stage's window (Complete) so the meter covers
        // the whole handler including the EOI tail.
        {
            let now = super::timer::uptime_us();
            let t0 = IRQ_STAGE_T0.swap(now, Ordering::Relaxed);
            let prev = IRQ_STAGE_CUR.load(Ordering::Relaxed);
            if prev != 0 {
                let slot = ((prev - b'A') as usize) % 26;
                let dt = now.saturating_sub(t0).min(u32::MAX as u64) as u32;
                let cur = IRQ_STAGE_US[slot].load(Ordering::Relaxed);
                if dt > cur {
                    IRQ_STAGE_US[slot].store(dt, Ordering::Relaxed);
                }
            }
            IRQ_STAGE_CUR.store(0, Ordering::Relaxed);
        }
        // v261/v262 tail-arm REVERTED: arming after the EOI starved the
        // responder (100 ms: every CNXN died EIO; 10 ms: unrecoverable
        // non-enumeration). The v246 head-arm shape is restored above.
        // v193: gate the trace newline like its neighbors above (779/797).
        // Ungated, this wrote one 0x0A PER TICK (3-10/s) into the durable
        // ring - the "newline flood" that filled the CNXN piggyback
        // collections (2048 x 0x0A) and washed the real boot log out of
        // the tail. Rate match confirmed: the flood rate ~11/s equals the
        // tick rate.
        if eoi_index < 3 {
            uart::putc(b'\n');
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
    // v242: how long did this IRQ handler take? Accumulate for the ring;
    // the IQ: line in the restamp (every 1024 beats) shows the total, and
    // total/beats is the per-IRQ cost that starves the boot thread.
    {
        use core::sync::atomic::Ordering;
        let dt = super::timer::uptime_us().wrapping_sub(irq_t0);
        IRQ_US_ACC.store(IRQ_US_ACC.load(Ordering::Relaxed) + dt, Ordering::Relaxed);
        IRQ_US_N.store(
            IRQ_US_N.load(Ordering::Relaxed).wrapping_add(1),
            Ordering::Relaxed,
        );
    }
    crate::blackbox::set_irq_context(false);
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
