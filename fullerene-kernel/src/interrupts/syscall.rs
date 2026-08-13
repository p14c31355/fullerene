//! System call mechanism
//!
//! This module implements the Fast System Call mechanism using SYSCALL/SYSRET instructions.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use petroleum::mem_debug;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{GsBase, KernelGsBase, Msr};
use x86_64::registers::rflags::RFlags;

/// Per-CPU syscall entry state addressed through `KERNEL_GS_BASE`.
///
/// Fullerene is currently single-core, so one entry state is sufficient.
/// `kernel_stack_top` is updated by the scheduler before entering each process;
/// it always names that process's dedicated kernel stack.
/// `user_rsp` is only scratch space until it has been copied to the kernel
/// stack; keeping it here lets the naked entry switch stacks without
/// clobbering a user register.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct SyscallEntryState {
    kernel_stack_top: u64,
    user_rsp: u64,
    syscall_number: u64,
    user_rip: u64,
    user_rflags: u64,
    user_rbx: u64,
    user_rcx: u64,
    user_rdx: u64,
    user_rsi: u64,
    user_rdi: u64,
    user_rbp: u64,
    user_r8: u64,
    user_r9: u64,
    user_r10: u64,
    user_r11: u64,
    user_r12: u64,
    user_r13: u64,
    user_r14: u64,
    user_r15: u64,
    return_override: u64,
    return_rip: u64,
    return_rsp: u64,
    return_rflags: u64,
}

/// The user return frame belonging to a syscall that was cooperatively
/// suspended before the syscall handler returned.
///
/// `SYSCALL_ENTRY_STATE` is per CPU, but a process may yield from inside a
/// syscall (the native Nozzle bridge does this while waiting for input).  The
/// CPU-global entry frame therefore has to travel with the suspended process
/// instead of being overwritten by the next process's syscall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SavedSyscallState {
    user_rsp: u64,
    syscall_number: u64,
    user_rip: u64,
    user_rflags: u64,
    user_rbx: u64,
    user_rcx: u64,
    user_rdx: u64,
    user_rsi: u64,
    user_rdi: u64,
    user_rbp: u64,
    user_r8: u64,
    user_r9: u64,
    user_r10: u64,
    user_r11: u64,
    user_r12: u64,
    user_r13: u64,
    user_r14: u64,
    user_r15: u64,
    return_override: u64,
    return_rip: u64,
    return_rsp: u64,
    return_rflags: u64,
}

impl SavedSyscallState {
    unsafe fn capture(state: *const SyscallEntryState) -> Self {
        let state = unsafe { &*state };
        Self {
            user_rsp: state.user_rsp,
            syscall_number: state.syscall_number,
            user_rip: state.user_rip,
            user_rflags: state.user_rflags,
            user_rbx: state.user_rbx,
            user_rcx: state.user_rcx,
            user_rdx: state.user_rdx,
            user_rsi: state.user_rsi,
            user_rdi: state.user_rdi,
            user_rbp: state.user_rbp,
            user_r8: state.user_r8,
            user_r9: state.user_r9,
            user_r10: state.user_r10,
            user_r11: state.user_r11,
            user_r12: state.user_r12,
            user_r13: state.user_r13,
            user_r14: state.user_r14,
            user_r15: state.user_r15,
            return_override: state.return_override,
            return_rip: state.return_rip,
            return_rsp: state.return_rsp,
            return_rflags: state.return_rflags,
        }
    }

    unsafe fn restore(self, state: *mut SyscallEntryState) {
        let state = unsafe { &mut *state };
        state.user_rsp = self.user_rsp;
        state.syscall_number = self.syscall_number;
        state.user_rip = self.user_rip;
        state.user_rflags = self.user_rflags;
        state.user_rbx = self.user_rbx;
        state.user_rcx = self.user_rcx;
        state.user_rdx = self.user_rdx;
        state.user_rsi = self.user_rsi;
        state.user_rdi = self.user_rdi;
        state.user_rbp = self.user_rbp;
        state.user_r8 = self.user_r8;
        state.user_r9 = self.user_r9;
        state.user_r10 = self.user_r10;
        state.user_r11 = self.user_r11;
        state.user_r12 = self.user_r12;
        state.user_r13 = self.user_r13;
        state.user_r14 = self.user_r14;
        state.user_r15 = self.user_r15;
        state.return_override = self.return_override;
        state.return_rip = self.return_rip;
        state.return_rsp = self.return_rsp;
        state.return_rflags = self.return_rflags;
    }
}

