//! Context switching implementation for Fullerene OS

use crate::process::ProcessContext;

/// Offsets for ProcessContext fields in the assembly code
struct ContextOffsets;

/// Context field offsets (in bytes, assuming 8-byte alignment)
impl ContextOffsets {
    const RAX: usize = 0;
    const RBX: usize = 1 * 8;
    const RCX: usize = 2 * 8;
    const RDX: usize = 3 * 8;
    const RSI: usize = 4 * 8;
    const RDI: usize = 5 * 8;
    const RBP: usize = 6 * 8;
    const RSP: usize = 7 * 8;
    const R8: usize = 8 * 8;
    const R9: usize = 9 * 8;
    const R10: usize = 10 * 8;
    const R11: usize = 11 * 8;
    const R12: usize = 12 * 8;
    const R13: usize = 13 * 8;
    const R14: usize = 14 * 8;
    const R15: usize = 15 * 8;
    const RFLAGS: usize = 16 * 8;
    const RIP: usize = 17 * 8;
    const CS: usize = 18 * 8;
    const SS: usize = 19 * 8;
    const DS: usize = 20 * 8;
    const ES: usize = 21 * 8;
    const FS: usize = 22 * 8;
    const GS: usize = 23 * 8;
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
            "mov [{0} + {rsp}], rsp",
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
            // Save RIP (return address)
            "mov rax, [rsp]",
            "mov [{0} + {rip}], rax",
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
            "mov [{0} + {gs}], gs",
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
