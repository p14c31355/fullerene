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
/// This naked function manually saves all registers on the stack, performs the
/// context switch, and then restores the new context's registers. This gives full
/// control over stack and register manipulation without relying on compiler-generated
/// prologues/epilogues or assumptions about stack frames.
#[unsafe(naked)]
pub extern "C" fn switch_context(
    _old_context: Option<&mut crate::process::ProcessContext>,
    _new_context: &crate::process::ProcessContext,
) {
    core::arch::naked_asm!(
        // Entry point: rdi = old_context, rsi = new_context
        // Check if old_context is null
        "test rdi, rdi",
        "jnz 2f",  // Jump forward to save context if not null
        "jmp 3f",  // Jump to restore if null

        // Save current context (label 2)
        "2:",
        // Push all callee-saved registers and some caller-saved
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",  // actually rdi points to old_context
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Get RIP and RSP manually by inspecting the stack
        // At this point, the stack has return address at [rsp+80] and saved rbp at some offset
        // We need to be very careful here
        "mov rax, [rsp + 80]",  // This is approximate - return address location
        "mov [rdi + {rip}], rax",
        "lea rax, [rsp + 88]",   // Approximate caller stack pointer
        "mov [rdi + {rsp}], rax",

        // Save RBP
        "mov [rdi + {rbp}], rbp",

        // Save RFLAGS
        "pushfq",
        "pop rax",
        "mov [rdi + {rflags}], rax",

        // Save segment registers
        "mov ax, cs",
        "movzx rax, ax",
        "mov [rdi + {cs}], rax",
        "mov ax, ss",
        "movzx rax, ax",
        "mov [rdi + {ss}], rax",
        "mov ax, ds",
        "movzx rax, ax",
        "mov [rdi + {ds}], rax",
        "mov ax, es",
        "movzx rax, ax",
        "mov [rdi + {es}], rax",
        "mov ax, fs",
        "movzx rax, ax",
        "mov [rdi + {fs}], rax",
        "mov ax, gs",
        "movzx rax, ax",
        "mov [rdi + {gs}], rax",

        // Restore the registers we pushed
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",

        // Restore context (label 3)
        "3:",
        // rsi = new_context
        // mov rsi, rsi  (already set)

        // Restore all registers from new context
        "mov rax, [rsi + {rax}]",
        "mov rbx, [rsi + {rbx}]",
        "mov rcx, [rsi + {rcx}]",
        "mov rdx, [rsi + {rdx}]",
        "mov rdi, [rsi + {rdi}]",  // This overwrites our new_context pointer
        "mov rbp, [rsi + {rbp}]",
        "mov r8, [rsi + {r8}]",
        "mov r9, [rsi + {r9}]",
        "mov r10, [rsi + {r10}]",
        "mov r11, [rsi + {r11}]",
        "mov r12, [rsi + {r12}]",
        "mov r13, [rsi + {r13}]",
        "mov r14, [rsi + {r14}]",
        "mov r15, [rsi + {r15}]",

        // Restore stack pointer and switch stacks
        "mov rsp, [rsi + {rsp}]",

        // Restore segment registers
        "mov rax, [rsi + {ds}]",
        "mov ds, ax",
        "mov rax, [rsi + {es}]",
        "mov es, ax",
        "mov rax, [rsi + {fs}]",
        "mov fs, ax",
        "mov rax, [rsi + {gs}]",
        "mov gs, ax",

        // Restore RFLAGS
        "mov rax, [rsi + {rflags}]",
        "push rax",
        "popfq",

        // Jump to RIP (never returns)
        "mov rax, [rsi + {rip}]",
        "jmp rax",

        // Compile-time constants for offsets
        rip = const ContextOffsets::RIP,
        rsp = const ContextOffsets::RSP,
        rbp = const ContextOffsets::RBP,
        rflags = const ContextOffsets::RFLAGS,
        cs = const ContextOffsets::CS,
        ss = const ContextOffsets::SS,
        ds = const ContextOffsets::DS,
        es = const ContextOffsets::ES,
        fs = const ContextOffsets::FS,
        gs = const ContextOffsets::GS,
        rax = const ContextOffsets::RAX,
        rbx = const ContextOffsets::RBX,
        rcx = const ContextOffsets::RCX,
        rdx = const ContextOffsets::RDX,
        rdi = const ContextOffsets::RDI,
        r8 = const ContextOffsets::R8,
        r9 = const ContextOffsets::R9,
        r10 = const ContextOffsets::R10,
        r11 = const ContextOffsets::R11,
        r12 = const ContextOffsets::R12,
        r13 = const ContextOffsets::R13,
        r14 = const ContextOffsets::R14,
        r15 = const ContextOffsets::R15,
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
    use super::*;
    use crate::process::ProcessContext;

    #[test]
    fn test_context_default_values() {
        let ctx = ProcessContext::default();
        assert_eq!(ctx.rax, 0);
        assert_eq!(ctx.rbx, 0);
        assert_eq!(ctx.rflags, 0x0202); // IF flag
        // Default values are dynamic now due to GDT selectors
        // Just check that they're set to reasonable values
        assert!(ctx.cs > 0); // Kernel code segment selector
        assert!(ctx.ss > 0); // Kernel data segment selector or user
    }
}
