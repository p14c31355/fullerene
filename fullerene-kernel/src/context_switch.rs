//! Context switching implementation for Fullerene OS.
//!
//! Rust builds a complete transition plan and materializes the first-entry
//! image on the destination kernel stack. The assembly boundary is kept
//! deliberately dumb: save the old continuation, switch CR3, restore the
//! prepared image, and use `iretq`/`ret`.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSwitchError {
    FirstUserRequiresKernelStack { stack_top: u64, required: u64 },
}

/// Complete input to the low-level context-switch trampoline.
///
/// All data needed after CR3 changes is copied here while the old address
/// space is still active. `old_context` is the sole pointer retained: it is
/// written before CR3 changes so the old task can resume later.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct ContextSwitchPlan {
    old_context: *mut ProcessContext,
    new_cr3: u64,
    new_kernel_stack: u64,
    new_kernel_rsp: u64,
    entry: SwitchEntry,
    _padding: [u8; 7],
    entry_stack: u64,
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
    ) -> Result<Self, ContextSwitchError> {
        let entry = if new_context.kernel_rsp != 0 {
            SwitchEntry::KernelContinuation
        } else if new_context.is_user {
            SwitchEntry::FirstUser
        } else {
            SwitchEntry::FirstKernel
        };
        let entry_stack = if entry == SwitchEntry::FirstUser {
            new_kernel_stack.checked_sub(POST_CALL_STACK_OFFSET).ok_or(
                ContextSwitchError::FirstUserRequiresKernelStack {
                    stack_top: new_kernel_stack,
                    required: POST_CALL_STACK_OFFSET,
                },
            )?
        } else {
            0
        };
        Ok(Self {
            old_context,
            new_cr3,
            new_kernel_stack,
            new_kernel_rsp: new_context.kernel_rsp,
            entry,
            _padding: [0; 7],
            entry_stack,
            registers: new_context.registers,
            rflags: new_context.rflags,
            rip: new_context.rip,
            cs: new_context.segments.cs,
            ss: new_context.segments.ss,
        })
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

    pub fn entry_stack(&self) -> u64 {
        self.entry_stack
    }
}

/// Exact memory image consumed by the first-user-entry pop/iret sequence.
///
/// The field order intentionally matches the assembly pop order. Keeping it
/// as a Rust type lets unit tests validate both register order and the iret
/// frame without executing privileged instructions.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UserEntryImage {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

const USER_ENTRY_IMAGE_SIZE: usize = core::mem::size_of::<UserEntryImage>();
const USER_REGISTER_IMAGE_SIZE: u64 = 15 * 8;
const IRET_FRAME_SIZE: u64 = 5 * 8;
// The diagnostic hooks call into Rust and the direct Klog Live renderer, so
// their stack usage is much larger than a single return address. Keep a real
// guard area between that stack and the user-entry image.
const ENTRY_HOOK_STACK_GAP: u64 = 8192;
const PRE_IRET_CALL_STACK_OFFSET: u64 = ENTRY_HOOK_STACK_GAP + IRET_FRAME_SIZE;
const POST_CALL_STACK_OFFSET: u64 = USER_ENTRY_IMAGE_SIZE as u64 + ENTRY_HOOK_STACK_GAP;

impl UserEntryImage {
    fn from_plan(plan: &ContextSwitchPlan) -> Self {
        let r = plan.registers;
        Self {
            r15: r.r15,
            r14: r.r14,
            r13: r.r13,
            r12: r.r12,
            r11: r.r11,
            r10: r.r10,
            r9: r.r9,
            r8: r.r8,
            rbp: r.rbp,
            rdi: r.rdi,
            rsi: r.rsi,
            rdx: r.rdx,
            rcx: r.rcx,
            rbx: r.rbx,
            rax: r.rax,
            rip: plan.rip,
            cs: plan.cs,
            rflags: plan.rflags,
            rsp: r.rsp,
            ss: plan.ss,
        }
    }
}

/// Materialize the first-user-entry image before changing CR3.
///
/// # Safety
///
/// `plan.entry_stack()` must point to writable memory in the current address
/// space and in the destination address space. The process allocator creates
/// kernel stacks before cloning the process page table, which establishes
/// this invariant for scheduler-created processes.
pub unsafe fn prepare_entry_image(plan: &ContextSwitchPlan) {
    if plan.entry != SwitchEntry::FirstUser || plan.entry_stack == 0 {
        return;
    }
    unsafe {
        (plan.entry_stack as *mut UserEntryImage).write(UserEntryImage::from_plan(plan));
    }
}

