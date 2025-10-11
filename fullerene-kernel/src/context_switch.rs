//! Context switching implementation for Fullerene OS

use crate::process::ProcessContext;
use memoffset::offset_of;

/// Offsets for ProcessContext fields in the assembly code
struct ContextOffsets;

/// Context field offsets (in bytes, assuming 8-byte alignment)
impl ContextOffsets {
    const RAX: usize = offset_of!(ProcessContext, rax);
    const RBX: usize = offset_of!(ProcessContext, rbx);
    const RCX: usize = offset_of!(ProcessContext, rcx);
    const RDX: usize = offset_of!(ProcessContext, rdx);
    const RSI: usize = offset_of!(ProcessContext, rsi);
    const RDI: usize = offset_of!(ProcessContext, rdi);
    const RBP: usize = offset_of!(ProcessContext, rbp);
    const RSP: usize = offset_of!(ProcessContext, rsp);
    const R8: usize = offset_of!(ProcessContext, r8);
    const R9: usize = offset_of!(ProcessContext, r9);
    const R10: usize = offset_of!(ProcessContext, r10);
    const R11: usize = offset_of!(ProcessContext, r11);
    const R12: usize = offset_of!(ProcessContext, r12);
    const R13: usize = offset_of!(ProcessContext, r13);
    const R14: usize = offset_of!(ProcessContext, r14);
    const R15: usize = offset_of!(ProcessContext, r15);
    const RFLAGS: usize = offset_of!(ProcessContext, rflags);
    const RIP: usize = offset_of!(ProcessContext, rip);
    const CS: usize = offset_of!(ProcessContext, cs);
    const SS: usize = offset_of!(ProcessContext, ss);
    const DS: usize = offset_of!(ProcessContext, ds);
    const ES: usize = offset_of!(ProcessContext, es);
    const FS: usize = offset_of!(ProcessContext, fs);
    const GS: usize = offset_of!(ProcessContext, gs);
}