const _: () = {
    assert!(core::mem::offset_of!(SyscallEntryState, kernel_stack_top) == 0);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rsp) == 8);
    assert!(core::mem::offset_of!(SyscallEntryState, syscall_number) == 16);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rip) == 24);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rflags) == 32);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rbx) == 40);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rcx) == 48);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rdx) == 56);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rsi) == 64);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rdi) == 72);
    assert!(core::mem::offset_of!(SyscallEntryState, user_rbp) == 80);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r8) == 88);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r9) == 96);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r10) == 104);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r11) == 112);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r12) == 120);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r13) == 128);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r14) == 136);
    assert!(core::mem::offset_of!(SyscallEntryState, user_r15) == 144);
    assert!(core::mem::offset_of!(SyscallEntryState, return_override) == 152);
    assert!(core::mem::offset_of!(SyscallEntryState, return_rip) == 160);
    assert!(core::mem::offset_of!(SyscallEntryState, return_rsp) == 168);
    assert!(core::mem::offset_of!(SyscallEntryState, return_rflags) == 176);
};

static mut SYSCALL_ENTRY_STATE: SyscallEntryState = SyscallEntryState {
    kernel_stack_top: 0,
    user_rsp: 0,
    syscall_number: 0,
    user_rip: 0,
    user_rflags: 0,
    user_rbx: 0,
    user_rcx: 0,
    user_rdx: 0,
    user_rsi: 0,
    user_rdi: 0,
    user_rbp: 0,
    user_r8: 0,
    user_r9: 0,
    user_r10: 0,
    user_r11: 0,
    user_r12: 0,
    user_r13: 0,
    user_r14: 0,
    user_r15: 0,
    return_override: 0,
    return_rip: 0,
    return_rsp: 0,
    return_rflags: 0,
};

/// PID whose real hardware syscall is currently executing.  Direct
/// `kernel_syscall()` calls from kernel compatibility code do not set this;
/// only the SYSCALL entry wrapper does.
static ACTIVE_SYSCALL_PID: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn begin_syscall(pid: Option<crate::process::ProcessId>) {
    ACTIVE_SYSCALL_PID.store(pid.map_or(0, |pid| pid.0 as usize), Ordering::Release);
}

pub(crate) fn end_syscall(pid: Option<crate::process::ProcessId>) {
    let pid = pid.map_or(0, |pid| pid.0 as usize);
    let _ = ACTIVE_SYSCALL_PID.compare_exchange(pid, 0, Ordering::AcqRel, Ordering::Acquire);
}

/// Move the active hardware syscall frame into the outgoing process before a
/// cooperative context switch can let another process use the CPU-global
/// SYSCALL entry state.
pub(crate) fn save_context_for_switch(pid: crate::process::ProcessId) {
    if ACTIVE_SYSCALL_PID.load(Ordering::Acquire) != pid.0 as usize {
        return;
    }
    let saved = unsafe { SavedSyscallState::capture(core::ptr::addr_of!(SYSCALL_ENTRY_STATE)) };
    if crate::process::SCHEDULER
        .with_process(pid, |process| process.syscall_state = Some(saved))
        .is_some()
    {
        ACTIVE_SYSCALL_PID.store(0, Ordering::Release);
    }
}

/// Restore a syscall frame belonging to a process resumed inside its syscall.
pub(crate) fn restore_context_for_switch(pid: crate::process::ProcessId) {
    let saved = crate::process::SCHEDULER
        .with_process(pid, |process| process.syscall_state.take())
        .flatten();
    if let Some(saved) = saved {
        unsafe { saved.restore(core::ptr::addr_of_mut!(SYSCALL_ENTRY_STATE)) };
        ACTIVE_SYSCALL_PID.store(pid.0 as usize, Ordering::Release);
    } else {
        ACTIVE_SYSCALL_PID.store(0, Ordering::Release);
    }
}

static FIRST_LINUX_SYSCALL_ENTRY_DIAG: AtomicBool = AtomicBool::new(false);

/// Mark the first Linux SYSCALL instruction before entering Rust dispatch.
///
/// The normal dispatcher marker is intentionally later than this checkpoint.
/// Keeping both lets the Klog Live trace distinguish an `iretq`/user-entry
/// failure from a fault in the SYSCALL trampoline itself.
#[inline(never)]
extern "sysv64" fn syscall_entry_checkpoint() {
    let is_linux = crate::process::current_pid()
        .and_then(|pid| {
            crate::process::SCHEDULER.with_process(pid, |p| {
                matches!(
                    p.dispatch_mode,
                    Some(crate::solvent_linux::DispatchMode::Linux(_))
                )
            })
        })
        .unwrap_or(false);
    if is_linux
        && FIRST_LINUX_SYSCALL_ENTRY_DIAG
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        crate::klog_fmt!("[CTX-DIAG] first Linux SYSCALL entry reached\n");
        solvent::mark_klog_live_dirty();
        solvent::flush_frame_no_fb();
    }
}

