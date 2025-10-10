//! Context switching implementation for Fullerene OS

use crate::process::ProcessContext;

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
            "mov [{0} + 0*8], rax",
            "mov [{0} + 1*8], rbx",
            "mov [{0} + 2*8], rcx",
            "mov [{0} + 3*8], rdx",
            "mov [{0} + 4*8], rsi",
            "mov [{0} + 5*8], rdi",
            "mov [{0} + 6*8], rbp",
            "mov [{0} + 7*8], rsp",
            "mov [{0} + 8*8], r8",
            "mov [{0} + 9*8], r9",
            "mov [{0} + 10*8], r10",
            "mov [{0} + 11*8], r11",
            "mov [{0} + 12*8], r12",
            "mov [{0} + 13*8], r13",
            "mov [{0} + 14*8], r14",
            "mov [{0} + 15*8], r15",
            // Save RFLAGS
            "pushfq",
            "pop rax",
            "mov [{0} + 16*8], rax",
            // Save RIP (return address)
            "mov rax, [rsp]",
            "mov [{0} + 17*8], rax",
            // Save segment registers
            "mov [{0} + 18*8], cs",
            "mov [{0} + 19*8], ss",
            "mov [{0} + 20*8], ds",
            "mov [{0} + 21*8], es",
            "mov [{0} + 22*8], fs",
            "mov [{0} + 23*8], gs",
            in(reg) old_ctx,
            out("rax") _,
        );
    }

    // Restore new context
    unsafe {
        core::arch::asm!(
            // Restore all general purpose registers
            "mov rax, [{0} + 0*8]",
            "mov rbx, [{0} + 1*8]",
            "mov rcx, [{0} + 2*8]",
            "mov rdx, [{0} + 3*8]",
            "mov rsi, [{0} + 4*8]",
            "mov rdi, [{0} + 5*8]",
            "mov rbp, [{0} + 6*8]",
            "mov rsp, [{0} + 7*8]",
            "mov r8, [{0} + 8*8]",
            "mov r9, [{0} + 9*8]",
            "mov r10, [{0} + 10*8]",
            "mov r11, [{0} + 11*8]",
            "mov r12, [{0} + 12*8]",
            "mov r13, [{0} + 13*8]",
            "mov r14, [{0} + 14*8]",
            "mov r15, [{0} + 15*8]",
            // Restore RFLAGS
            "mov rax, [{0} + 16*8]",
            "push rax",
            "popfq",
            // Restore RIP (return to new process)
            "mov rax, [{0} + 17*8]",
            "jmp rax",
            in(reg) new_context,
            out("rax") _
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