/// Last Rust-visible checkpoint after loading the destination CR3 and stack.
/// The direct Klog Live repaint is intentional: interrupts are still disabled
/// at this point, so the ordinary timer-driven repaint cannot run.
#[inline(never)]
extern "sysv64" fn context_switch_post_cr3_stage() {
    crate::klog_fmt!("[CTX-DIAG] post-cr3 kernel stack active\n");
    let _ = crate::klog::try_render_live_surface();
}

#[inline(never)]
extern "sysv64" fn context_switch_pre_iret_stage(
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
) {
    crate::klog_fmt!(
        "[CTX-DIAG] pre-iret restored rip={:#x} cs={:#x} rflags={:#x}\n",
        rip,
        cs,
        rflags
    );
    crate::klog_fmt!("[CTX-DIAG] pre-iret stack rsp={:#x} ss={:#x}\n", rsp, ss);
    let _ = crate::klog::try_render_live_surface();
}

const PLAN_OLD_CONTEXT_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, old_context);
const PLAN_NEW_CR3_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, new_cr3);
const PLAN_NEW_KERNEL_RSP_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, new_kernel_rsp);
const PLAN_ENTRY_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, entry);
const PLAN_ENTRY_STACK_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, entry_stack);
const PLAN_REGISTERS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, registers);
const PLAN_RFLAGS_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, rflags);
const PLAN_RIP_OFFSET: usize = core::mem::offset_of!(ContextSwitchPlan, rip);

const CONTEXT_KERNEL_RSP_OFFSET: usize = core::mem::offset_of!(ProcessContext, kernel_rsp);
const PLAN_REG_RSP_OFFSET: usize =
    PLAN_REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rsp);
const IMAGE_RAX_OFFSET: usize = core::mem::offset_of!(UserEntryImage, rax);
const IMAGE_RCX_OFFSET: usize = core::mem::offset_of!(UserEntryImage, rcx);
const IMAGE_RDX_OFFSET: usize = core::mem::offset_of!(UserEntryImage, rdx);
const IMAGE_RSI_OFFSET: usize = core::mem::offset_of!(UserEntryImage, rsi);
const IMAGE_RDI_OFFSET: usize = core::mem::offset_of!(UserEntryImage, rdi);
const IMAGE_R8_OFFSET: usize = core::mem::offset_of!(UserEntryImage, r8);
const IMAGE_R9_OFFSET: usize = core::mem::offset_of!(UserEntryImage, r9);
const IMAGE_R10_OFFSET: usize = core::mem::offset_of!(UserEntryImage, r10);
const IMAGE_R11_OFFSET: usize = core::mem::offset_of!(UserEntryImage, r11);

static_assertions::const_assert_eq!(PLAN_OLD_CONTEXT_OFFSET, 0);
static_assertions::const_assert_eq!(PLAN_NEW_CR3_OFFSET, 8);
static_assertions::const_assert_eq!(PLAN_NEW_KERNEL_RSP_OFFSET, 24);
static_assertions::const_assert_eq!(PLAN_ENTRY_OFFSET, 32);
static_assertions::const_assert_eq!(PLAN_ENTRY_STACK_OFFSET, 40);
static_assertions::const_assert_eq!(PLAN_REGISTERS_OFFSET, 48);
static_assertions::const_assert_eq!(PLAN_RFLAGS_OFFSET, 176);
static_assertions::const_assert_eq!(PLAN_RIP_OFFSET, 184);
static_assertions::const_assert_eq!(core::mem::size_of::<ContextSwitchPlan>(), 208);
static_assertions::const_assert_eq!(USER_ENTRY_IMAGE_SIZE, 160);

/// Save the current kernel continuation and switch according to `plan`.
///
/// # Safety
///
/// The pointers and physical address in `plan` must be valid for the current
/// scheduler state. `prepare_entry_image` must have been called for a first
/// user entry.
#[inline(never)]
pub unsafe extern "sysv64" fn switch_context(plan: &ContextSwitchPlan) {
    debug_assert_ne!(plan.new_cr3, 0);
    unsafe {
        switch_context_trampoline(plan);
    }
    core::hint::black_box(plan);
}