/// Save current process context and switch to next
///
/// This function saves the current CPU state to old_context and loads
/// the CPU state from new_context, effectively switching execution
/// from one process to another.
///
/// # Safety
/// This function is unsafe because it modifies CPU state directly
/// and can only be called from interrupt context or kernel code.
///
/// # Arguments
/// * `old_context` - Where to save current context (can be null if not saving)
/// * `new_context` - Where to load new context from (must not be null)
///
/// The function uses inline assembly to save and restore CPU registers
/// in the exact order defined by ProcessContext structure.
#[inline(never)]
pub unsafe fn switch_context(
    old_context: Option<&mut crate::process::ProcessContext>,
    new_context: &crate::process::ProcessContext,
) {
    // We need to save the old context if provided, then load the new context
    // Using inline assembly to save/load all registers including RFLAGS and RIP

    if let Some(old_ctx) = old_context {
        // Save current context
        core::arch::asm!(
            // Save all general purpose registers
            "mov [{0} + {rax}], rax",
            "mov [{0} + {rbx}], rbx",
            "mov [{0} + {rcx}], rcx",
            "mov [{0} + {rdx}], rdx",
            "mov [{0} + {rsi}], rsi",
            "mov [{0} + {rdi}], rdi",
            "mov [{0} + {rbp}], rbp",
            "mov [{0} + {r8}], r8",
            "mov [{0} + {r9}], r9",
            "mov [{0} + {r10}], r10",
            "mov [{0} + {r11}], r11",
            "mov [{0} + {r12}], r12",
            "mov [{0} + {r13}], r13",
            "mov [{0} + {r14}], r14",
            "mov [{0} + {r15}], r15",
            // Save RFLAGS
            "pushfq",
            "pop rax",
            "mov [{0} + {rflags}], rax",
            // Save RIP and RSP. This assumes a stack frame with a base pointer (rbp).
            "mov rax, [rbp + 8]",      // Get return address from stack frame.
            "mov [{0} + {rip}], rax",
            "lea rax, [rbp + 16]",     // Get caller's stack pointer.
            "mov [{0} + {rsp}], rax",
            // Save segment registers
            "mov rax, cs",
            "mov [{0} + {cs}], rax",
            "mov rax, ss",
            "mov [{0} + {ss}], rax",
            "mov rax, ds",
            "mov [{0} + {ds}], rax",
            "mov rax, es",
            "mov [{0} + {es}], rax",
            "mov rax, fs",
            "mov [{0} + {fs}], rax",
            "mov rax, gs",
            "mov [{0} + {gs}], rax",
            in(reg) old_ctx,
            rax = const ContextOffsets::RAX,
            rbx = const ContextOffsets::RBX,
            rcx = const ContextOffsets::RCX,
            rdx = const ContextOffsets::RDX,
            rsi = const ContextOffsets::RSI,
            rdi = const ContextOffsets::RDI,
            rbp = const ContextOffsets::RBP,
            rsp = const ContextOffsets::RSP,
            r8 = const ContextOffsets::R8,
            r9 = const ContextOffsets::R9,
            r10 = const ContextOffsets::R10,
            r11 = const ContextOffsets::R11,
            r12 = const ContextOffsets::R12,
            r13 = const ContextOffsets::R13,
            r14 = const ContextOffsets::R14,
            r15 = const ContextOffsets::R15,
            rflags = const ContextOffsets::RFLAGS,
            rip = const ContextOffsets::RIP,
            cs = const ContextOffsets::CS,
            ss = const ContextOffsets::SS,
            ds = const ContextOffsets::DS,
            es = const ContextOffsets::ES,
            fs = const ContextOffsets::FS,
            gs = const ContextOffsets::GS,
            out("rax") _,
        );
    }

    // Restore new context
    unsafe {
        core::arch::asm!(
            // Restore all general purpose registers
            "mov rax, [{0} + {rax}]",
            "mov rbx, [{0} + {rbx}]",
            "mov rcx, [{0} + {rcx}]",
            "mov rdx, [{0} + {rdx}]",
            "mov rsi, [{0} + {rsi}]",
            "mov rdi, [{0} + {rdi}]",
            "mov rbp, [{0} + {rbp}]",
            "mov rsp, [{0} + {rsp}]",
            "mov r8, [{0} + {r8}]",
            "mov r9, [{0} + {r9}]",
            "mov r10, [{0} + {r10}]",
            "mov r11, [{0} + {r11}]",
            "mov r12, [{0} + {r12}]",
            "mov r13, [{0} + {r13}]",
            "mov r14, [{0} + {r14}]",
            "mov r15, [{0} + {r15}]",
            // Restore segment registers
            "mov rax, [{0} + {ds}]",
            "mov ds, ax",
            "mov rax, [{0} + {es}]",
            "mov es, ax",
            "mov rax, [{0} + {fs}]",
            "mov fs, ax",
            "mov rax, [{0} + {gs}]",
            "mov gs, ax",
            // Restore RFLAGS
            "mov rax, [{0} + {rflags}]",
            "push rax",
            "popfq",
            // Restore RIP (return to new process)
            "mov rax, [{0} + {rip}]",
            "jmp rax",
            in(reg) new_context,
            rax = const ContextOffsets::RAX,
            rbx = const ContextOffsets::RBX,
            rcx = const ContextOffsets::RCX,
            rdx = const ContextOffsets::RDX,
            rsi = const ContextOffsets::RSI,
            rdi = const ContextOffsets::RDI,
            rbp = const ContextOffsets::RBP,
            rsp = const ContextOffsets::RSP,
            r8 = const ContextOffsets::R8,
            r9 = const ContextOffsets::R9,
            r10 = const ContextOffsets::R10,
            r11 = const ContextOffsets::R11,
            r12 = const ContextOffsets::R12,
            r13 = const ContextOffsets::R13,
            r14 = const ContextOffsets::R14,
            r15 = const ContextOffsets::R15,
            ds = const ContextOffsets::DS,
            es = const ContextOffsets::ES,
            fs = const ContextOffsets::FS,
            gs = const ContextOffsets::GS,
            rflags = const ContextOffsets::RFLAGS,
            rip = const ContextOffsets::RIP,
            options(noreturn)
        );
    }
}

/// Initialize context switching system
///
/// This function sets up any global state needed for context switching.
/// Currently empty but may be extended in the future.
pub fn init() {
    // No global initialization needed for basic context switching
    // Future: Set up Task State Segment, kernel stack pointers, etc.
}

// Helper macro for easier context switching calls
#[macro_export]
macro_rules! switch_to_process {
    ($old:expr, $new:expr) => {
        unsafe { $crate::context_switch::switch_context($old, $new) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessContext;

    #[test]
    fn test_context_default_values() {
        let ctx = ProcessContext::default();
        assert_eq!(ctx.rax, 0);
        assert_eq!(ctx.rbx, 0);
        assert_eq!(ctx.rflags, 0x0202); // IF flag
        assert_eq!(ctx.cs, 0x08); // Kernel code segment
        assert_eq!(ctx.ss, 0x10); // Kernel data segment
    }
}
