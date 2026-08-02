//! Context switching implementation for Fullerene OS.
//!
//! Rust builds a complete, testable transition plan before entering the
//! assembly boundary. The assembly only saves the old kernel continuation,
//! changes CR3, and transfers control using the already-copied plan. In
//! particular, it never dereferences a `ProcessContext` after CR3 changes.

use crate::process::{GeneralRegisters, ProcessContext};

/// The kind of continuation that the low-level trampoline must enter.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchEntry {
    /// Resume a kernel continuation saved on `new_kernel_rsp`.
    KernelContinuation = 0,
    /// Enter a user process for the first time through `iretq`.
    FirstUser = 1,
    /// Enter a kernel process for the first time through its entry point.
    FirstKernel = 2,
}

/// Complete input to the low-level context-switch trampoline.
///
/// This is deliberately a value object rather than a pair of process
/// pointers. All data needed after CR3 changes is copied here while the old
/// address space is still active. `old_context` is the sole pointer retained:
/// it is written before CR3 changes so the old task can resume later.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct ContextSwitchPlan {
    old_context: *mut ProcessContext,
    new_cr3: u64,
    new_kernel_stack: u64,
    new_kernel_rsp: u64,
    entry: SwitchEntry,
    _padding: [u8; 7],
    registers: GeneralRegisters,
    rflags: u64,
    rip: u64,
    cs: u64,
    ss: u64,
}

impl ContextSwitchPlan {
    /// Build the immutable transition data from a process context.
    pub fn new(
        old_context: *mut ProcessContext,
        new_context: &ProcessContext,
        new_cr3: u64,
        new_kernel_stack: u64,
    ) -> Self {
        let entry = if new_context.kernel_rsp != 0 {
            SwitchEntry::KernelContinuation
        } else if new_context.is_user {
            SwitchEntry::FirstUser
        } else {
            SwitchEntry::FirstKernel
        };
        Self {
            old_context,
            new_cr3,
            new_kernel_stack,
            new_kernel_rsp: new_context.kernel_rsp,
            entry,
            _padding: [0; 7],
            registers: new_context.registers,
            rflags: new_context.rflags,
            rip: new_context.rip,
            cs: new_context.segments.cs,
            ss: new_context.segments.ss,
        }
    }

    pub fn entry(&self) -> SwitchEntry {
        self.entry
    }

    pub fn new_cr3(&self) -> u64 {
        self.new_cr3
    }

    pub fn new_kernel_stack(&self) -> u64 {
        self.new_kernel_stack
    }

    pub fn new_kernel_rsp(&self) -> u64 {
        self.new_kernel_rsp
    }
}

const PLAN_OLD_CONTEXT_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, old_context);
const PLAN_NEW_CR3_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, new_cr3);
const PLAN_NEW_KERNEL_STACK_OFFSET: usize =
    core::mem::offset_of!(ContextSwitchPlan, new_kernel_stack);
const PLAN_NEW_KERNEL_RSP_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, new_kernel_rsp);
const PLAN_ENTRY_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, entry);
const PLAN_REGISTERS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, registers);
const PLAN_RFLAGS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, rflags);
const PLAN_RIP_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, rip);
const PLAN_CS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, cs);
const PLAN_SS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, ss);

const CONTEXT_KERNEL_RSP_OFFSET: usize = core::mem::offset_of!(ProcessContext, kernel_rsp);
const REG_RAX_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rax);
const REG_RBX_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rbx);
const REG_RCX_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rcx);
const REG_RDX_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rdx);
const REG_RSI_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rsi);
const REG_RDI_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rdi);
const REG_RBP_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rbp);
const REG_RSP_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, rsp);
const REG_R8_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r8);
const REG_R9_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r9);
const REG_R10_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r10);
const REG_R11_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r11);
const REG_R12_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r12);
const REG_R13_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r13);
const REG_R14_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r14);
const REG_R15_OFFSET: usize = core::mem::offset_of!(GeneralRegisters, r15);

static_assertions::const_assert_eq!(PLAN_OLD_CONTEXT_OFFSET, 0);
static_assertions::const_assert_eq!(PLAN_NEW_CR3_OFFSET, 8);
static_assertions::const_assert_eq!(PLAN_NEW_KERNEL_STACK_OFFSET, 16);
static_assertions::const_assert_eq!(PLAN_NEW_KERNEL_RSP_OFFSET, 24);
static_assertions::const_assert_eq!(PLAN_ENTRY_OFFSET, 32);
static_assertions::const_assert_eq!(PLAN_REGISTERS_OFFSET, 40);
static_assertions::const_assert_eq!(PLAN_RFLAGS_OFFSET, 168);
static_assertions::const_assert_eq!(PLAN_RIP_OFFSET, 176);
static_assertions::const_assert_eq!(PLAN_CS_OFFSET, 184);
static_assertions::const_assert_eq!(PLAN_SS_OFFSET, 192);
static_assertions::const_assert_eq!(core::mem::size_of::<ContextSwitchPlan>(), 208);