/// Minimal privileged boundary. All process-layout decisions and first-entry
/// image construction happen in Rust; this code only performs CPU operations
/// that Rust cannot express.
#[unsafe(naked)]
unsafe extern "sysv64" fn switch_context_trampoline(_plan: *const ContextSwitchPlan) {
    core::arch::naked_asm!(
        // Save the old continuation before changing address spaces.
        "pushfq",
        "cli",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rax, [rdi + {old_context}]",
        "test rax, rax",
        "jz 1f",
        "mov [rax + {context_kernel_rsp}], rsp",
        "1:",

        // Capture every value needed after CR3 changes.
        "mov rdx, [rdi + {new_cr3}]",
        "mov r9, [rdi + {new_kernel_rsp}]",
        "mov r10b, [rdi + {entry}]",
        "mov r11, [rdi + {entry_stack}]",

        // Resume a suspended kernel continuation.
        "cmp r10b, {kernel_continuation}",
        "je 3f",

        // First kernel entry. The kernel entry image does not need an iret
        // frame, only the initial kernel RSP/RIP/RFLAGS values.
        "cmp r10b, {first_user}",
        "je 2f",
        "mov rax, [rdi + {plan_reg_rsp}]",
        "mov rcx, [rdi + {rflags}]",
        "mov rsi, [rdi + {rip}]",
        "mov cr3, rdx",
        "mov rsp, rax",
        "push 0",
        "push rcx",
        "popfq",
        "jmp rsi",

        // First user entry. Rust wrote the exact pop/iret image to the new
        // kernel stack before this CR3 change; no old-space read follows it.
        "2:",
        "mov cr3, rdx",
        // Call Rust while the destination kernel stack is active. Keep the
        // image pointer in a callee-saved register; the hook may clobber all
        // caller-saved registers, and the image is restored immediately after.
        "mov r12, r11",
        "mov rsp, r11",
        "add rsp, {post_call_stack}",
        "call {post_cr3_stage}",
        "mov rsp, r12",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        // Preserve the iret frame while calling Rust, then reload all
        // caller-saved registers from the image because the hook may clobber
        // them according to the SysV ABI.
        "mov rdi, [rsp]",
        "mov rsi, [rsp + 8]",
        "mov rdx, [rsp + 16]",
        "mov rcx, [rsp + 24]",
        "mov r8, [rsp + 32]",
        "mov r12, rsp",
        "add rsp, {pre_iret_call_stack}",
        "call {pre_iret_stage}",
        "mov rsp, r12",
        // Callee-saved registers survive the hook call, so reload only the
        // caller-saved registers from the correctly ordered image.
        "lea rax, [r12 - {user_register_image_size}]",
        "mov rcx, [rax + {image_rcx}]",
        "mov rdx, [rax + {image_rdx}]",
        "mov rdi, [rax + {image_rdi}]",
        "mov r8, [rax + {image_r8}]",
        "mov r9, [rax + {image_r9}]",
        "mov r10, [rax + {image_r10}]",
        "mov r11, [rax + {image_r11}]",
        "mov rsi, [rax + {image_rsi}]",
        "mov rax, [rax + {image_rax}]",
        "iretq",

        // Kernel continuation restore.
        "3:",
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
        new_kernel_rsp = const PLAN_NEW_KERNEL_RSP_OFFSET,
        entry = const PLAN_ENTRY_OFFSET,
        entry_stack = const PLAN_ENTRY_STACK_OFFSET,
        plan_reg_rsp = const PLAN_REG_RSP_OFFSET,
        context_kernel_rsp = const CONTEXT_KERNEL_RSP_OFFSET,
        rflags = const PLAN_RFLAGS_OFFSET,
        rip = const PLAN_RIP_OFFSET,
        kernel_continuation = const SwitchEntry::KernelContinuation as u8,
        first_user = const SwitchEntry::FirstUser as u8,
        post_call_stack = const POST_CALL_STACK_OFFSET,
        post_cr3_stage = sym context_switch_post_cr3_stage,
        pre_iret_call_stack = const PRE_IRET_CALL_STACK_OFFSET,
        user_register_image_size = const USER_REGISTER_IMAGE_SIZE,
        pre_iret_stage = sym context_switch_pre_iret_stage,
        image_rax = const IMAGE_RAX_OFFSET,
        image_rcx = const IMAGE_RCX_OFFSET,
        image_rdx = const IMAGE_RDX_OFFSET,
        image_rsi = const IMAGE_RSI_OFFSET,
        image_rdi = const IMAGE_RDI_OFFSET,
        image_r8 = const IMAGE_R8_OFFSET,
        image_r9 = const IMAGE_R9_OFFSET,
        image_r10 = const IMAGE_R10_OFFSET,
        image_r11 = const IMAGE_R11_OFFSET,
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
        ctx.registers.rax = 0x01;
        ctx.registers.rbx = 0x02;
        ctx.registers.rsp = 0x7000;
        ctx.registers.r15 = 0x15;
        ctx.rip = 0x2000;
        ctx.segments.cs = 0x1b;
        ctx.segments.ss = 0x23;
        ctx
    }

    #[test]
    fn first_user_context_builds_user_entry_plan() {
        let ctx = context(true, 0);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x10000).unwrap();
        assert_eq!(plan.entry(), SwitchEntry::FirstUser);
        assert_eq!(plan.new_cr3(), 0x123000);
        assert_eq!(plan.new_kernel_stack(), 0x10000);
        assert_eq!(plan.new_kernel_rsp(), 0);
        assert_eq!(plan.entry_stack(), 0x10000 - POST_CALL_STACK_OFFSET);
    }

    #[test]
    fn user_entry_image_matches_pop_and_iret_order() {
        let ctx = context(true, 0);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x10000).unwrap();
        let image = UserEntryImage::from_plan(&plan);
        assert_eq!(image.r15, 0x15);
        assert_eq!(image.rbx, 0x02);
        assert_eq!(image.rax, 0x01);
        assert_eq!(image.rip, 0x2000);
        assert_eq!(image.cs, 0x1b);
        assert_eq!(image.rsp, 0x7000);
        assert_eq!(image.ss, 0x23);
    }

    #[test]
    fn resumed_context_takes_precedence_over_user_flag() {
        let ctx = context(true, 0xfeed_0000);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x10000).unwrap();
        assert_eq!(plan.entry(), SwitchEntry::KernelContinuation);
        assert_eq!(plan.new_kernel_rsp(), 0xfeed_0000);
        assert_eq!(plan.entry_stack(), 0);
    }

    #[test]
    fn first_kernel_context_is_distinct_from_user_entry() {
        let ctx = context(false, 0);
        let plan = ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0x10000).unwrap();
        assert_eq!(plan.entry(), SwitchEntry::FirstKernel);
        assert_eq!(plan.entry_stack(), 0);
    }

    #[test]
    fn plan_has_stable_wire_layout() {
        assert_eq!(PLAN_ENTRY_OFFSET, 32);
        assert_eq!(PLAN_ENTRY_STACK_OFFSET, 40);
        assert_eq!(PLAN_REGISTERS_OFFSET, 48);
        assert_eq!(PLAN_RFLAGS_OFFSET, 176);
        assert_eq!(core::mem::size_of::<ContextSwitchPlan>(), 208);
        assert_eq!(USER_ENTRY_IMAGE_SIZE, 160);
        assert_eq!(ENTRY_HOOK_STACK_GAP, 8192);
        assert_eq!(core::mem::offset_of!(UserEntryImage, r15), 0);
        assert_eq!(core::mem::offset_of!(UserEntryImage, rax), 112);
        assert_eq!(
            core::mem::offset_of!(UserEntryImage, rip),
            USER_REGISTER_IMAGE_SIZE as usize
        );
        assert_eq!(core::mem::offset_of!(UserEntryImage, ss), 152);
    }

    #[test]
    fn first_user_context_rejects_missing_or_too_small_kernel_stack() {
        let ctx = context(true, 0);
        assert!(matches!(
            ContextSwitchPlan::new(core::ptr::null_mut(), &ctx, 0x123000, 0),
            Err(ContextSwitchError::FirstUserRequiresKernelStack {
                stack_top: 0,
                required: POST_CALL_STACK_OFFSET,
            })
        ));
        assert!(matches!(
            ContextSwitchPlan::new(
                core::ptr::null_mut(),
                &ctx,
                0x123000,
                POST_CALL_STACK_OFFSET - 1
            ),
            Err(ContextSwitchError::FirstUserRequiresKernelStack { .. })
        ));
    }
}
