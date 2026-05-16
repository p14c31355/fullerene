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
        // Save RIP, RFLAGS
        "mov rax, [rsp]",
        "mov [rdi + 128], rax", // rip
        "pushfq",
        "pop rax",
        "mov [rdi + 136], rax", // rflags
        // Save Segments (segments[0..5])
        "mov ax, cs",
        "movzx rax, ax",
        "mov [rdi + 144], rax",
        "mov ax, ss",
        "movzx rax, ax",
        "mov [rdi + 152], rax",
        "mov ax, ds",
        "movzx rax, ax",
        "mov [rdi + 160], rax",
        "mov ax, es",
        "movzx rax, ax",
        "mov [rdi + 168], rax",
        "mov ax, fs",
        "movzx rax, ax",
        "mov [rdi + 176], rax",
        "mov ax, gs",
        "movzx rax, ax",
        "mov [rdi + 184], rax",
        "2:",
        // Restore: rsi = new_context
        "mov r15, rsi",
        "mov rsp, [rsi + 56]", // regs[7] = rsp
        // Restore GPRs
        "mov rax, [r15 + 0]",
        "mov rbx, [r15 + 8]",
        "mov rcx, [r15 + 16]",
        "mov rdx, [r15 + 24]",
        "mov rsi, [r15 + 32]",
        "mov rdi, [r15 + 40]",
        "mov rbp, [r15 + 48]",
        "mov r8, [r15 + 64]",
        "mov r9, [r15 + 72]",
        "mov r10, [r15 + 80]",
        "mov r11, [r15 + 88]",
        "mov r12, [r15 + 96]",
        "mov r13, [r15 + 104]",
        "mov r14, [r15 + 112]",
        "mov r15, [r15 + 120]",
        // Restore Segments (ds, es, fs, gs)
        "mov rax, [rsi + 160]",
        "mov ds, ax",
        "mov rax, [rsi + 168]",
        "mov es, ax",
        "mov rax, [rsi + 176]",
        "mov fs, ax",
        "mov rax, [rsi + 184]",
        "mov gs, ax",
        // Check is_user (offset 192 + 8 + 8 + 48 = 256? No, let's calculate)
        // ProcessContext: regs(128) + rflags(8) + rip(8) + segments(48) + tss(8) = 200
        // is_user is at offset 200.
        "movzx rax, byte ptr [rsi + 200]",
        "test rax, rax",
        "jz 1f",
        // User mode: iretq frame (SS, RSP, RFLAGS, CS, RIP)
        "mov rax, [rsi + 152]", // ss
        "push rax",
        "mov rax, [rsi + 56]", // rsp
        "push rax",
        "mov rax, [rsi + 136]", // rflags
        "push rax",
        "mov rax, [rsi + 144]", // cs
        "push rax",
        "mov rax, [rsi + 128]", // rip
        "push rax",
        "iretq",
        "1:",
        // Kernel mode
        "mov rax, [rsi + 136]", // rflags
        "push rax",
        "popfq",
        "mov rax, [rsi + 128]", // rip
        "jmp rax",
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

// Helper macro for easier context switching calls
#[macro_export]
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
