//! Context switching implementation for Fullerene OS

use crate::process::ProcessContext;
use memoffset::offset_of;

/// Save current process context and switch to next
#[unsafe(naked)]
pub extern "C" fn switch_context(
    _old_context: Option<&mut crate::process::ProcessContext>,
    _new_context: &crate::process::ProcessContext,
) {
    core::arch::naked_asm!(
        // Entry: rdi = old_context, rsi = new_context
        "test rdi, rdi",
        "jz 2f",
        // Save GPRs (regs[0..15])
        "mov [rdi + 0], rax",
        "mov [rdi + 8], rbx",
        "mov [rdi + 16], rcx",
        "mov [rdi + 24], rdx",
        "mov [rdi + 32], rsi",
        "mov [rdi + 40], rdi",
        "mov [rdi + 48], rbp",
        "mov [rdi + 56], rsp",
        "mov [rdi + 64], r8",
        "mov [rdi + 72], r9",
        "mov [rdi + 80], r10",
        "mov [rdi + 88], r11",
        "mov [rdi + 96], r12",
        "mov [rdi + 104], r13",
        "mov [rdi + 112], r14",
        "mov [rdi + 120], r15",
        // Save RIP (at [rsp]), RFLAGS
        "mov rax, [rsp]",
        "mov [rdi + 128], rax", // rip
        "pushfq",
        "pop rax",
        "mov [rdi + 136], rax", // rflags
        // Save Segments
        "mov ax, cs; movzx rax, ax; mov [rdi + 144], rax",
        "mov ax, ss; movzx rax, ax; mov [rdi + 152], rax",
        "mov ax, ds; movzx rax, ax; mov [rdi + 160], rax",
        "mov ax, es; movzx rax, ax; mov [rdi + 168], rax",
        "mov ax, fs; movzx rax, ax; mov [rdi + 176], rax",
        "mov ax, gs; movzx rax, ax; mov [rdi + 184], rax",
        "2:",
        // Restore new_context (rsi)
        // Store new_context in a callee-saved register to avoid corruption
        "mov rbx, rsi", 
        // Restore GPRs
        "mov rax, [rbx + 0]",
        "mov rcx, [rbx + 16]",
        "mov rdx, [rbx + 24]",
        "mov rsi, [rbx + 32]",
        "mov rdi, [rbx + 40]",
        "mov rbp, [rbx + 48]",
        "mov r8, [rbx + 64]",
        "mov r9, [rbx + 72]",
        "mov r10, [rbx + 80]",
        "mov r11, [rbx + 88]",
        "mov r12, [rbx + 96]",
        "mov r13, [rbx + 104]",
        "mov r14, [rbx + 112]",
        "mov r15, [rbx + 120]",
        // Restore Segments
        "mov rax, [rbx + 160]; mov ds, ax",
        "mov rax, [rbx + 168]; mov es, ax",
        "mov rax, [rbx + 176]; mov fs, ax",
        "mov rax, [rbx + 184]; mov gs, ax",
        "mov rsp, [rbx + 56]", // restore rsp
        "mov rbx, [rbx + 8]", // restore rbx last
        "mov rax, [rsi + 200]", // is_user
        "test rax, rax",
        "jz 1f",
        // User: push frame for iretq
        "push qword ptr [rsi + 152]", // ss
        "push qword ptr [rsi + 56]", // rsp
        "push qword ptr [rsi + 136]", // rflags
        "push qword ptr [rsi + 144]", // cs
        "push qword ptr [rsi + 128]", // rip
        "iretq",
        "1:",
        // Kernel: push RFLAGS, popfq, jump RIP
        "push qword ptr [rsi + 136]",
        "popfq",
        "jmp qword ptr [rsi + 128]",
    );
}


/// Initialize context switching system
///
/// This function sets up any global state needed for context switching.
/// Currently empty but may be extended in the future.
pub fn init() {
    // No global initialization needed for basic context switching
    // Future: Set up Task State Segment, kernel stack pointers, etc.
}

/// Macro for easier context switching calls (crate-local only)
macro_rules! switch_to_process {
    ($old:expr, $new:expr) => {
        unsafe { $crate::context_switch::switch_context($old, $new) }
    };
}

#[cfg(test)]
mod tests {
    use crate::process::ProcessContext;

    #[test]
    fn test_context_default_values() {
        let ctx = ProcessContext::default();
        assert_eq!(ctx.regs[0], 0); // rax
        assert_eq!(ctx.regs[1], 0); // rbx
        assert_eq!(ctx.rflags, 0x0202); // IF flag
        // Default values are dynamic now due to GDT selectors
        // Just check that they're set to reasonable values
        assert!(ctx.segments[0] > 0); // cs: Kernel code segment selector
        assert!(ctx.segments[1] > 0); // ss: Kernel data segment selector or user
    }
}