/// Number of bytes reserved below a new kernel stack for the copied register
/// image. The iret frame occupies the final 40 bytes below the stack top.
const ENTRY_SCRATCH_SIZE: u64 = 176;
const IRET_FRAME_SIZE: u64 = 40;

/// Save the current kernel continuation and switch according to `plan`.
///
/// The plan must remain alive until this function returns. For a first user
/// entry it does not return until the process is later switched away from.
///
/// # Safety
///
/// The pointers and physical address in `plan` must be valid for the current
/// scheduler state, and the new kernel stack must be mapped in `new_cr3`.
#[inline(never)]
pub unsafe extern "sysv64" fn switch_context(plan: &ContextSwitchPlan) {
    debug_assert_ne!(plan.new_cr3, 0);
    unsafe {
        switch_context_trampoline(plan);
    }
    core::hint::black_box(plan);
}

/// Assembly boundary. All process data is read from the plan before CR3 is
/// changed. The plan's register image is copied to the new kernel stack so
/// the post-CR3 path needs no old-address-space pointer at all.
#[unsafe(naked)]
unsafe extern "sysv64" fn switch_context_trampoline(_plan: *const ContextSwitchPlan) {
    core::arch::naked_asm!(
        // Preserve the old continuation and make the switch atomic.
        "pushfq",
        "cli",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Save the old continuation pointer before any address-space change.
        "mov rax, [rdi + {old_context}]",
        "test rax, rax",
        "jz 1f",
        "mov [rax + {context_kernel_rsp}], rsp",
        "1:",

        // Load all plan fields needed after CR3 changes.
        "mov rdx, [rdi + {new_cr3}]",
        "mov r8, [rdi + {new_kernel_stack}]",
        "mov r9, [rdi + {new_kernel_rsp}]",
        "mov r10b, [rdi + {entry}]",

        // A suspended kernel continuation already has its complete register
        // image on the destination stack. No plan dereference follows CR3.
        "cmp r10b, {kernel_continuation}",
        "je 4f",

        // First user entry: copy the register image and construct the
        // transition frame while the old address space is still active.
        "cmp r10b, {first_user}",
        "jne 3f",
        "mov rax, [rdi + {plan_reg_rsp}]",
        "mov [r8 - 32], rax",
        "mov rax, [rdi + {rflags}]",
        "mov [r8 - 24], rax",
        "mov rax, [rdi + {rip}]",
        "mov [r8 - 8], rax",
        "mov rax, [rdi + {cs}]",
        "mov [r8 - 16], rax",
        "mov rax, [rdi + {ss}]",
        "mov [r8 - 40], rax",
        "lea rsi, [rdi + {registers}]",
        "mov rdi, r8",
        "sub rdi, {scratch_size}",
        "mov rcx, 16",
        "rep movsq",
        "jmp 5f",

        // First kernel entry needs only its initial RSP/RIP/RFLAGS image.
        "3:",
        "mov rax, [rdi + {plan_reg_rsp}]",
        "mov rcx, [rdi + {rflags}]",
        "mov rsi, [rdi + {rip}]",
        "mov cr3, rdx",
        "mov rsp, rax",
        "push 0",
        "push rcx",
        "popfq",
        "jmp rsi",

        // First user entry. The copied register block remains at
        // new_stack - ENTRY_SCRATCH_SIZE and is mapped by the new CR3.
        "5:",
        "mov cr3, rdx",
        "mov rsp, r8",
        "sub rsp, {iret_size}",
        "mov rsi, r8",
        "sub rsi, {scratch_size}",
        "mov rax, [rsi + {reg_rax}]",
        "mov rbx, [rsi + {reg_rbx}]",
        "mov rcx, [rsi + {reg_rcx}]",
        "mov rdx, [rsi + {reg_rdx}]",
        "mov rdi, [rsi + {reg_rdi}]",
        "mov rbp, [rsi + {reg_rbp}]",
        "mov r8, [rsi + {reg_r8}]",
        "mov r9, [rsi + {reg_r9}]",
        "mov r10, [rsi + {reg_r10}]",
        "mov r11, [rsi + {reg_r11}]",
        "mov r12, [rsi + {reg_r12}]",
        "mov r13, [rsi + {reg_r13}]",
        "mov r14, [rsi + {reg_r14}]",
        "mov r15, [rsi + {reg_r15}]",
        // RSP is supplied by the iret frame; RSI must be loaded last because
        // it is also the temporary source pointer.
        "mov rsi, [rsi + {reg_rsi}]",
        "iretq",

        // Resume a suspended kernel continuation.
        "4:",
        "mov cr3, rdx",
        "mov rsp, r9",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "popfq",
        "ret",

        old_context = const PLAN_OLD_CONTEXT_OFFSET,
        new_cr3 = const PLAN_NEW_CR3_OFFSET,
        new_kernel_stack = const PLAN_NEW_KERNEL_STACK_OFFSET,
        new_kernel_rsp = const PLAN_NEW_KERNEL_RSP_OFFSET,
        entry = const PLAN_ENTRY_OFFSET,
        registers = const PLAN_REGISTERS_OFFSET,
        rflags = const PLAN_RFLAGS_OFFSET,
        rip = const PLAN_RIP_OFFSET,
        cs = const PLAN_CS_OFFSET,
        ss = const PLAN_SS_OFFSET,
        plan_reg_rsp = const PLAN_REGISTERS_OFFSET + REG_RSP_OFFSET,
        context_kernel_rsp = const CONTEXT_KERNEL_RSP_OFFSET,
        reg_rax = const REG_RAX_OFFSET,
        reg_rbx = const REG_RBX_OFFSET,
        reg_rcx = const REG_RCX_OFFSET,
        reg_rdx = const REG_RDX_OFFSET,
        reg_rsi = const REG_RSI_OFFSET,
        reg_rdi = const REG_RDI_OFFSET,
        reg_rbp = const REG_RBP_OFFSET,
        reg_r8 = const REG_R8_OFFSET,
        reg_r9 = const REG_R9_OFFSET,
        reg_r10 = const REG_R10_OFFSET,
        reg_r11 = const REG_R11_OFFSET,
        reg_r12 = const REG_R12_OFFSET,
        reg_r13 = const REG_R13_OFFSET,
        reg_r14 = const REG_R14_OFFSET,
        reg_r15 = const REG_R15_OFFSET,
        kernel_continuation = const SwitchEntry::KernelContinuation as u8,
        first_user = const SwitchEntry::FirstUser as u8,
        scratch_size = const ENTRY_SCRATCH_SIZE,
        iret_size = const IRET_FRAME_SIZE,
    );
}

