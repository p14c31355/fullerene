use core::arch::{asm, global_asm};

use super::uart;

// AArch64 exception vectors are 16 slots of 128 bytes and the table must be
// 2048-byte aligned. All early-boot faults are fatal until the scheduler and
// per-process exception frames exist; keeping one path makes failures visible
// on UART instead of silently looping.
global_asm!(
    ".section .text.exception_vectors,\"ax\"\n\
     .balign 2048\n\
     .global aarch64_exception_vectors\n\
     .type aarch64_exception_vectors, %function\n\
     aarch64_exception_vectors:\n\
     .rept 4\n\
         b aarch64_exception_sync\n\
         .space 124\n\
         b aarch64_exception_irq_entry\n\
         .space 124\n\
         b aarch64_exception_sync\n\
         .space 124\n\
         b aarch64_exception_sync\n\
         .space 124\n\
     .endr\n\
     .size aarch64_exception_vectors, . - aarch64_exception_vectors\n\
\
     .global aarch64_exception_irq_entry\n\
     .type aarch64_exception_irq_entry, %function\n\
     aarch64_exception_irq_entry:\n\
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
         bl aarch64_exception_irq\n\
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
     .size aarch64_exception_irq_entry, . - aarch64_exception_irq_entry\n"
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

#[unsafe(no_mangle)]
extern "C" fn aarch64_exception_sync() -> ! {
    #[cfg(fullerene_aarch64_bramble)]
    super::usb::trace_marker(super::usb::TRACE_EXCEPTION_SYNC, 0);
    #[cfg(fullerene_aarch64_bramble)]
    super::usb::dump_trace();
    uart::puts("aarch64 exception: synchronous fault\n");
    report_exception_state();
    halt()
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_exception_irq() {
    let interrupt_id: u64;
    unsafe {
        asm!(
            "mrs {interrupt_id}, ICC_IAR1_EL1",
            interrupt_id = out(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
    uart::put_hex("aarch64 exception: irq id=", interrupt_id);
    #[cfg(fullerene_aarch64_bramble)]
    if interrupt_id as u32 == super::platform::bramble::USB_DWC3_IRQ {
        // DWC3's event buffer is the source of truth. The IRQ handler only
        // drains it; transfer state transitions remain shared with the early
        // polling path so a pending event cannot be processed twice.
        super::usb::poll();
    }
    if interrupt_id as u32 == super::timer::TIMER_PPI {
        super::timer::arm_ms(100);
    }
    unsafe {
        asm!(
            "msr ICC_EOIR1_EL1, {interrupt_id}",
            interrupt_id = in(reg) interrupt_id,
            options(nomem, nostack)
        );
    }
}

fn report_exception_state() {
    let esr: u64;
    let elr: u64;
    let far: u64;
    unsafe {
        asm!("mrs {esr}, ESR_EL1", esr = out(reg) esr, options(nomem, nostack));
        asm!("mrs {elr}, ELR_EL1", elr = out(reg) elr, options(nomem, nostack));
        asm!("mrs {far}, FAR_EL1", far = out(reg) far, options(nomem, nostack));
    }
    uart::put_hex("esr: ", esr);
    uart::put_hex("elr: ", elr);
    uart::put_hex("far: ", far);
}

fn halt() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