/// Initialize the per-CPU SYSCALL entry state.
///
/// No stack is allocated here. User processes receive a dedicated kernel
/// stack in `create_process`, and the scheduler installs it before CPL3 entry.
pub fn init_syscall_stack() {
    mem_debug!("Syscall: init_syscall_stack start\n");
    unsafe {
        SYSCALL_ENTRY_STATE.kernel_stack_top = 0;
    }
    mem_debug!("Syscall: init_syscall_stack done\n");
}

/// Install the kernel stack for the process that is about to run.
///
/// This must be called before switching to a user context so both SYSCALL and
/// interrupt privilege transitions land on the same process-owned stack.
pub fn set_process_kernel_stack(stack_top: VirtAddr) {
    debug_assert_ne!(stack_top.as_u64(), 0);
    unsafe {
        SYSCALL_ENTRY_STATE.kernel_stack_top = stack_top.as_u64();
    }
    crate::gdt::set_ring0_stack(stack_top);
}

/// Restore the MSR arrangement expected by a fresh CPL3 entry.
///
/// A Linux task normally returns through `sysret`, which executes `swapgs`
/// before resuming user mode.  Fullerene can instead terminate a task from
/// inside its syscall and context-switch directly to the shell.  In that
/// path the CPU is still using the kernel GS base when the next task is
/// entered, so the next user's `swapgs` would exchange in a zero GS base and
/// fault before the first syscall.  Make the transition state explicit.
pub fn prepare_user_entry() {
    GsBase::write(VirtAddr::new(0));
    KernelGsBase::write(VirtAddr::new(
        &raw const SYSCALL_ENTRY_STATE as *const _ as u64,
    ));
}

/// Restore the GS swap state for a task suspended inside `syscall_entry`.
///
/// A task that is entered through `iretq` starts with user GS active and the
/// syscall state in `KERNEL_GS_BASE`.  A task resumed after its syscall-entry
/// `swapgs`, however, is already in the kernel half of that exchange.  A raw
/// context switch does not restore MSRs, so make that state explicit before
/// returning to the suspended assembly continuation.
pub fn prepare_kernel_continuation() {
    GsBase::write(VirtAddr::new(
        &raw const SYSCALL_ENTRY_STATE as *const _ as u64,
    ));
    KernelGsBase::write(VirtAddr::new(0));
}

/// Return the user return frame for the syscall currently being dispatched.
///
/// This is consumed by Linux `clone`: a child created from a process that has
/// already yielded cannot reuse the parent's original first-entry context.
/// The syscall entry state is single-CPU state and is valid until the current
/// syscall handler returns or switches away from this task.
pub fn current_user_return_context() -> (crate::process::GeneralRegisters, u64, u64) {
    unsafe {
        (
            crate::process::GeneralRegisters {
                rax: 0,
                rbx: SYSCALL_ENTRY_STATE.user_rbx,
                // SYSRET clobbers RCX/R11; the child must start with a
                // deterministic value for these syscall-clobbered registers.
                rcx: 0,
                rdx: SYSCALL_ENTRY_STATE.user_rdx,
                rsi: SYSCALL_ENTRY_STATE.user_rsi,
                rdi: SYSCALL_ENTRY_STATE.user_rdi,
                rbp: SYSCALL_ENTRY_STATE.user_rbp,
                rsp: SYSCALL_ENTRY_STATE.user_rsp,
                r8: SYSCALL_ENTRY_STATE.user_r8,
                r9: SYSCALL_ENTRY_STATE.user_r9,
                r10: SYSCALL_ENTRY_STATE.user_r10,
                r11: 0,
                r12: SYSCALL_ENTRY_STATE.user_r12,
                r13: SYSCALL_ENTRY_STATE.user_r13,
                r14: SYSCALL_ENTRY_STATE.user_r14,
                r15: SYSCALL_ENTRY_STATE.user_r15,
            },
            SYSCALL_ENTRY_STATE.user_rip,
            SYSCALL_ENTRY_STATE.user_rflags,
        )
    }
}

