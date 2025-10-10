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
    // Since we can't directly modify the stack pointer in Rust,
    // this needs to be done in assembly

    // TODO: Implement proper context switching
    // For now, this is a placeholder that doesn't actually switch contexts
    // due to inline assembly limitations with registers used by LLVM

    // Just update context pointers without actual register manipulation
    if let Some(old_ctx) = old_context {
        // In a real implementation, we would save current register state here
        // For now, we'll just mark that we don't implement this yet
        *old_ctx = ProcessContext::default();
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