/// Initialize context switching system.
pub fn init() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(is_user: bool, kernel_rsp: u64) -> ProcessContext {
        let mut ctx = ProcessContext::default();
        ctx.is_user = is_user;
        ctx.kernel_rsp = kernel_rsp;
        ctx.registers.rsp = 0x7000;
        ctx.rip = 0x2000;
        ctx.segments.cs = 0x1b;
        ctx.segments.ss = 0x23;
        ctx
    }

    #[test]
    fn first_user_context_builds_user_entry_plan() {
        let ctx = context(true, 0);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x9000);
        assert_eq!(plan.entry(), SwitchEntry::FirstUser);
        assert_eq!(plan.new_cr3(), 0x123000);
        assert_eq!(plan.new_kernel_stack(), 0x9000);
        assert_eq!(plan.new_kernel_rsp(), 0);
        assert_eq!(plan.registers.rsp, 0x7000);
        assert_eq!(plan.rip, 0x2000);
    }

    #[test]
    fn resumed_context_takes_precedence_over_user_flag() {
        let ctx = context(true, 0xfeed_0000);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x9000);
        assert_eq!(plan.entry(), SwitchEntry::KernelContinuation);
        assert_eq!(plan.new_kernel_rsp(), 0xfeed_0000);
    }

    #[test]
    fn first_kernel_context_is_distinct_from_user_entry() {
        let ctx = context(false, 0);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x9000);
        assert_eq!(plan.entry(), SwitchEntry::FirstKernel);
    }

    #[test]
    fn plan_has_stable_wire_layout() {
        assert_eq!(PLAN_ENTRY_OFFSET, 32);
        assert_eq!(PLAN_REGISTERS_OFFSET, 40);
        assert_eq!(PLAN_RFLAGS_OFFSET, 168);
        assert_eq!(core::mem::size_of::<ContextSwitchPlan>(), 208);
        assert_eq!(ENTRY_SCRATCH_SIZE, 176);
        assert_eq!(IRET_FRAME_SIZE, 40);
    }
}
