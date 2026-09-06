use core::arch::{asm, global_asm};

use super::uart;

/// Register state captured by an EL1 exception entry.
///
/// The first 31 words match x0..x30, followed by the EL1 return state and
/// the user stack pointer. Keeping this layout explicit is the ABI boundary
/// for future SVC, page-fault, and scheduler paths; assembly only moves this
/// frame, while Rust interprets it.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub(crate) struct Aarch64TrapFrame {
    pub(crate) x: [u64; 31],
    pub(crate) elr_el1: u64,
    pub(crate) spsr_el1: u64,
    pub(crate) sp_el0: u64,
    pub(crate) esr_el1: u64,
    pub(crate) far_el1: u64,
}

impl Aarch64TrapFrame {
    pub(crate) const BYTE_SIZE: usize = core::mem::size_of::<Self>();

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
         sub sp, sp, #288\n\
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
         mov x0, sp\n\
         bl aarch64_exception_irq\n\
         ldr x1, [sp, #248]\n\
         msr ELR_EL1, x1\n\
         ldr x1, [sp, #256]\n\
         msr SPSR_EL1, x1\n\
         ldr x1, [sp, #264]\n\
         msr SP_EL0, x1\n\
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
         add sp, sp, #288\n\
         eret\n\
     .size aarch64_exception_irq_entry, . - aarch64_exception_irq_entry\n\
\
     .global aarch64_exception_sync_entry\n\
     .type aarch64_exception_sync_entry, %function\n\
     aarch64_exception_sync_entry:\n\
         sub sp, sp, #288\n\
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
         mov x0, sp\n\
         bl aarch64_exception_sync\n\
         ldr x1, [sp, #248]\n\
         msr ELR_EL1, x1\n\
         ldr x1, [sp, #256]\n\
         msr SPSR_EL1, x1\n\
         ldr x1, [sp, #264]\n\
         msr SP_EL0, x1\n\
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
         add sp, sp, #288\n\
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
    if frame.from_user()
        && ((frame.esr_el1 >> 26) & 0x3f) == 0x15
        && super::syscall::dispatch(frame)
    {
        return;
    }
    if frame.from_user()
        && is_lower_el_abort(frame.esr_el1)
        && super::task::handle_user_fault(frame)
    {
        return;
    }
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
    let _frame = unsafe { &mut *frame };
    let interrupt_id: u64;
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
    if !usb_irq {
        // DWC3's IRQ path must stay free of UART MMIO: a host SETUP can
        // arrive immediately after Connect Done, and the UART transaction
        // is much slower than draining the event buffer. Keep diagnostics
        // for timer and unexpected interrupts only.
        uart::put_hex("aarch64 exception: irq id=", interrupt_id);
    }
    #[cfg(fullerene_aarch64_bramble)]
    if usb_irq {
        let controller_irq = interrupt_id as u32 == super::platform::bramble::usb_controller_irq();
        if !controller_irq {
            super::usb::handle_platform_irq(interrupt_id as u32);
            if interrupt_id as u32 == super::platform::bramble::usb_typec_parent_irq() {
                unsafe {
                    super::platform::gicv3::disable_spis(
                        super::platform::bramble::GICD_BASE,
                        &[interrupt_id as u32],
                    );
                }
            }
        } else {
            // Auxiliary Qualcomm IRQs are platform notifications. Drain the
            // DWC3 event ring only for the controller SPI; deferred Type-C
            // work runs from the normal polling context after eret.
            super::usb::poll();
        }
    }
    if interrupt_id as u32 == super::timer::TIMER_PPI {
        super::timer::arm_ms(100);
    }
    if usb_irq {
        // Make the event-count acknowledgement visible before deasserting a
        // level-sensitive SPI. This mirrors the readl/writel ordering in the
        // Linux DWC3 interrupt path and prevents an avoidable IRQ retrigger.
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