/// Replace the return frame for a successful Linux execve. The current
/// syscall must return directly into the newly loaded image; returning to the
/// old libc call would use a stack that execve has just replaced.
pub fn override_user_return_context(rip: u64, rsp: u64, rflags: u64) {
    unsafe {
        SYSCALL_ENTRY_STATE.return_rip = rip;
        SYSCALL_ENTRY_STATE.return_rsp = rsp;
        SYSCALL_ENTRY_STATE.return_rflags = rflags;
        SYSCALL_ENTRY_STATE.return_override = 1;
    }
}

/// System call entry point (naked function for manual assembly handling)
#[unsafe(naked)]
pub extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // SYSCALL leaves the user stack in RSP.  Save it before switching to
        // the trusted kernel stack selected through KERNEL_GS_BASE.
        "swapgs",
        "mov gs:[16], rax",
        "mov gs:[8], rsp",
        // SYSCALL overwrites RCX with the return RIP and R11 with RFLAGS.
        // These slots intentionally do not contain caller values; the return
        // context exposes zero for both registers.
        "mov gs:[24], rcx",
        "mov gs:[32], r11",
        "mov gs:[40], rbx",
        "mov gs:[48], rcx",
        "mov gs:[56], rdx",
        "mov gs:[64], rsi",
        "mov gs:[72], rdi",
        "mov gs:[80], rbp",
        "mov gs:[88], r8",
        "mov gs:[96], r9",
        "mov gs:[104], r10",
        "mov gs:[112], r11",
        "mov gs:[120], r12",
        "mov gs:[128], r13",
        "mov gs:[136], r14",
        "mov gs:[144], r15",
        "mov rsp, gs:[0]",
        // Keep the process CR3 active. Process page tables share the kernel
        // half, including this stack and the kernel heap, while retaining the
        // user half needed by copy_from_user/copy_to_user.
        // Save the SYSRET frame and all Linux syscall argument registers.
        // Linux only documents RAX, RCX, and R11 as clobbered by syscall.
        "push gs:[8]",
        "push rcx",
        "push r11",
        "push rdi",
        "push rsi",
        "push rdx",
        "push r10",
        "push r8",
        "push r9",
        // Shuffle Linux syscall ABI
        //   rax, rdi, rsi, rdx, r10, r8, r9
        // into the SysV C ABI used by
        //   handle_syscall(nr, a1, a2, a3, a4, a5, a6).
        // The seventh C argument is placed on the stack.  Ten pushes from an
        // aligned kernel-stack top leave RSP correctly aligned before CALL.
        "push r9",
        // The user frame is now fully saved. Checkpoint the SYSCALL entry
        // before register shuffling, then reload every value needed below;
        // the Rust call is allowed to clobber caller-saved registers.
        "call {syscall_entry_checkpoint}",
        "mov r9, [rsp + 8]",
        "mov r8, [rsp + 16]",
        "mov r10, [rsp + 24]",
        "mov rdx, [rsp + 32]",
        "mov rsi, [rsp + 40]",
        "mov rdi, [rsp + 48]",
        "mov r11, [rsp + 56]",
        "mov rcx, [rsp + 64]",
        "mov r9, r8",
        "mov r8, r10",
        "mov rcx, rdx",
        "mov rdx, rsi",
        "mov rsi, rdi",
        "mov rdi, gs:[16]",
        "call handle_syscall",
        // Discard the seventh C argument, restore the user-visible argument
        // registers, then restore the SYSRET frame.
        "add rsp, 8",
        "pop r9",
        "pop r8",
        "pop r10",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop r11",
        "pop rcx",
        "cmp qword ptr gs:[152], 0",
        "jne 1f",
        "pop qword ptr gs:[8]",
        // RAX remains the syscall result.
        "mov rsp, gs:[8]",
        "swapgs",
        "sysretq",
        "1:",
        "add rsp, 8",
        // execve starts a fresh image; do not leak the old syscall caller's
        // register image into its process-entry ABI.
        "xor eax, eax",
        "xor ebx, ebx",
        "xor edx, edx",
        "xor esi, esi",
        "xor edi, edi",
        "xor ebp, ebp",
        "xor r8d, r8d",
        "xor r9d, r9d",
        "xor r10d, r10d",
        "xor r12d, r12d",
        "xor r13d, r13d",
        "xor r14d, r14d",
        "xor r15d, r15d",
        "mov rcx, gs:[160]",
        "mov rsp, gs:[168]",
        "mov r11, gs:[176]",
        "mov qword ptr gs:[152], 0",
        "swapgs",
        "sysretq",
        syscall_entry_checkpoint = sym syscall_entry_checkpoint,
    );
}

