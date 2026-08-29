//! A bounded Xtensa interrupt and context model.

use core::arch::asm;

pub const TIMER_LEVEL_1: u32 = 1 << 6;

/// Xtensa execution frame for timer preemption. The initial scheduler switches
/// use a minimal frame; expanding to full exception frames is isolated here.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TaskContext {
    pub pc: usize,
    pub ps: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub a8: usize,
    pub a9: usize,
    pub a10: usize,
    pub a11: usize,
    pub a12: usize,
    pub a13: usize,
    pub a14: usize,
    pub a15: usize,
    pub sar: usize,
}

impl TaskContext {
    pub const fn empty() -> Self {
        Self {
            pc: 0,
            ps: 0x20,
            a2: 0,
            a3: 0,
            a4: 0,
            a5: 0,
            a6: 0,
            a7: 0,
            a8: 0,
            a9: 0,
            a10: 0,
            a11: 0,
            a12: 0,
            a13: 0,
            a14: 0,
            a15: 0,
            sar: 0,
        }
    }
}

/// Enable only the level-1 timer interrupt until device interrupts are added.
#[inline]
pub fn enable_timer_interrupt() {
    unsafe {
        asm!(
            "rsr.intenable a2",
            "or a2, a2, a3",
            "wsr.intenable a2",
            "rsync",
            in("a3") TIMER_LEVEL_1,
            lateout("a2") _,
            options(nomem)
        );
    }
}

/// Restore the previous interrupt-enable register value.
#[inline]
pub fn restore_interrupts(value: u32) {
    unsafe {
        asm!("wsr.intenable a2; rsync", in("a2") value as usize, options(nomem));
    }
}

/// Read the interrupt-enable register, preserving only the kernel-owned mask.
#[inline]
pub fn interrupt_state() -> u32 {
    let value: usize;
    unsafe {
        asm!("rsr.intenable a2", out("a2") value, options(nomem, nostack));
    }
    value as u32
}
