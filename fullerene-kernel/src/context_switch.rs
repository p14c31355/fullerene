//! Context switching implementation for Fullerene OS

use crate::process::ProcessContext;
use memoffset::offset_of;

// Generate register offset constants using macro to reduce duplicative code
macro_rules! define_register_offsets {
    ($($reg:ident),*) => {
        #[derive(Clone, Copy)]
        pub struct ContextOffsets;

        impl ContextOffsets {
            $(
                const $reg: usize = offset_of!(ProcessContext, $reg);
            )*
        }
    };
}

define_register_offsets!(
    rax, rbx, rcx, rdx, rsi, rdi, rbp, rsp, r8, r9, r10, r11, r12, r13, r14, r15, rflags, rip, cs,
    ss, ds, es, fs, gs
);

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
        // Save current context (label 2)
        "2:",
        // Save GPRs to old_context. Note that rdi holds old_context pointer.
        "mov [rdi + {rax}], rax",
        "mov [rdi + {rbx}], rbx",
        "mov [rdi + {rcx}], rcx",
        "mov [rdi + {rdx}], rdx",
        "mov [rdi + {rsi}], rsi",
        "mov [rdi + {rbp}], rbp",
        "mov [rdi + {r8}], r8",
        "mov [rdi + {r9}], r9",
        "mov [rdi + {r10}], r10",
        "mov [rdi + {r11}], r11",
        "mov [rdi + {r12}], r12",
        "mov [rdi + {r13}], r13",
        "mov [rdi + {r14}], r14",
        "mov [rdi + {r15}], r15",

        // The current rdi (old_context pointer) is saved as the process's rdi
        "mov [rdi + {rdi}], rdi",

        // Save RIP and RSP.
        "mov rax, [rsp]", // Get return address from stack.
        "mov [rdi + {rip}], rax",
        "mov [rdi + {rsp}], rsp", // Save current stack pointer.

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

        "jmp 3f",

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
        "mov rsi, [rsi + {rsi}]",

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
        rip = const ContextOffsets::rip,
        rsp = const ContextOffsets::rsp,
        rbp = const ContextOffsets::rbp,
        rflags = const ContextOffsets::rflags,
        cs = const ContextOffsets::cs,
        ss = const ContextOffsets::ss,
        ds = const ContextOffsets::ds,
        es = const ContextOffsets::es,
        fs = const ContextOffsets::fs,
        gs = const ContextOffsets::gs,
        rax = const ContextOffsets::rax,
        rbx = const ContextOffsets::rbx,
        rcx = const ContextOffsets::rcx,
        rdx = const ContextOffsets::rdx,
        rsi = const ContextOffsets::rsi,
        rdi = const ContextOffsets::rdi,
        r8 = const ContextOffsets::r8,
        r9 = const ContextOffsets::r9,
        r10 = const ContextOffsets::r10,
        r11 = const ContextOffsets::r11,
        r12 = const ContextOffsets::r12,
        r13 = const ContextOffsets::r13,
        r14 = const ContextOffsets::r14,
        r15 = const ContextOffsets::r15,
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
        assert_eq!(ctx.rax, 0);
        assert_eq!(ctx.rbx, 0);
        assert_eq!(ctx.rflags, 0x0202); // IF flag
        // Default values are dynamic now due to GDT selectors
        // Just check that they're set to reasonable values
        assert!(ctx.cs > 0); // Kernel code segment selector
        assert!(ctx.ss > 0); // Kernel data segment selector or user
    }
}