/// Set up Fast System Call mechanism
pub fn setup_syscall() {
    mem_debug!("Syscall: setup_syscall start\n");

    // Enable SYSCALL/SYSRET and the page-table NX bit in EFER. Some UEFI
    // implementations leave NXE disabled; without it, any PTE containing
    // NO_EXECUTE (bit 63) is treated as reserved and user accesses fail with
    // a page-fault error code containing RSVD (0x8).
    mem_debug!("Syscall: writing EFER\n");
    unsafe {
        let current = Msr::new(0xC0000080).read();
        const EFER_SCE: u64 = 1 << 0;
        const EFER_NXE: u64 = 1 << 11;
        Msr::new(0xC0000080).write(current | EFER_SCE | EFER_NXE);
    }
    mem_debug!("Syscall: EFER written\n");

    // Set LSTAR MSR to syscall entry point
    let entry_addr = syscall_entry as *const () as u64;
    mem_debug!("Syscall: writing LSTAR\n");
    unsafe {
        Msr::new(0xC0000082).write(entry_addr);
    }
    mem_debug!("Syscall: LSTAR written\n");

    // Set STAR MSR for CS/SS switching
    // Use fallback selectors if GDT not yet fully initialized
    // SYSRET adds 16 to the user selector stored in STAR for CS and uses the
    // next selector down (+8) for SS.  Our GDT stores user data at 0x1b and
    // user code at 0x23, so STAR must contain 0x13; writing 0x23 directly
    // would select the TSS descriptors (0x2b/0x33) and raise #GP on return.
    let user_cs = crate::gdt::user_code_selector_checked()
        .0
        .checked_sub(16)
        .expect("user code selector must leave room for SYSRET") as u64;
    let kernel_cs = crate::gdt::code_selector_checked().0 as u64;
    let star_value = (user_cs << 48) | (kernel_cs << 32);
    mem_debug!("Syscall: writing STAR\n");
    unsafe {
        Msr::new(0xC0000081).write(star_value);
    }
    mem_debug!("Syscall: STAR written\n");

    // Mask RFLAGS during syscall
    mem_debug!("Syscall: writing SFMASK\n");
    unsafe {
        Msr::new(0xC0000084).write(RFlags::INTERRUPT_FLAG.bits() | RFlags::TRAP_FLAG.bits());
    }
    mem_debug!("Syscall: SFMASK written\n");

    // Set KERNEL_GS_BASE to the entry state used by the naked assembly.
    mem_debug!("Syscall: writing KernelGsBase\n");
    KernelGsBase::write(VirtAddr::new(
        &raw const SYSCALL_ENTRY_STATE as *const _ as u64,
    ));
    mem_debug!("Syscall: KernelGsBase written\n");

    petroleum::debug_log_no_alloc!("Syscall: initialized. LSTAR: {}", entry_addr);
    mem_debug!("Syscall: setup_syscall done\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_syscall_state_round_trips_without_kernel_stack() {
        let mut source = SyscallEntryState::default();
        source.kernel_stack_top = 0x1111;
        source.user_rsp = 0x2222;
        source.syscall_number = 22;
        source.user_rip = 0x3333;
        source.user_rflags = 0x202;
        source.user_rbx = 0x4444;
        source.user_rcx = 0x5555;
        source.user_rdx = 0x6666;
        source.user_rsi = 0x7777;
        source.user_rdi = 0x8888;
        source.user_rbp = 0x9999;
        source.user_r8 = 0xaaaa;
        source.user_r9 = 0xbbbb;
        source.user_r10 = 0xcccc;
        source.user_r11 = 0xdddd;
        source.user_r12 = 0xeeee;
        source.user_r13 = 0xffff;
        source.user_r14 = 0x1234;
        source.user_r15 = 0x5678;
        source.return_override = 1;
        source.return_rip = 0x9abc;
        source.return_rsp = 0xdef0;
        source.return_rflags = 0x202;

        let saved = unsafe { SavedSyscallState::capture(&source as *const _) };
        let mut target = SyscallEntryState {
            kernel_stack_top: 0xaaaa_aaaa,
            ..SyscallEntryState::default()
        };
        unsafe { saved.restore(&mut target as *mut _) };

        let restored = unsafe { SavedSyscallState::capture(&target as *const _) };
        assert_eq!(saved, restored);
        assert_eq!(target.kernel_stack_top, 0xaaaa_aaaa);
    }
}
