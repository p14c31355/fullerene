//! Context switching implementation for Fullerene OS.
//!
//! Rust owns the switch description and the process context layout. The
//! assembly trampoline only performs the operations that Rust cannot express:
//! changing RSP/CR3 and entering CPL3 with `iretq`.

use crate::process::{GeneralRegisters, ProcessContext, SegmentRegisters};

/// Stable input passed to the low-level context-switch trampoline.
///
/// This mirrors the `TransitionFrame` pattern in `petroleum::assembly`: Rust
/// constructs one typed frame and assembly reads fields through compile-time
/// `offset_of!` constants. No Rust enum/reference ABI crosses the boundary.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ContextSwitchFrame {
    old_context: *mut ProcessContext,
    new_context: *const ProcessContext,
    new_cr3: u64,
}

impl ContextSwitchFrame {
    const OLD_CONTEXT_OFFSET: usize = core::mem::offset_of!(Self, old_context);
    const NEW_CONTEXT_OFFSET: usize = core::mem::offset_of!(Self, new_context);
    const NEW_CR3_OFFSET: usize = core::mem::offset_of!(Self, new_cr3);

    const fn new(
        old_context: *mut ProcessContext,
        new_context: *const ProcessContext,
        new_cr3: u64,
    ) -> Self {
        Self {
            old_context,
            new_context,
            new_cr3,
        }
    }
}

const REGISTERS_OFFSET: usize = core::mem::offset_of!(ProcessContext, registers);
const RFLAGS_OFFSET: usize = core::mem::offset_of!(ProcessContext, rflags);
const RIP_OFFSET: usize = core::mem::offset_of!(ProcessContext, rip);
const SEGMENTS_OFFSET: usize = core::mem::offset_of!(ProcessContext, segments);
const KERNEL_RSP_OFFSET: usize = core::mem::offset_of!(ProcessContext, kernel_rsp);
const IS_USER_OFFSET: usize = core::mem::offset_of!(ProcessContext, is_user);

const RAX_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rax);
const RBX_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rbx);
const RCX_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rcx);
const RDX_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rdx);
const RSI_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rsi);
const RDI_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rdi);
const RBP_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rbp);
const RSP_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, rsp);
const R8_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r8);
const R9_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r9);
const R10_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r10);
const R11_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r11);
const R12_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r12);
const R13_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r13);
const R14_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r14);
const R15_OFFSET: usize = REGISTERS_OFFSET + core::mem::offset_of!(GeneralRegisters, r15);

const CS_OFFSET: usize = SEGMENTS_OFFSET + core::mem::offset_of!(SegmentRegisters, cs);
const SS_OFFSET: usize = SEGMENTS_OFFSET + core::mem::offset_of!(SegmentRegisters, ss);

static_assertions::const_assert_eq!(ContextSwitchFrame::OLD_CONTEXT_OFFSET, 0);
static_assertions::const_assert_eq!(
    ContextSwitchFrame::NEW_CONTEXT_OFFSET,
    core::mem::size_of::<usize>()
);
static_assertions::const_assert_eq!(
    ContextSwitchFrame::NEW_CR3_OFFSET,
    core::mem::size_of::<usize>() * 2
);

/// Save the current kernel continuation and switch to `new_context`.
///
/// A suspended continuation is represented by its own kernel stack. This is
/// the conventional cooperative-switch model: callee-saved registers and the
/// return address stay on that stack, so no guessed return-address/RSP pair is
/// written into a flat register array.
///
/// # Safety
///
/// Both context pointers and `new_cr3` must stay valid across the switch.
#[inline(never)]
pub unsafe extern "sysv64" fn switch_context(
    old_context: *mut ProcessContext,
    new_context: *const ProcessContext,
    new_cr3: u64,
) {
    debug_assert!(!new_context.is_null());
    debug_assert_ne!(new_cr3, 0);

    let frame = ContextSwitchFrame::new(old_context, new_context, new_cr3);
    unsafe {
        switch_context_trampoline(&frame);
    }

    // Keep a real call/return boundary. The trampoline returns here when this
    // process is selected again, potentially much later.
    core::hint::black_box(&frame);
}

/// Assembly boundary for the operations Rust cannot model.
///
/// The common resume path is intentionally small: save callee-saved registers
/// on the old kernel stack, exchange RSP/CR3, restore them, and `ret`.
#[unsafe(naked)]
unsafe extern "sysv64" fn switch_context_trampoline(_frame: *const ContextSwitchFrame) {
    core::arch::naked_asm!(
        // Preserve the original interrupt state, then make the switch atomic.
        "pushfq",
        "cli",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Resolve the typed frame before changing address spaces.
        "mov rax, [rdi + {frame_old}]",
        "mov rsi, [rdi + {frame_new}]",
        "mov rdx, [rdi + {frame_cr3}]",
        "test rax, rax",
        "jz 2f",
        "mov [rax + {kernel_rsp}], rsp",
        "2:",

        "mov rax, cr3",
        "cmp rax, rdx",
        "je 3f",
        "mov cr3, rdx",
        "3:",

        // A non-zero kernel_rsp is a suspended Rust continuation. Restoring
        // its stack and returning is both smaller and less fragile than
        // synthesizing a RIP/RSP pair in assembly.
        "mov rax, [rsi + {kernel_rsp}]",
        "test rax, rax",
        "jz 4f",
        "mov rsp, rax",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "popfq",
        "ret",

        // First entry: choose a CPL3 iret frame or a kernel entry point.
        "4:",
        "cmp byte ptr [rsi + {is_user}], 0",
        "je 5f",

        // Build the complete privilege-transition frame on the current kernel
        // stack before restoring the initial user register image.
        "mov rax, [rsi + {ss}]",
        "push rax",
        "mov rax, [rsi + {rsp}]",
        "push rax",
        "mov rax, [rsi + {rflags}]",
        "push rax",
        "mov rax, [rsi + {cs}]",
        "push rax",
        "mov rax, [rsi + {rip}]",
        "push rax",

        // R11 remains the context base until every other register is loaded.
        "mov r11, rsi",
        "mov rax, [r11 + {rax}]",
        "mov rbx, [r11 + {rbx}]",
        "mov rcx, [r11 + {rcx}]",
        "mov rdx, [r11 + {rdx}]",
        "mov rsi, [r11 + {rsi}]",
        "mov rdi, [r11 + {rdi}]",
        "mov rbp, [r11 + {rbp}]",
        "mov r8,  [r11 + {r8}]",
        "mov r9,  [r11 + {r9}]",
        "mov r10, [r11 + {r10}]",
        "mov r12, [r11 + {r12}]",
        "mov r13, [r11 + {r13}]",
        "mov r14, [r11 + {r14}]",
        "mov r15, [r11 + {r15}]",
        "mov r11, [r11 + {r11}]",
        "iretq",

        // A new kernel task starts on its allocated stack. Give it call-like
        // alignment; kernel entry functions are expected not to return.
        "5:",
        "mov rax, [rsi + {rsp}]",
        "mov rcx, [rsi + {rflags}]",
        "mov rdx, [rsi + {rip}]",
        "mov rsp, rax",
        "push 0",
        "push rcx",
        "popfq",
        "jmp rdx",

        frame_old = const ContextSwitchFrame::OLD_CONTEXT_OFFSET,
        frame_new = const ContextSwitchFrame::NEW_CONTEXT_OFFSET,
        frame_cr3 = const ContextSwitchFrame::NEW_CR3_OFFSET,
        kernel_rsp = const KERNEL_RSP_OFFSET,
        is_user = const IS_USER_OFFSET,
        rflags = const RFLAGS_OFFSET,
        rip = const RIP_OFFSET,
        cs = const CS_OFFSET,
        ss = const SS_OFFSET,
        rax = const RAX_OFFSET,
        rbx = const RBX_OFFSET,
        rcx = const RCX_OFFSET,
        rdx = const RDX_OFFSET,
        rsi = const RSI_OFFSET,
        rdi = const RDI_OFFSET,
        rbp = const RBP_OFFSET,
        rsp = const RSP_OFFSET,
        r8 = const R8_OFFSET,
        r9 = const R9_OFFSET,
        r10 = const R10_OFFSET,
        r11 = const R11_OFFSET,
        r12 = const R12_OFFSET,
        r13 = const R13_OFFSET,
        r14 = const R14_OFFSET,
        r15 = const R15_OFFSET,
    );
}

/// Initialize context switching system.
pub fn init() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_defaults_are_valid_for_kernel_entry() {
        let ctx = ProcessContext::default();
        assert_eq!(ctx.registers.rax, 0);
        assert_eq!(ctx.registers.rbx, 0);
        assert_eq!(ctx.kernel_rsp, 0);
        assert_eq!(ctx.rflags, 0x0202);
        assert!(ctx.segments.cs > 0);
        assert!(ctx.segments.ss > 0);
    }

    #[test]
    fn switch_frame_has_stable_c_layout() {
        assert_eq!(ContextSwitchFrame::OLD_CONTEXT_OFFSET, 0);
        assert_eq!(ContextSwitchFrame::NEW_CONTEXT_OFFSET, 8);
        assert_eq!(ContextSwitchFrame::NEW_CR3_OFFSET, 16);
        assert_eq!(core::mem::size_of::<ContextSwitchFrame>(), 24);
    }
}
