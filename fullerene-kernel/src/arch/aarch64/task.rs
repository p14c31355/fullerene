//! Saved user contexts for the first AArch64 cooperative scheduler boundary.

use super::{
    allocator, allocator::PhysicalFrameAllocator, cpu, elf, exceptions::Aarch64TrapFrame, fs, mmu,
    timer, uart, user_memory,
};

pub(crate) const MAX_TASKS: usize = 8;
const MAX_TASK_NAME: usize = 16;
const LINUX_SIGNAL_COUNT: usize = 64;
const LINUX_SIGACTION_BYTES: usize = 32;
const MAX_LINUX_SIGNAL_FRAMES: usize = 4;
pub(crate) const MAX_LINUX_SUPPLEMENTARY_GROUPS: usize = 16;
pub(crate) const LINUX_CAPABILITY_MASK: u64 = (1u64 << 41) - 1;
const ERR_NOT_SUPPORTED: u64 = (-(95i64)) as u64;
const ERR_PERMISSION_DENIED: u64 = (-(1i64)) as u64;
const ERR_NO_SUCH_PROCESS: u64 = (-(3i64)) as u64;
const ERR_FAULT: u64 = (-(14i64)) as u64;
const ERR_BAD_HANDLE: u64 = (-(104i64)) as u64;
const ERR_WOULD_BLOCK: u64 = (-(140i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OUT_OF_MEMORY: u64 = (-(12i64)) as u64;
const ERR_OVERFLOW: u64 = (-(75i64)) as u64;
const ERR_TIMED_OUT: u64 = (-(110i64)) as u64;
const ERR_INTERRUPTED: u64 = (-(4i64)) as u64;
const PROCESS_HANDLE_TAG: u64 = 1 << 63;
const PROCESS_HANDLE_OWNER_SHIFT: u64 = 32;
const PROCESS_HANDLE_GENERATION_SHIFT: u64 = 16;
const PROCESS_HANDLE_GENERATION_MASK: u64 = 0xffff;
const PROCESS_HANDLE_PID_MASK: u64 = 0xffff;
const MAX_PROCESS_CONTROLS: usize = 8;
const INIT_PID: u64 = 1;
const STACK_ADDRESS: u64 = 0x47ff_0000;
const PAGE_SIZE: u64 = 4096;
// The interpreter occupies 0x4000_0000 and the dynamic main executable is
// based at 0x4160_0000; keep anonymous/file-backed Linux mappings above both.
const DYNAMIC_MEMORY_START: u64 = 0x4202_0000;
// Keep this below mmu::USER_SPACE_END. The stack occupies the final page of
// the bounded user window, leaving the lower range available for mappings.
const DYNAMIC_MEMORY_END: u64 = STACK_ADDRESS;
// The Android linker reserves an address window and then replaces pieces of
// it with MAP_FIXED file-backed segments. Init itself has more than twenty
// DT_NEEDED libraries, so the early process needs room for many segment
// mappings rather than the original native-smoke count.
const MAX_MEMORY_MAPPINGS: usize = 128;
// Android's linker maps several ELF segments at once.  Keep the allocation
// bounded, but do not cap a single Linux mapping at the 256 KiB prototype
// limit; 4 MiB is enough for the early linker/DSO bring-up path.
const MAX_MEMORY_PAGES: usize = 1024;
const MAX_INTERPRETER_IMAGE: usize = 4 * 1024 * 1024;
const MAX_SELINUX_CONTEXT: usize = 64;
const INIT_SELINUX_CONTEXT: &[u8] = b"u:r:init:s0";

/// ABI personality selected for an AArch64 user task.
///
/// Native Fullerene syscalls and the Linux AArch64 syscall table intentionally
/// use different register-number namespaces but overlap numerically. The
/// personality is therefore process state, not a heuristic based on the
/// syscall number.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbiPersonality {
    Native,
    LinuxAarch64,
}

#[derive(Clone, Copy)]
struct UserMapping {
    base: u64,
    length: u64,
    protection: u64,
    shared: bool,
    active: bool,
}

#[derive(Clone, Copy)]
struct ProcessControlEntry {
    target_pid: u64,
    generation: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxSignalAction {
    bytes: [u8; LINUX_SIGACTION_BYTES],
}

impl LinuxSignalAction {
    const EMPTY: Self = Self {
        bytes: [0; LINUX_SIGACTION_BYTES],
    };
}

#[derive(Clone, Copy)]
struct LinuxSignalFrame {
    saved_frame: Aarch64TrapFrame,
    saved_mask: u64,
    active: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct LinuxCredentials {
    pub(crate) real_uid: u32,
    pub(crate) effective_uid: u32,
    pub(crate) saved_uid: u32,
    pub(crate) fsuid: u32,
    pub(crate) real_gid: u32,
    pub(crate) effective_gid: u32,
    pub(crate) saved_gid: u32,
    pub(crate) fsgid: u32,
    pub(crate) supplementary_groups: [u32; MAX_LINUX_SUPPLEMENTARY_GROUPS],
    pub(crate) supplementary_group_count: usize,
    pub(crate) cap_effective: u64,
    pub(crate) cap_permitted: u64,
    pub(crate) cap_inheritable: u64,
    pub(crate) cap_bounding: u64,
    pub(crate) keep_caps: bool,
    pub(crate) no_new_privs: bool,
    pub(crate) dumpable: bool,
}

impl LinuxCredentials {
    const ROOT: Self = Self {
        real_uid: 0,
        effective_uid: 0,
        saved_uid: 0,
        fsuid: 0,
        real_gid: 0,
        effective_gid: 0,
        saved_gid: 0,
        fsgid: 0,
        supplementary_groups: [0; MAX_LINUX_SUPPLEMENTARY_GROUPS],
        supplementary_group_count: 0,
        cap_effective: LINUX_CAPABILITY_MASK,
        cap_permitted: LINUX_CAPABILITY_MASK,
        cap_inheritable: 0,
        cap_bounding: LINUX_CAPABILITY_MASK,
        keep_caps: false,
        no_new_privs: false,
        dumpable: true,
    };
}

impl LinuxSignalFrame {
    const EMPTY: Self = Self {
        saved_frame: Aarch64TrapFrame {
            x: [0; 31],
            elr_el1: 0,
            spsr_el1: 0,
            sp_el0: 0,
            esr_el1: 0,
            far_el1: 0,
            tpidr_el0: 0,
        },
        saved_mask: 0,
        active: false,
    };
}

impl ProcessControlEntry {
    const EMPTY: Self = Self {
        target_pid: 0,
        generation: 0,
        active: false,
    };
}

impl UserMapping {
    const EMPTY: Self = Self {
        base: 0,
        length: 0,
        protection: 0,
        shared: false,
        active: false,
    };
}

#[derive(Clone, Copy)]
struct TaskSlot {
    frame: Aarch64TrapFrame,
    pid: u64,
    address_space: usize,
    personality: AbiPersonality,
    is_thread: bool,
    thread_detached: bool,
    name: [u8; MAX_TASK_NAME],
    name_len: usize,
    parent_pid: u64,
    supervisor_pid: u64,
    linux_pgid: u64,
    linux_sid: u64,
    linux_credentials: LinuxCredentials,
    selinux_context: [u8; MAX_SELINUX_CONTEXT],
    selinux_context_length: usize,
    selinux_exec_context: [u8; MAX_SELINUX_CONTEXT],
    selinux_exec_context_length: usize,
    wait_target: u64,
    thread_wait_target: u64,
    linux_wait: bool,
    linux_wait_status_address: u64,
    linux_clear_child_tid: u64,
    wait_event: u64,
    sleep_wait: bool,
    wait_deadline_us: u64,
    exit_status: u64,
    terminal_handle: u64,
    linux_signal_mask: u64,
    linux_pending_signals: u64,
    linux_signal_actions: [LinuxSignalAction; LINUX_SIGNAL_COUNT],
    linux_signal_frames: [LinuxSignalFrame; MAX_LINUX_SIGNAL_FRAMES],
    process_controls: [ProcessControlEntry; MAX_PROCESS_CONTROLS],
    next_process_control_generation: u16,
    mappings: [UserMapping; MAX_MEMORY_MAPPINGS],
    linux_brk: u64,
    state: TaskState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskState {
    Empty,
    Runnable,
    Blocked,
    Exited,
}

impl TaskState {
    const READY: u64 = 0;
    const RUNNING: u64 = 1;
    const BLOCKED: u64 = 2;
    const TERMINATED: u64 = 3;
}

impl TaskSlot {
    const EMPTY: Self = Self {
        frame: Aarch64TrapFrame {
            x: [0; 31],
            elr_el1: 0,
            spsr_el1: 0,
            sp_el0: 0,
            esr_el1: 0,
            far_el1: 0,
            tpidr_el0: 0,
        },
        pid: 0,
        address_space: 0,
        personality: AbiPersonality::Native,
        is_thread: false,
        thread_detached: false,
        name: [0; MAX_TASK_NAME],
        name_len: 0,
        parent_pid: 0,
        supervisor_pid: 0,
        linux_pgid: 0,
        linux_sid: 0,
        linux_credentials: LinuxCredentials::ROOT,
        selinux_context: [0; MAX_SELINUX_CONTEXT],
        selinux_context_length: 0,
        selinux_exec_context: [0; MAX_SELINUX_CONTEXT],
        selinux_exec_context_length: 0,
        wait_target: 0,
        thread_wait_target: 0,
        linux_wait: false,
        linux_wait_status_address: 0,
        linux_clear_child_tid: 0,
        wait_event: 0,
        sleep_wait: false,
        wait_deadline_us: 0,
        exit_status: 0,
        terminal_handle: 0,
        linux_signal_mask: 0,
        linux_pending_signals: 0,
        linux_signal_actions: [LinuxSignalAction::EMPTY; LINUX_SIGNAL_COUNT],
        linux_signal_frames: [LinuxSignalFrame::EMPTY; MAX_LINUX_SIGNAL_FRAMES],
        process_controls: [ProcessControlEntry::EMPTY; MAX_PROCESS_CONTROLS],
        next_process_control_generation: 1,
        mappings: [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS],
        linux_brk: 0,
        state: TaskState::Empty,
    };
}

#[derive(Clone, Copy)]
pub(crate) struct TaskSwitch {
    pub(crate) from_index: usize,
    pub(crate) to_index: usize,
    pub(crate) from_pid: u64,
    pub(crate) to_pid: u64,
}

/// A bounded cooperative scheduler over saved AArch64 user frames.
///
/// This deliberately contains no allocation or address-space policy. It now
/// owns the smallest process identity/lifecycle state needed by the native
/// syscall boundary; the generic process table will replace this bounded
/// storage without changing the saved-frame handoff contract.
pub(crate) struct Aarch64TaskScheduler {
    slots: [TaskSlot; MAX_TASKS],
    current: usize,
    next_pid_hint: u64,
}

impl Aarch64TaskScheduler {
    pub(crate) const EMPTY: Self = Self {
        slots: [TaskSlot::EMPTY; MAX_TASKS],
        current: 0,
        next_pid_hint: 1,
    };

    pub(crate) fn reset(&mut self) {
        *self = Self::EMPTY;
    }

    pub(crate) fn install(
        &mut self,
        index: usize,
        pid: u64,
        name: &[u8],
        address_space: usize,
        frame: Aarch64TrapFrame,
    ) -> bool {
        if pid == 0 || name.len() > MAX_TASK_NAME || address_space >= mmu::MAX_USER_SPACES {
            return false;
        }
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        slot.frame = frame;
        slot.pid = pid;
        slot.address_space = address_space;
        slot.personality = AbiPersonality::Native;
        slot.is_thread = false;
        slot.thread_detached = false;
        slot.name = [0; MAX_TASK_NAME];
        slot.name[..name.len()].copy_from_slice(name);
        slot.name_len = name.len();
        slot.parent_pid = 0;
        slot.supervisor_pid = 0;
        slot.linux_pgid = pid;
        slot.linux_sid = pid;
        slot.linux_credentials = LinuxCredentials::ROOT;
        slot.selinux_context = [0; MAX_SELINUX_CONTEXT];
        slot.selinux_context[..INIT_SELINUX_CONTEXT.len()].copy_from_slice(INIT_SELINUX_CONTEXT);
        slot.selinux_context_length = INIT_SELINUX_CONTEXT.len();
        slot.selinux_exec_context = [0; MAX_SELINUX_CONTEXT];
        slot.selinux_exec_context_length = 0;
        slot.wait_target = 0;
        slot.thread_wait_target = 0;
        slot.linux_clear_child_tid = 0;
        slot.wait_event = 0;
        slot.sleep_wait = false;
        slot.wait_deadline_us = 0;
        slot.exit_status = 0;
        slot.terminal_handle = 0;
        slot.linux_signal_mask = 0;
        slot.linux_pending_signals = 0;
        slot.linux_signal_actions = [LinuxSignalAction::EMPTY; LINUX_SIGNAL_COUNT];
        slot.linux_signal_frames = [LinuxSignalFrame::EMPTY; MAX_LINUX_SIGNAL_FRAMES];
        slot.process_controls = [ProcessControlEntry::EMPTY; MAX_PROCESS_CONTROLS];
        slot.next_process_control_generation = 1;
        slot.mappings = [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS];
        slot.linux_brk = 0;
        slot.state = TaskState::Runnable;
        if pid >= self.next_pid_hint {
            self.next_pid_hint = pid.checked_add(1).unwrap_or(1);
        }
        true
    }

    fn install_child(
        &mut self,
        index: usize,
        pid: u64,
        name: &[u8],
        address_space: usize,
        parent_pid: u64,
        supervisor_pid: u64,
        terminal_handle: u64,
        frame: Aarch64TrapFrame,
        personality: AbiPersonality,
    ) -> bool {
        if !self.install(index, pid, name, address_space, frame) {
            return false;
        }
        self.slots[index].parent_pid = parent_pid;
        self.slots[index].supervisor_pid = supervisor_pid;
        self.slots[index].terminal_handle = terminal_handle;
        self.slots[index].personality = personality;
        if let Some(credentials) = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == parent_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| slot.linux_credentials)
        {
            self.slots[index].linux_credentials = credentials;
        }
        let parent_selinux = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == parent_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| {
                (
                    slot.selinux_context,
                    slot.selinux_context_length,
                    slot.selinux_exec_context,
                    slot.selinux_exec_context_length,
                )
            });
        if let Some((context, context_length, exec_context, exec_context_length)) = parent_selinux {
            self.slots[index].selinux_context = context;
            self.slots[index].selinux_context_length = context_length;
            self.slots[index].selinux_exec_context = exec_context;
            self.slots[index].selinux_exec_context_length = exec_context_length;
        }
        let parent_groups = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == parent_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| (slot.linux_pgid, slot.linux_sid));
        if let Some((pgid, sid)) = parent_groups {
            self.slots[index].linux_pgid = pgid;
            self.slots[index].linux_sid = sid;
        }
        true
    }

    fn install_thread(
        &mut self,
        index: usize,
        pid: u64,
        address_space: usize,
        owner_pid: u64,
        frame: Aarch64TrapFrame,
    ) -> bool {
        if !self.install(index, pid, b"thread", address_space, frame) {
            return false;
        }
        self.slots[index].is_thread = true;
        self.slots[index].linux_clear_child_tid = 0;
        self.slots[index].parent_pid = owner_pid;
        self.slots[index].supervisor_pid = owner_pid;
        if let Some(credentials) = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == owner_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| slot.linux_credentials)
        {
            self.slots[index].linux_credentials = credentials;
        }
        let owner_selinux = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == owner_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| {
                (
                    slot.selinux_context,
                    slot.selinux_context_length,
                    slot.selinux_exec_context,
                    slot.selinux_exec_context_length,
                )
            });
        if let Some((context, context_length, exec_context, exec_context_length)) = owner_selinux {
            self.slots[index].selinux_context = context;
            self.slots[index].selinux_context_length = context_length;
            self.slots[index].selinux_exec_context = exec_context;
            self.slots[index].selinux_exec_context_length = exec_context_length;
        }
        self.slots[index].terminal_handle = self
            .slots
            .iter()
            .find(|slot| slot.pid == owner_pid && !slot.is_thread)
            .map(|slot| slot.terminal_handle)
            .unwrap_or(0);
        self.slots[index].personality = self
            .slots
            .iter()
            .find(|slot| slot.pid == owner_pid && !slot.is_thread)
            .map(|slot| slot.personality)
            .unwrap_or(AbiPersonality::Native);
        let owner_groups = self
            .slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == owner_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| (slot.linux_pgid, slot.linux_sid));
        if let Some((pgid, sid)) = owner_groups {
            self.slots[index].linux_pgid = pgid;
            self.slots[index].linux_sid = sid;
        }
        true
    }

    pub(crate) fn current_frame(&self) -> *const Aarch64TrapFrame {
        &self.slots[self.current].frame
    }

    pub(crate) fn current_index(&self) -> usize {
        self.current
    }

    pub(crate) fn current_pid(&self) -> Option<u64> {
        let slot = &self.slots[self.current];
        matches!(slot.state, TaskState::Runnable | TaskState::Blocked).then_some(slot.pid)
    }

    fn resource_owner_pid(&self) -> Option<u64> {
        let slot = &self.slots[self.current];
        if !matches!(slot.state, TaskState::Runnable | TaskState::Blocked) {
            return None;
        }
        Some(if slot.is_thread {
            slot.parent_pid
        } else {
            slot.pid
        })
    }

    fn current_linux_credentials(&self) -> Option<LinuxCredentials> {
        let owner_pid = self.resource_owner_pid()?;
        self.slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == owner_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| slot.linux_credentials)
    }

    fn set_current_linux_credentials(&mut self, credentials: LinuxCredentials) -> bool {
        let Some(owner_pid) = self.resource_owner_pid() else {
            return false;
        };
        let mut updated = false;
        for slot in &mut self.slots {
            let slot_owner = if slot.is_thread {
                slot.parent_pid
            } else {
                slot.pid
            };
            if slot_owner == owner_pid
                && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            {
                slot.linux_credentials = credentials;
                updated = true;
            }
        }
        updated
    }

    fn contains_live_pid(&self, pid: u64) -> bool {
        pid != 0
            && self.slots.iter().any(|slot| {
                slot.pid == pid && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
    }

    fn contains_live_process_pid(&self, pid: u64) -> bool {
        pid != 0
            && self.slots.iter().any(|slot| {
                !slot.is_thread
                    && slot.pid == pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
    }

    fn current_address_space(&self) -> Option<usize> {
        let slot = &self.slots[self.current];
        matches!(slot.state, TaskState::Runnable | TaskState::Blocked).then_some(slot.address_space)
    }

    pub(crate) fn current_name(&self, destination: &mut [u8]) -> usize {
        let slot = &self.slots[self.current];
        let length = slot.name_len.min(destination.len());
        destination[..length].copy_from_slice(&slot.name[..length]);
        length
    }

    /// Save the live frame and switch to the next active task, if any.
    pub(crate) fn yield_current(&mut self, live: &mut Aarch64TrapFrame) -> Option<TaskSwitch> {
        let current = self.current;
        let from_pid = self.slots[current].pid;
        self.slots[current].frame = *live;
        self.slots[current].frame.x[0] = 0;
        live.x[0] = 0;
        let next = self.next_active(current)?;
        if !mmu::activate_user_space(self.slots[next].address_space) {
            return None;
        }
        self.current = next;
        *live = self.slots[next].frame;
        Some(TaskSwitch {
            from_index: current,
            to_index: next,
            from_pid,
            to_pid: self.slots[next].pid,
        })
    }

    /// Retire the live task and switch to another active task, if any.
    ///
    /// A blocked direct parent is woken and the just-exited child is reaped
    /// after the switch. Children of a terminating non-init task are adopted
    /// by PID 1; an already-exited adopted child can wake init's wait here.
    pub(crate) fn exit_current(
        &mut self,
        live: &mut Aarch64TrapFrame,
        status: u64,
    ) -> Option<TaskSwitch> {
        let current = self.current;
        let from_pid = self.slots[current].pid;
        let is_thread = self.slots[current].is_thread;
        let thread_detached = self.slots[current].thread_detached;
        self.slots[current].frame = *live;
        self.slots[current].exit_status = status;
        self.slots[current].state = TaskState::Exited;
        if is_thread {
            fs::thread_exit(from_pid, status);
        } else {
            fs::drop_owner(from_pid);
        }
        let thread_waiters = if is_thread {
            self.wake_thread_waiters(from_pid, status)
        } else {
            0
        };
        if is_thread {
            let clear_child_tid = self.slots[current].linux_clear_child_tid;
            if clear_child_tid != 0 {
                let _ = user_memory::copy_to_user(clear_child_tid, &[0u8; 4]);
                self.wake_event(Self::linux_futex_event_key(clear_child_tid), true);
            }
        }
        let mut direct_wait = false;
        let mut linux_waiters = [(0usize, 0u64); MAX_TASKS];
        let mut linux_waiter_count = 0usize;
        if !is_thread {
            for (waiter_index, slot) in self.slots.iter_mut().enumerate() {
                if slot.state == TaskState::Blocked
                    && !slot.is_thread
                    && (slot.wait_target == from_pid || slot.wait_target == u64::MAX)
                {
                    if slot.linux_wait
                        && slot.linux_wait_status_address != 0
                        && linux_waiter_count < MAX_TASKS
                    {
                        linux_waiters[linux_waiter_count] =
                            (waiter_index, slot.linux_wait_status_address);
                        linux_waiter_count += 1;
                    }
                    slot.frame.x[0] = if slot.linux_wait { from_pid } else { status };
                    slot.wait_target = 0;
                    slot.linux_wait = false;
                    slot.linux_wait_status_address = 0;
                    slot.wait_event = 0;
                    slot.sleep_wait = false;
                    slot.wait_deadline_us = 0;
                    slot.state = TaskState::Runnable;
                    direct_wait = true;
                }
            }
        }
        for (waiter_index, status_address) in linux_waiters.iter().copied().take(linux_waiter_count)
        {
            let old_space = self.slots[current].address_space;
            let copied = mmu::activate_user_space(self.slots[waiter_index].address_space)
                && user_memory::copy_to_user(
                    status_address,
                    &(((status & 0xff) << 8) as u32).to_ne_bytes(),
                )
                .is_ok();
            let _ = mmu::activate_user_space(old_space);
            if !copied {
                self.slots[waiter_index].frame.x[0] = ERR_FAULT;
            }
        }
        let init_index = if is_thread {
            None
        } else {
            self.slots
                .iter()
                .position(|slot| slot.state != TaskState::Empty && slot.pid == INIT_PID)
        };
        let mut wait_reap_indices = [0usize; MAX_TASKS];
        let mut wait_reap_count = 0usize;
        if direct_wait {
            wait_reap_indices[wait_reap_count] = current;
            wait_reap_count += 1;
        }
        let mut init_waiter = init_index
            .filter(|index| *index != current)
            .filter(|index| self.slots[*index].state == TaskState::Blocked);
        for child_index in 0..MAX_TASKS {
            if child_index == current || self.slots[child_index].state == TaskState::Empty {
                continue;
            }
            if is_thread || self.slots[child_index].is_thread {
                continue;
            }
            if self.slots[child_index].parent_pid != from_pid {
                continue;
            }
            if from_pid != INIT_PID && init_index.is_some() {
                self.slots[child_index].parent_pid = INIT_PID;
                if self.slots[child_index].state == TaskState::Exited {
                    if let Some(init_index) = init_waiter
                        && (self.slots[init_index].wait_target == self.slots[child_index].pid
                            || self.slots[init_index].wait_target == u64::MAX)
                    {
                        init_waiter = None;
                        self.slots[init_index].frame.x[0] = if self.slots[init_index].linux_wait {
                            self.slots[child_index].pid
                        } else {
                            self.slots[child_index].exit_status
                        };
                        self.slots[init_index].wait_target = 0;
                        self.slots[init_index].linux_wait = false;
                        self.slots[init_index].linux_wait_status_address = 0;
                        self.slots[init_index].state = TaskState::Runnable;
                        if wait_reap_count < MAX_TASKS {
                            wait_reap_indices[wait_reap_count] = child_index;
                            wait_reap_count += 1;
                        }
                    }
                }
            } else {
                self.slots[child_index].parent_pid = 0;
            }
        }
        let next = self.next_active(current)?;
        if !mmu::activate_user_space(self.slots[next].address_space) {
            return None;
        }
        self.current = next;
        *live = self.slots[next].frame;
        if is_thread && (thread_detached || thread_waiters != 0) {
            self.slots[current] = TaskSlot::EMPTY;
        }
        if wait_reap_count != 0 {
            let _ = allocator::with_global(|frames| {
                for index in wait_reap_indices.iter().copied().take(wait_reap_count) {
                    let _ = self.reap_exited(index, frames);
                }
            });
        }
        Some(TaskSwitch {
            from_index: current,
            to_index: next,
            from_pid,
            to_pid: self.slots[next].pid,
        })
    }

    /// Block the current process until one of its children exits.
    fn wait_current(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        frame: &mut Aarch64TrapFrame,
        pid: u64,
    ) -> bool {
        self.wait_current_mode(frames, frame, pid, false, 0, false)
    }

    fn wait_current_mode(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        frame: &mut Aarch64TrapFrame,
        pid: u64,
        linux_result: bool,
        status_address: u64,
        nonblocking: bool,
    ) -> bool {
        if pid == 0 {
            frame.x[0] = 0;
            return true;
        }
        let current_pid = match self.current_pid() {
            Some(pid) => pid,
            None => {
                frame.x[0] = ERR_NO_SUCH_PROCESS;
                return true;
            }
        };
        let has_child = self.slots.iter().any(|slot| {
            !slot.is_thread
                && slot.parent_pid == current_pid
                && slot.state != TaskState::Empty
                && (pid == u64::MAX || slot.pid == pid)
        });
        if !has_child {
            frame.x[0] = ERR_NO_SUCH_PROCESS;
            return true;
        }
        let exited_child = self.slots.iter().position(|slot| {
            !slot.is_thread
                && slot.parent_pid == current_pid
                && slot.state == TaskState::Exited
                && (pid == u64::MAX || slot.pid == pid)
        });
        if let Some(child_index) = exited_child {
            let child_pid = self.slots[child_index].pid;
            let status = self.slots[child_index].exit_status;
            if self.reap_exited(child_index, frames).is_err() {
                frame.x[0] = ERR_OUT_OF_MEMORY;
            } else {
                if linux_result && status_address != 0 {
                    let linux_status = ((status & 0xff) << 8) as u32;
                    if user_memory::copy_to_user(status_address, &linux_status.to_ne_bytes())
                        .is_err()
                    {
                        frame.x[0] = (-(14i64)) as u64;
                        return true;
                    }
                }
                frame.x[0] = if linux_result { child_pid } else { status };
            }
            return true;
        }

        if nonblocking {
            frame.x[0] = 0;
            return true;
        }

        let current = self.current;
        self.slots[current].frame = *frame;
        self.slots[current].wait_target = pid;
        self.slots[current].linux_wait = linux_result;
        self.slots[current].linux_wait_status_address =
            if linux_result { status_address } else { 0 };
        self.slots[current].wait_event = 0;
        self.slots[current].sleep_wait = false;
        self.slots[current].wait_deadline_us = 0;
        self.slots[current].state = TaskState::Blocked;
        let Some(next) = self.next_active(current) else {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_target = 0;
            self.slots[current].linux_wait = false;
            self.slots[current].linux_wait_status_address = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        };
        if !mmu::activate_user_space(self.slots[next].address_space) {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_target = 0;
            self.slots[current].linux_wait = false;
            self.slots[current].linux_wait_status_address = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        }
        self.current = next;
        *frame = self.slots[next].frame;
        uart::put_hex("user-smoke: wait task=", current as u64);
        uart::put_hex("user-smoke: wait switch to=", next as u64);
        true
    }

    fn block_thread(&mut self, frame: &mut Aarch64TrapFrame, target_pid: u64) -> bool {
        let current = self.current;
        self.slots[current].frame = *frame;
        self.slots[current].thread_wait_target = target_pid;
        self.slots[current].wait_target = 0;
        self.slots[current].wait_event = 0;
        self.slots[current].sleep_wait = false;
        self.slots[current].wait_deadline_us = 0;
        self.slots[current].state = TaskState::Blocked;
        let Some(next) = self.next_active(current) else {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].thread_wait_target = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        };
        if !mmu::activate_user_space(self.slots[next].address_space) {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].thread_wait_target = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        }
        self.current = next;
        *frame = self.slots[next].frame;
        uart::put_hex("user-smoke: thread join task=", current as u64);
        uart::put_hex("user-smoke: thread join switch to=", next as u64);
        true
    }

    fn wake_thread_waiters(&mut self, target_pid: u64, status: u64) -> usize {
        let mut woken = 0;
        for slot in &mut self.slots {
            if slot.state != TaskState::Blocked || slot.thread_wait_target != target_pid {
                continue;
            }
            slot.frame.x[0] = status;
            slot.thread_wait_target = 0;
            slot.state = TaskState::Runnable;
            woken += 1;
        }
        woken
    }

    fn create_thread(
        &mut self,
        live: &Aarch64TrapFrame,
        entry: u64,
        stack: u64,
    ) -> Result<u64, u64> {
        if entry < 0x4000_0000
            || entry >= 0x1_0000_0000
            || stack < 0x4000_0000
            || stack >= 0x1_0000_0000
            || stack & 0xf != 0
        {
            return Err(ERR_INVALID);
        }
        let parent_pid = self.resource_owner_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        let owner_index = self.current;
        let index = self.free_slot().ok_or(ERR_OUT_OF_MEMORY)?;
        let pid = self.next_pid().ok_or(ERR_OUT_OF_MEMORY)?;
        let mut frame = *live;
        frame.x = [0; 31];
        frame.elr_el1 = entry;
        frame.sp_el0 = stack;
        frame.esr_el1 = 0;
        frame.far_el1 = 0;
        if !self.install_thread(
            index,
            pid,
            self.slots[owner_index].address_space,
            parent_pid,
            frame,
        ) {
            return Err(ERR_OUT_OF_MEMORY);
        }
        self.slots[index].mappings = self.slots[owner_index].mappings;
        let handle = match fs::thread_handle_create(parent_pid, pid) {
            Ok(handle) => handle,
            Err(error) => {
                self.slots[index] = TaskSlot::EMPTY;
                return Err(error);
            }
        };
        Ok(handle)
    }

    fn create_linux_thread(
        &mut self,
        live: &Aarch64TrapFrame,
        stack: u64,
        tls: u64,
        clear_child_tid: u64,
    ) -> Result<u64, u64> {
        if stack < 0x4000_0000 || stack >= 0x1_0000_0000 || stack & 0xf != 0 {
            return Err(ERR_INVALID);
        }
        let owner_pid = self.resource_owner_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        let owner_index = self.current;
        let index = self.free_slot().ok_or(ERR_OUT_OF_MEMORY)?;
        let pid = self.next_pid().ok_or(ERR_OUT_OF_MEMORY)?;
        let mut frame = *live;
        frame.x[0] = 0;
        frame.sp_el0 = stack;
        frame.tpidr_el0 = tls;
        frame.esr_el1 = 0;
        frame.far_el1 = 0;
        if !self.install_thread(
            index,
            pid,
            self.slots[owner_index].address_space,
            owner_pid,
            frame,
        ) {
            return Err(ERR_OUT_OF_MEMORY);
        }
        self.slots[index].linux_signal_mask = self.slots[owner_index].linux_signal_mask;
        self.slots[index].linux_signal_actions = self.slots[owner_index].linux_signal_actions;
        // Linux pthread_join uses CLONE_CHILD_CLEARTID plus a futex rather
        // than the native Fullerene thread handle, so these threads are
        // detached from the bounded native handle table.
        self.slots[index].thread_detached = true;
        self.slots[index].linux_clear_child_tid = clear_child_tid;
        self.slots[index].mappings = self.slots[owner_index].mappings;
        Ok(pid)
    }

    fn join_thread(&mut self, frame: &mut Aarch64TrapFrame, target_pid: u64) -> bool {
        let Some(index) = self.slots.iter().position(|slot| {
            slot.pid == target_pid && slot.state != TaskState::Empty && slot.is_thread
        }) else {
            frame.x[0] = ERR_NO_SUCH_PROCESS;
            return true;
        };
        if index == self.current || self.slots[index].thread_detached {
            frame.x[0] = ERR_INVALID;
            return true;
        }
        if self.slots[index].state == TaskState::Exited {
            let status = self.slots[index].exit_status;
            self.slots[index] = TaskSlot::EMPTY;
            frame.x[0] = status;
            return true;
        }
        self.block_thread(frame, target_pid)
    }

    fn mark_thread_detached(&mut self, target_pid: u64) -> bool {
        let Some(index) = self.slots.iter().position(|slot| {
            slot.pid == target_pid && slot.state != TaskState::Empty && slot.is_thread
        }) else {
            return false;
        };
        self.slots[index].thread_detached = true;
        true
    }

    fn reap_thread_if_exited(&mut self, target_pid: u64) -> Option<u64> {
        let index = self.slots.iter().position(|slot| {
            slot.pid == target_pid && slot.state == TaskState::Exited && slot.is_thread
        })?;
        let status = self.slots[index].exit_status;
        self.slots[index] = TaskSlot::EMPTY;
        Some(status)
    }

    fn block_event(
        &mut self,
        frame: &mut Aarch64TrapFrame,
        event_slot: u64,
        timeout_us: u64,
    ) -> bool {
        let current = self.current;
        self.slots[current].frame = *frame;
        self.slots[current].wait_target = 0;
        self.slots[current].wait_event = event_slot;
        self.slots[current].sleep_wait = false;
        self.slots[current].wait_deadline_us = timer::uptime_us().saturating_add(timeout_us);
        self.slots[current].state = TaskState::Blocked;
        let Some(next) = self.next_active(current) else {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_event = 0;
            self.slots[current].sleep_wait = false;
            self.slots[current].wait_deadline_us = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        };
        if !mmu::activate_user_space(self.slots[next].address_space) {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_event = 0;
            self.slots[current].sleep_wait = false;
            self.slots[current].wait_deadline_us = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        }
        self.current = next;
        *frame = self.slots[next].frame;
        uart::put_hex("user-smoke: event wait task=", current as u64);
        uart::put_hex("user-smoke: event wait switch to=", next as u64);
        true
    }

    fn block_sleep(&mut self, frame: &mut Aarch64TrapFrame, duration_us: u64) -> bool {
        let current = self.current;
        self.slots[current].frame = *frame;
        self.slots[current].wait_target = 0;
        self.slots[current].wait_event = 0;
        self.slots[current].sleep_wait = true;
        self.slots[current].wait_deadline_us = timer::uptime_us().saturating_add(duration_us);
        self.slots[current].state = TaskState::Blocked;
        let Some(next) = self.next_active(current) else {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].sleep_wait = false;
            self.slots[current].wait_deadline_us = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        };
        if !mmu::activate_user_space(self.slots[next].address_space) {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].sleep_wait = false;
            self.slots[current].wait_deadline_us = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        }
        self.current = next;
        *frame = self.slots[next].frame;
        uart::put_hex("user-smoke: sleep task=", current as u64);
        uart::put_hex("user-smoke: sleep switch to=", next as u64);
        true
    }

    fn wake_event(&mut self, event_slot: u64, manual_reset: bool) -> usize {
        self.wake_event_count(event_slot, if manual_reset { usize::MAX } else { 1 })
    }

    fn wake_event_count(&mut self, event_slot: u64, maximum: usize) -> usize {
        if maximum == 0 {
            return 0;
        }
        let mut woken = 0;
        for slot in &mut self.slots {
            if slot.state != TaskState::Blocked || slot.sleep_wait || slot.wait_event != event_slot
            {
                continue;
            }
            slot.frame.x[0] = 0;
            slot.wait_event = 0;
            slot.sleep_wait = false;
            slot.wait_deadline_us = 0;
            slot.state = TaskState::Runnable;
            woken += 1;
            if woken == maximum {
                break;
            }
        }
        woken
    }

    fn linux_futex_event_key(address: u64) -> u64 {
        0x8000_0000_0000_0000 | address
    }

    fn wake_event_timeouts(&mut self, now_us: u64) -> usize {
        let mut woken = 0;
        for slot in &mut self.slots {
            if slot.state != TaskState::Blocked
                || slot.wait_deadline_us == 0
                || now_us < slot.wait_deadline_us
            {
                continue;
            }
            slot.frame.x[0] = if slot.sleep_wait { 0 } else { ERR_TIMED_OUT };
            slot.wait_event = 0;
            slot.sleep_wait = false;
            slot.wait_deadline_us = 0;
            slot.state = TaskState::Runnable;
            woken += 1;
        }
        woken
    }

    fn mapping_overlaps(&self, task_index: usize, base: u64, length: u64) -> bool {
        let end = base.saturating_add(length);
        self.slots[task_index].mappings.iter().any(|mapping| {
            mapping.active
                && base < mapping.base.saturating_add(mapping.length)
                && mapping.base < end
        })
    }

    fn mapping_owner_index(&self, task_index: usize) -> usize {
        if !self.slots[task_index].is_thread {
            return task_index;
        }
        let address_space = self.slots[task_index].address_space;
        self.slots
            .iter()
            .position(|slot| {
                !slot.is_thread
                    && slot.address_space == address_space
                    && slot.state != TaskState::Empty
            })
            .unwrap_or(task_index)
    }

    fn sync_address_space_mappings(
        &mut self,
        address_space: usize,
        mappings: [UserMapping; MAX_MEMORY_MAPPINGS],
    ) {
        for slot in &mut self.slots {
            if slot.state != TaskState::Empty && slot.address_space == address_space {
                slot.mappings = mappings;
            }
        }
    }

    fn map_memory(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        addr_hint: u64,
        length: u64,
        flags: u64,
    ) -> Result<u64, u64> {
        self.map_memory_inner(frames, addr_hint, length, flags, false)
    }

    fn map_memory_fixed(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        address: u64,
        length: u64,
        flags: u64,
    ) -> Result<u64, u64> {
        self.map_memory_inner(frames, address, length, flags, true)
    }

    fn map_memory_inner(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        addr_hint: u64,
        length: u64,
        flags: u64,
        replace_existing: bool,
    ) -> Result<u64, u64> {
        let rounded_length = round_memory_length(length)?;
        let page_count = usize::try_from(rounded_length / PAGE_SIZE).map_err(|_| ERR_OVERFLOW)?;
        if page_count > MAX_MEMORY_PAGES {
            return Err(ERR_OVERFLOW);
        }
        let protection = (flags >> 16) & 0xff;
        if protection & !0x7 != 0 {
            return Err(ERR_INVALID);
        }
        let task_index = self.mapping_owner_index(self.current);
        let base = if addr_hint == 0 {
            if replace_existing {
                return Err(ERR_INVALID);
            }
            let mut candidate = DYNAMIC_MEMORY_START;
            let mut selected = None;
            while candidate
                .checked_add(rounded_length)
                .is_some_and(|end| end <= DYNAMIC_MEMORY_END)
            {
                if !self.mapping_overlaps(task_index, candidate, rounded_length) {
                    selected = Some(candidate);
                    break;
                }
                candidate = candidate.saturating_add(PAGE_SIZE);
            }
            selected.ok_or(ERR_OUT_OF_MEMORY)?
        } else {
            if addr_hint & (PAGE_SIZE - 1) != 0
                || addr_hint < DYNAMIC_MEMORY_START
                || addr_hint
                    .checked_add(rounded_length)
                    .is_none_or(|end| end > DYNAMIC_MEMORY_END)
                || (!replace_existing
                    && self.mapping_overlaps(task_index, addr_hint, rounded_length))
            {
                return Err(ERR_INVALID);
            }
            addr_hint
        };

        let readable = protection & 1 != 0;
        let writable = protection & 2 != 0;
        let executable = protection & 4 != 0;
        let address_space = self.slots[task_index].address_space;
        let mut physical_pages = [0u64; MAX_MEMORY_PAGES];
        let mut allocated_count = 0;
        mmu::activate_kernel_identity_space();
        for physical_page in physical_pages.iter_mut().take(page_count) {
            let Some(physical) = frames.next_frame() else {
                for allocated in physical_pages.iter().copied().take(allocated_count) {
                    let _ = frames.release_frame(allocated);
                }
                let _ = mmu::activate_user_space(address_space);
                return Err(ERR_OUT_OF_MEMORY);
            };
            unsafe {
                core::ptr::write_bytes(physical as *mut u8, 0, PAGE_SIZE as usize);
            }
            *physical_page = physical;
            allocated_count += 1;
        }

        if replace_existing {
            if let Err(error) =
                self.replace_overlapping_mappings(frames, task_index, base, rounded_length)
            {
                for allocated in physical_pages.iter().copied().take(allocated_count) {
                    let _ = frames.release_frame(allocated);
                }
                let _ = mmu::activate_user_space(address_space);
                return Err(error);
            }
        }
        let Some(mapping_index) = self.slots[task_index]
            .mappings
            .iter()
            .position(|mapping| !mapping.active)
        else {
            for allocated in physical_pages.iter().copied().take(allocated_count) {
                let _ = frames.release_frame(allocated);
            }
            let _ = mmu::activate_user_space(address_space);
            return Err(ERR_OUT_OF_MEMORY);
        };
        for (index, physical) in physical_pages.iter().copied().take(page_count).enumerate() {
            let virtual_address = base + index as u64 * PAGE_SIZE;
            if !mmu::map_user_page(
                self.slots[task_index].address_space,
                virtual_address,
                physical,
                readable,
                writable,
                executable,
            ) {
                for rollback in 0..index {
                    if let Some(released) = mmu::unmap_user_page(
                        self.slots[task_index].address_space,
                        base + rollback as u64 * PAGE_SIZE,
                    ) {
                        let _ = frames.release_frame(released);
                    }
                }
                let _ = frames.release_frame(physical);
                let _ = mmu::activate_user_space(address_space);
                return Err(ERR_OUT_OF_MEMORY);
            }
        }
        self.slots[task_index].mappings[mapping_index] = UserMapping {
            base,
            length: rounded_length,
            protection,
            shared: false,
            active: true,
        };
        let mappings = self.slots[task_index].mappings;
        self.sync_address_space_mappings(address_space, mappings);
        if !mmu::activate_user_space(address_space) {
            for index in 0..page_count {
                if let Some(physical) =
                    mmu::unmap_user_page(address_space, base + index as u64 * PAGE_SIZE)
                {
                    let _ = frames.release_frame(physical);
                }
            }
            self.slots[task_index].mappings[mapping_index] = UserMapping::EMPTY;
            let mappings = self.slots[task_index].mappings;
            self.sync_address_space_mappings(address_space, mappings);
            return Err(ERR_NOT_SUPPORTED);
        }
        Ok(base)
    }

    /// Replace every existing private mapping page covered by a fixed Linux
    /// mapping. Bionic reserves a larger anonymous range first and later maps
    /// file-backed ELF segments over portions of that range; retaining the
    /// reservation would make the fixed segment appear to overlap itself.
    fn replace_overlapping_mappings(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        task_index: usize,
        base: u64,
        length: u64,
    ) -> Result<(), u64> {
        let end = base.checked_add(length).ok_or(ERR_OVERFLOW)?;
        let address_space = self.slots[task_index].address_space;
        let mut survivors = [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS];
        let mut survivor_count = 0usize;

        mmu::activate_kernel_identity_space();
        for mapping in self.slots[task_index].mappings.iter().copied() {
            let overlaps = mapping.active
                && base < mapping.base.saturating_add(mapping.length)
                && mapping.base < end;
            if overlaps && mapping.shared {
                return Err(ERR_INVALID);
            }
            if !overlaps {
                if mapping.active {
                    if survivor_count == survivors.len() {
                        return Err(ERR_OUT_OF_MEMORY);
                    }
                    survivors[survivor_count] = mapping;
                    survivor_count += 1;
                }
                continue;
            }
            let mapping_end = mapping
                .base
                .checked_add(mapping.length)
                .ok_or(ERR_OVERFLOW)?;
            let overlap_start = base.max(mapping.base);
            let overlap_end = end.min(mapping_end);
            let page_count = (overlap_end - overlap_start) / PAGE_SIZE;
            for page in 0..page_count {
                let address = overlap_start + page * PAGE_SIZE;
                let physical = mmu::unmap_user_page(address_space, address).ok_or(ERR_INVALID)?;
                let _ = frames.release_frame(physical);
            }
            if mapping.base < overlap_start {
                if survivor_count == survivors.len() {
                    return Err(ERR_OUT_OF_MEMORY);
                }
                survivors[survivor_count] = UserMapping {
                    base: mapping.base,
                    length: overlap_start - mapping.base,
                    ..mapping
                };
                survivor_count += 1;
            }
            if overlap_end < mapping_end {
                if survivor_count == survivors.len() {
                    return Err(ERR_OUT_OF_MEMORY);
                }
                survivors[survivor_count] = UserMapping {
                    base: overlap_end,
                    length: mapping_end - overlap_end,
                    ..mapping
                };
                survivor_count += 1;
            }
        }
        self.slots[task_index].mappings = [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS];
        self.slots[task_index].mappings[..survivor_count]
            .copy_from_slice(&survivors[..survivor_count]);
        Ok(())
    }

    fn unmap_memory(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        address: u64,
        length: u64,
    ) -> Result<u64, u64> {
        let length = round_memory_length(length)?;
        let task_index = self.mapping_owner_index(self.current);
        let mapping_index = self.slots[task_index]
            .mappings
            .iter()
            .position(|mapping| {
                mapping.active
                    && !mapping.shared
                    && mapping.base == address
                    && mapping.length == length
            })
            .ok_or(ERR_INVALID)?;
        let address_space = self.slots[task_index].address_space;
        let page_count = usize::try_from(length / PAGE_SIZE).map_err(|_| ERR_OVERFLOW)?;
        let mut physical_pages = [0u64; MAX_MEMORY_PAGES];
        for (index, physical_page) in physical_pages.iter_mut().take(page_count).enumerate() {
            *physical_page =
                mmu::user_page_physical_in_space(address_space, address + index as u64 * PAGE_SIZE)
                    .ok_or(ERR_INVALID)?;
        }
        for index in 0..page_count {
            if mmu::unmap_user_page(address_space, address + index as u64 * PAGE_SIZE).is_none() {
                return Err(ERR_INVALID);
            }
            let _ = frames.release_frame(physical_pages[index]);
        }
        self.slots[task_index].mappings[mapping_index] = UserMapping::EMPTY;
        let mappings = self.slots[task_index].mappings;
        self.sync_address_space_mappings(address_space, mappings);
        Ok(0)
    }

    fn protect_memory(&mut self, address: u64, length: u64, protection: u64) -> Result<u64, u64> {
        let length = round_memory_length(length)?;
        if protection & !0x7 != 0 {
            return Err(ERR_INVALID);
        }
        let task_index = self.mapping_owner_index(self.current);
        let mapping_index = self.slots[task_index]
            .mappings
            .iter()
            .position(|mapping| {
                mapping.active
                    && !mapping.shared
                    && mapping.base == address
                    && mapping.length == length
            })
            .ok_or(ERR_INVALID)?;
        let address_space = self.slots[task_index].address_space;
        let readable = protection & 1 != 0;
        let writable = protection & 2 != 0;
        let executable = protection & 4 != 0;
        let page_count = usize::try_from(length / PAGE_SIZE).map_err(|_| ERR_OVERFLOW)?;
        for index in 0..page_count {
            if !mmu::protect_user_page(
                address_space,
                address + index as u64 * PAGE_SIZE,
                readable,
                writable,
                executable,
            ) {
                return Err(ERR_INVALID);
            }
        }
        self.slots[task_index].mappings[mapping_index].protection = protection;
        let mappings = self.slots[task_index].mappings;
        self.sync_address_space_mappings(address_space, mappings);
        Ok(0)
    }

    fn fork_current(
        &mut self,
        live: &mut Aarch64TrapFrame,
        frames: &mut PhysicalFrameAllocator,
    ) -> Result<u64, u64> {
        let parent_index = self.current;
        let child_index = self.free_slot().ok_or(ERR_OUT_OF_MEMORY)?;
        let child_space = self.free_address_space().ok_or(ERR_OUT_OF_MEMORY)?;
        let child_pid = self.next_pid().ok_or(ERR_OUT_OF_MEMORY)?;
        let parent_pid = self.slots[parent_index].pid;
        let terminal_handle = self.slots[parent_index].terminal_handle;
        let address_space = self.slots[parent_index].address_space;
        let personality = self.slots[parent_index].personality;
        let name = self.slots[parent_index].name;
        let name_len = self.slots[parent_index].name_len;
        let mappings = self.slots[parent_index].mappings;
        let linux_signal_mask = self.slots[parent_index].linux_signal_mask;
        let linux_signal_actions = self.slots[parent_index].linux_signal_actions;
        let mut shared_ranges = [(0u64, 0u64); MAX_MEMORY_MAPPINGS];
        let mut shared_range_count = 0usize;
        for mapping in mappings
            .iter()
            .copied()
            .filter(|mapping| mapping.active && mapping.shared)
        {
            shared_ranges[shared_range_count] = (mapping.base, mapping.length);
            shared_range_count += 1;
        }
        let mut child_frame = *live;
        child_frame.x[0] = 0;
        if !fs::inherit_fds(parent_pid, child_pid) {
            return Err(ERR_OUT_OF_MEMORY);
        }
        if !mmu::clone_user_space(
            address_space,
            child_space,
            frames,
            &shared_ranges[..shared_range_count],
        ) {
            fs::drop_owner(child_pid);
            return Err(ERR_OUT_OF_MEMORY);
        }
        if !fs::inherit_shared_mappings(parent_pid, child_pid) {
            fs::drop_owner(child_pid);
            let _ =
                mmu::release_user_space(child_space, frames, &shared_ranges[..shared_range_count]);
            return Err(ERR_OUT_OF_MEMORY);
        }
        if !self.install_child(
            child_index,
            child_pid,
            &name[..name_len],
            child_space,
            parent_pid,
            parent_pid,
            terminal_handle,
            child_frame,
            personality,
        ) {
            fs::drop_owner(child_pid);
            let _ =
                mmu::release_user_space(child_space, frames, &shared_ranges[..shared_range_count]);
            return Err(ERR_OUT_OF_MEMORY);
        }
        self.slots[child_index].mappings = mappings;
        self.slots[child_index].linux_signal_mask = linux_signal_mask;
        self.slots[child_index].linux_signal_actions = linux_signal_actions;
        live.x[0] = child_pid;
        Ok(child_pid)
    }

    fn next_active(&self, current: usize) -> Option<usize> {
        for offset in 1..=MAX_TASKS {
            let candidate = (current + offset) % MAX_TASKS;
            if self.slots[candidate].state == TaskState::Runnable {
                return Some(candidate);
            }
        }
        None
    }

    fn has_runnable_peer(&self) -> bool {
        self.next_active(self.current).is_some()
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.state == TaskState::Empty)
    }

    fn free_address_space(&self) -> Option<usize> {
        (0..mmu::MAX_USER_SPACES).find(|address_space| {
            !self
                .slots
                .iter()
                .any(|slot| slot.state != TaskState::Empty && slot.address_space == *address_space)
        })
    }

    fn next_pid(&self) -> Option<u64> {
        let start = self.next_pid_hint.max(1);
        let mut candidate = start;
        loop {
            if !self
                .slots
                .iter()
                .any(|slot| slot.state != TaskState::Empty && slot.pid == candidate)
            {
                return Some(candidate);
            }
            candidate = candidate.checked_add(1).unwrap_or(1);
            if candidate == start {
                return None;
            }
        }
    }

    fn reserve_shared_mapping(
        &mut self,
        addr_hint: u64,
        length: u64,
        protection: u64,
    ) -> Result<u64, u64> {
        let task_index = self.current;
        let mapping_index = self.slots[task_index]
            .mappings
            .iter()
            .position(|mapping| !mapping.active)
            .ok_or(ERR_OUT_OF_MEMORY)?;
        let base = if addr_hint == 0 {
            let mut candidate = DYNAMIC_MEMORY_START;
            let mut selected = None;
            while candidate
                .checked_add(length)
                .is_some_and(|end| end <= DYNAMIC_MEMORY_END)
            {
                if !self.mapping_overlaps(task_index, candidate, length) {
                    selected = Some(candidate);
                    break;
                }
                candidate = candidate.saturating_add(PAGE_SIZE);
            }
            selected.ok_or(ERR_OUT_OF_MEMORY)?
        } else if addr_hint & (PAGE_SIZE - 1) == 0
            && addr_hint >= DYNAMIC_MEMORY_START
            && addr_hint
                .checked_add(length)
                .is_some_and(|end| end <= DYNAMIC_MEMORY_END)
            && !self.mapping_overlaps(task_index, addr_hint, length)
        {
            addr_hint
        } else {
            return Err(ERR_INVALID);
        };
        self.slots[task_index].mappings[mapping_index] = UserMapping {
            base,
            length,
            protection,
            shared: true,
            active: true,
        };
        Ok(base)
    }

    fn release_shared_mapping(&mut self, address: u64, length: u64) -> bool {
        let task_index = self.current;
        let Some(mapping) = self.slots[task_index].mappings.iter_mut().find(|mapping| {
            mapping.active && mapping.shared && mapping.base == address && mapping.length == length
        }) else {
            return false;
        };
        *mapping = UserMapping::EMPTY;
        true
    }

    fn release_shared_mapping_for_pid(&mut self, pid: u64, address: u64, length: u64) -> bool {
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.pid == pid && slot.state != TaskState::Empty)
        else {
            return false;
        };
        let Some(mapping) = slot.mappings.iter_mut().find(|mapping| {
            mapping.active && mapping.shared && mapping.base == address && mapping.length == length
        }) else {
            return false;
        };
        *mapping = UserMapping::EMPTY;
        true
    }

    fn address_space_for_pid(&self, pid: u64) -> Option<usize> {
        self.slots
            .iter()
            .find(|slot| slot.pid == pid && slot.state != TaskState::Empty)
            .map(|slot| slot.address_space)
    }

    fn process_control_parts(handle: u64) -> Option<(u64, u64, u16)> {
        if handle & PROCESS_HANDLE_TAG == 0 {
            return None;
        }
        let owner_pid = (handle >> PROCESS_HANDLE_OWNER_SHIFT) & PROCESS_HANDLE_PID_MASK;
        let generation =
            ((handle >> PROCESS_HANDLE_GENERATION_SHIFT) & PROCESS_HANDLE_GENERATION_MASK) as u16;
        let target_pid = handle & PROCESS_HANDLE_PID_MASK;
        (owner_pid != 0 && target_pid != 0 && generation != 0)
            .then_some((owner_pid, target_pid, generation))
    }

    fn encode_process_control(owner_pid: u64, target_pid: u64, generation: u16) -> u64 {
        PROCESS_HANDLE_TAG
            | ((owner_pid & PROCESS_HANDLE_PID_MASK) << PROCESS_HANDLE_OWNER_SHIFT)
            | ((u64::from(generation) & PROCESS_HANDLE_GENERATION_MASK)
                << PROCESS_HANDLE_GENERATION_SHIFT)
            | (target_pid & PROCESS_HANDLE_PID_MASK)
    }

    fn process_control_owner(handle: u64) -> Option<u64> {
        Self::process_control_parts(handle).map(|(owner_pid, _, _)| owner_pid)
    }

    fn process_control_entry(&self, handle: u64) -> Option<usize> {
        let (owner_pid, target_pid, generation) = Self::process_control_parts(handle)?;
        if self.current_pid() != Some(owner_pid) {
            return None;
        }
        self.slots[self.current]
            .process_controls
            .iter()
            .position(|entry| {
                entry.active && entry.target_pid == target_pid && entry.generation == generation
            })
    }

    fn process_control_slot(&self, handle: u64) -> Option<usize> {
        let entry_index = self.process_control_entry(handle)?;
        let target_pid = self.slots[self.current].process_controls[entry_index].target_pid;
        self.slots.iter().position(|slot| {
            slot.state != TaskState::Empty && !slot.is_thread && slot.pid == target_pid
        })
    }

    fn next_process_control_generation(&mut self, owner_index: usize) -> u16 {
        let generation = self.slots[owner_index].next_process_control_generation;
        self.slots[owner_index].next_process_control_generation = generation.wrapping_add(1).max(1);
        generation
    }

    fn allocate_process_control(
        &mut self,
        owner_index: usize,
        target_pid: u64,
    ) -> Result<u64, u64> {
        if owner_index >= MAX_TASKS
            || self.slots[owner_index].state == TaskState::Empty
            || self.slots[owner_index].pid > PROCESS_HANDLE_PID_MASK
            || target_pid == 0
            || target_pid > PROCESS_HANDLE_PID_MASK
        {
            return Err(ERR_OVERFLOW);
        }
        let entry_index = self.slots[owner_index]
            .process_controls
            .iter()
            .position(|entry| !entry.active)
            .ok_or(ERR_OUT_OF_MEMORY)?;
        let generation = self.next_process_control_generation(owner_index);
        self.slots[owner_index].process_controls[entry_index] = ProcessControlEntry {
            target_pid,
            generation,
            active: true,
        };
        Ok(Self::encode_process_control(
            self.slots[owner_index].pid,
            target_pid,
            generation,
        ))
    }

    fn open_process_control(&mut self, pid: u64) -> Result<u64, u64> {
        let caller_index = self.current;
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        let child = self
            .slots
            .iter()
            .find(|slot| slot.state != TaskState::Empty && !slot.is_thread && slot.pid == pid)
            .ok_or(ERR_NO_SUCH_PROCESS)?;
        if child.parent_pid != caller && child.supervisor_pid != caller {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        self.allocate_process_control(caller_index, pid)
    }

    fn close_process_control(&mut self, handle: u64) -> Result<u64, u64> {
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let entry_index = self.process_control_entry(handle).ok_or(ERR_BAD_HANDLE)?;
        self.slots[self.current].process_controls[entry_index] = ProcessControlEntry::EMPTY;
        Ok(0)
    }

    fn duplicate_process_control(&mut self, handle: u64) -> Result<u64, u64> {
        let caller_index = self.current;
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let entry_index = self.process_control_entry(handle).ok_or(ERR_BAD_HANDLE)?;
        let target_pid = self.slots[self.current].process_controls[entry_index].target_pid;
        self.allocate_process_control(caller_index, target_pid)
    }

    fn transfer_process_control(&mut self, target_pid: u64, handle: u64) -> Result<u64, u64> {
        let source_index = self.current;
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let target_index = self
            .slots
            .iter()
            .position(|slot| {
                slot.state != TaskState::Empty
                    && !slot.is_thread
                    && slot.pid == target_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .ok_or(ERR_NO_SUCH_PROCESS)?;
        let entry_index = self.process_control_entry(handle).ok_or(ERR_BAD_HANDLE)?;
        let entry = self.slots[source_index].process_controls[entry_index];
        self.slots[source_index].process_controls[entry_index] = ProcessControlEntry::EMPTY;
        match self.allocate_process_control(target_index, entry.target_pid) {
            Ok(new_handle) => Ok(new_handle),
            Err(error) => {
                self.slots[source_index].process_controls[entry_index] = entry;
                Err(error)
            }
        }
    }

    fn revoke_process_control(&mut self, handle: u64) -> Result<u64, u64> {
        self.close_process_control(handle)
    }

    fn process_control_stop(&mut self, handle: u64, status: u64) -> Result<u64, u64> {
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let index = self.process_control_slot(handle).ok_or(ERR_BAD_HANDLE)?;
        if self.slots[index].is_thread {
            return Err(ERR_INVALID);
        }
        if self.slots[index].pid == INIT_PID {
            return Err(ERR_PERMISSION_DENIED);
        }
        if index == self.current {
            return Err(ERR_INVALID);
        }
        if self.slots[index].state == TaskState::Exited {
            return Ok(0);
        }

        let target_pid = self.slots[index].pid;
        self.slots[index].frame.x[0] = status;
        self.slots[index].exit_status = status;
        self.slots[index].wait_target = 0;
        self.slots[index].wait_event = 0;
        self.slots[index].sleep_wait = false;
        self.slots[index].wait_deadline_us = 0;
        self.slots[index].state = TaskState::Exited;
        fs::drop_owner(target_pid);

        let mut init_index = None;
        for child_index in 0..MAX_TASKS {
            if self.slots[child_index].state != TaskState::Empty
                && self.slots[child_index].pid == INIT_PID
            {
                init_index = Some(child_index);
                break;
            }
        }
        for child_index in 0..MAX_TASKS {
            if child_index != index
                && self.slots[child_index].state != TaskState::Empty
                && !self.slots[child_index].is_thread
                && self.slots[child_index].parent_pid == target_pid
            {
                let adopted_parent = init_index.map(|_| INIT_PID).unwrap_or(0);
                self.slots[child_index].parent_pid = adopted_parent;
                if self.slots[child_index].supervisor_pid == target_pid {
                    self.slots[child_index].supervisor_pid = adopted_parent;
                }
            }
        }
        for slot in &mut self.slots {
            if slot.state == TaskState::Blocked
                && (slot.wait_target == target_pid || slot.wait_target == u64::MAX)
            {
                slot.frame.x[0] = status;
                slot.wait_target = 0;
                slot.wait_event = 0;
                slot.sleep_wait = false;
                slot.wait_deadline_us = 0;
                slot.state = TaskState::Runnable;
            }
        }
        Ok(0)
    }

    fn process_control_assign(&mut self, handle: u64, supervisor_pid: u64) -> Result<u64, u64> {
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let target_index = self.process_control_slot(handle).ok_or(ERR_BAD_HANDLE)?;
        let target_pid = self.slots[target_index].pid;
        if target_pid == supervisor_pid || supervisor_pid == 0 {
            return Err(ERR_INVALID);
        }
        if !self.contains_live_process_pid(supervisor_pid) {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        self.slots[target_index].supervisor_pid = supervisor_pid;
        Ok(0)
    }

    fn process_control_status(&self, handle: u64) -> Result<u64, u64> {
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let index = self.process_control_slot(handle).ok_or(ERR_BAD_HANDLE)?;
        let slot = &self.slots[index];
        if slot.is_thread {
            return Err(ERR_INVALID);
        }
        Ok(match slot.state {
            TaskState::Empty => return Err(ERR_NO_SUCH_PROCESS),
            TaskState::Runnable if index == self.current => TaskState::RUNNING,
            TaskState::Runnable => TaskState::READY,
            TaskState::Blocked => TaskState::BLOCKED,
            TaskState::Exited => TaskState::TERMINATED,
        })
    }

    fn process_control_reap(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        handle: u64,
    ) -> Result<u64, u64> {
        let caller = self.current_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        if Self::process_control_owner(handle) != Some(caller) {
            return Err(ERR_BAD_HANDLE);
        }
        let index = self.process_control_slot(handle).ok_or(ERR_BAD_HANDLE)?;
        let slot = &self.slots[index];
        if slot.parent_pid != caller && slot.supervisor_pid != caller {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        if slot.state != TaskState::Exited {
            return Err(ERR_WOULD_BLOCK);
        }
        self.reap_exited(index, frames)
    }

    fn reap_exited(
        &mut self,
        index: usize,
        frames: &mut PhysicalFrameAllocator,
    ) -> Result<u64, u64> {
        if index >= MAX_TASKS
            || index == self.current
            || self.slots[index].state != TaskState::Exited
        {
            return Err(ERR_INVALID);
        }
        let slot = self.slots[index];
        let mut shared_ranges = [(0u64, 0u64); MAX_MEMORY_MAPPINGS];
        let mut shared_range_count = 0usize;
        for mapping in slot
            .mappings
            .iter()
            .copied()
            .filter(|mapping| mapping.active && mapping.shared)
        {
            shared_ranges[shared_range_count] = (mapping.base, mapping.length);
            shared_range_count += 1;
        }
        if !mmu::release_user_space(
            slot.address_space,
            frames,
            &shared_ranges[..shared_range_count],
        ) {
            return Err(ERR_OUT_OF_MEMORY);
        }
        self.slots[index] = TaskSlot::EMPTY;
        Ok(slot.exit_status)
    }

    fn queue_linux_signal(
        &mut self,
        target_pid: u64,
        signal: u64,
        thread_target: bool,
    ) -> Result<u64, u64> {
        if signal > LINUX_SIGNAL_COUNT as u64 {
            return Err(ERR_INVALID);
        }
        let target_index = self
            .slots
            .iter()
            .position(|slot| {
                slot.pid == target_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
                    && (thread_target || !slot.is_thread)
            })
            .ok_or(ERR_NO_SUCH_PROCESS)?;
        if signal == 0 {
            return Ok(0);
        }

        let action = self.slots[target_index].linux_signal_actions[signal as usize - 1].bytes;
        let handler = u64::from_ne_bytes(action[..8].try_into().unwrap());
        // SIG_IGN is a disposition, not a queued event. SIGKILL/SIGSTOP
        // cannot be ignored and are handled by the default-action path.
        if handler == 1 && !matches!(signal, 9 | 19) {
            return Ok(0);
        }
        let bit = 1u64 << (signal - 1);
        self.slots[target_index].linux_pending_signals |= bit;
        if self.slots[target_index].state == TaskState::Blocked
            && self.slots[target_index].linux_signal_mask & bit == 0
        {
            self.slots[target_index].state = TaskState::Runnable;
            self.slots[target_index].wait_target = 0;
            self.slots[target_index].wait_event = 0;
            self.slots[target_index].sleep_wait = false;
            self.slots[target_index].wait_deadline_us = 0;
            self.slots[target_index].linux_wait = false;
            self.slots[target_index].linux_wait_status_address = 0;
            self.slots[target_index].frame.x[0] = ERR_INTERRUPTED;
        }
        Ok(0)
    }

    fn linux_thread_belongs_to_process(&self, tid: u64, tgid: u64) -> bool {
        self.slots.iter().any(|slot| {
            slot.pid == tid
                && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
                && if slot.is_thread {
                    slot.parent_pid == tgid
                } else {
                    slot.pid == tgid
                }
        })
    }

    fn current_linux_pgid(&self) -> Option<u64> {
        let slot = &self.slots[self.current];
        matches!(slot.state, TaskState::Runnable | TaskState::Blocked).then_some(slot.linux_pgid)
    }

    fn set_linux_pgid(&mut self, requested_pid: u64, requested_pgid: u64) -> Result<u64, u64> {
        let caller_pid = self.resource_owner_pid().ok_or(ERR_NO_SUCH_PROCESS)?;
        let target_pid = if requested_pid == 0 {
            caller_pid
        } else {
            requested_pid
        };
        let target_index = self
            .slots
            .iter()
            .position(|slot| {
                !slot.is_thread
                    && slot.pid == target_pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .ok_or(ERR_NO_SUCH_PROCESS)?;
        if target_pid != caller_pid && self.slots[target_index].parent_pid != caller_pid {
            return Err(ERR_PERMISSION_DENIED);
        }
        let pgid = if requested_pgid == 0 {
            target_pid
        } else {
            requested_pgid
        };
        if pgid == 0 {
            return Err(ERR_INVALID);
        }
        let session = self.slots[target_index].linux_sid;
        if pgid != target_pid
            && !self.slots.iter().any(|slot| {
                !slot.is_thread
                    && slot.linux_pgid == pgid
                    && slot.linux_sid == session
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
        {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        self.slots[target_index].linux_pgid = pgid;
        Ok(0)
    }

    fn linux_pgid_for(&self, requested_pid: u64) -> Result<u64, u64> {
        let pid = if requested_pid == 0 {
            self.resource_owner_pid().ok_or(ERR_NO_SUCH_PROCESS)?
        } else {
            requested_pid
        };
        self.slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| slot.linux_pgid)
            .ok_or(ERR_NO_SUCH_PROCESS)
    }

    fn linux_sid_for(&self, requested_pid: u64) -> Result<u64, u64> {
        let pid = if requested_pid == 0 {
            self.resource_owner_pid().ok_or(ERR_NO_SUCH_PROCESS)?
        } else {
            requested_pid
        };
        self.slots
            .iter()
            .find(|slot| {
                !slot.is_thread
                    && slot.pid == pid
                    && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
            })
            .map(|slot| slot.linux_sid)
            .ok_or(ERR_NO_SUCH_PROCESS)
    }

    fn linux_setsid(&mut self) -> Result<u64, u64> {
        let current = self.current;
        if self.slots[current].is_thread
            || !matches!(
                self.slots[current].state,
                TaskState::Runnable | TaskState::Blocked
            )
        {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        let pid = self.slots[current].pid;
        if self.slots[current].linux_pgid == pid {
            return Err(ERR_PERMISSION_DENIED);
        }
        self.slots[current].linux_sid = pid;
        self.slots[current].linux_pgid = pid;
        Ok(pid)
    }

    fn queue_linux_process_group(
        &mut self,
        pgid: u64,
        signal: u64,
        include_init: bool,
    ) -> Result<u64, u64> {
        let mut pids = [0u64; MAX_TASKS];
        let mut count = 0usize;
        for slot in &self.slots {
            if !slot.is_thread
                && slot.linux_pgid == pgid
                && (!(!include_init && slot.pid == INIT_PID))
                && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
                && count < pids.len()
            {
                pids[count] = slot.pid;
                count += 1;
            }
        }
        if count == 0 {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        for pid in pids.into_iter().take(count) {
            let _ = self.queue_linux_signal(pid, signal, false)?;
        }
        Ok(0)
    }

    fn queue_linux_all_processes(&mut self, signal: u64) -> Result<u64, u64> {
        let caller = self.resource_owner_pid().unwrap_or(0);
        let mut pids = [0u64; MAX_TASKS];
        let mut count = 0usize;
        for slot in &self.slots {
            if !slot.is_thread
                && slot.pid != INIT_PID
                && slot.pid != caller
                && matches!(slot.state, TaskState::Runnable | TaskState::Blocked)
                && count < pids.len()
            {
                pids[count] = slot.pid;
                count += 1;
            }
        }
        if count == 0 {
            return Err(ERR_NO_SUCH_PROCESS);
        }
        for pid in pids.into_iter().take(count) {
            let _ = self.queue_linux_signal(pid, signal, false)?;
        }
        Ok(0)
    }

    fn take_linux_signal_frame(&mut self, live: &mut Aarch64TrapFrame) -> bool {
        loop {
            let current = self.current;
            if self.slots[current].state != TaskState::Runnable {
                return true;
            }
            let available =
                self.slots[current].linux_pending_signals & !self.slots[current].linux_signal_mask;
            if available == 0 {
                return true;
            }
            let bit_index = available.trailing_zeros() as usize;
            let signal = bit_index + 1;
            let bit = 1u64 << bit_index;
            self.slots[current].linux_pending_signals &= !bit;
            let action = self.slots[current].linux_signal_actions[bit_index].bytes;
            let handler = u64::from_ne_bytes(action[..8].try_into().unwrap());
            let flags = u64::from_ne_bytes(action[8..16].try_into().unwrap());
            let restorer = u64::from_ne_bytes(action[16..24].try_into().unwrap());
            let action_mask = u64::from_ne_bytes(action[24..32].try_into().unwrap());

            if handler == 1 || (handler == 0 && is_linux_default_ignored(signal)) {
                continue;
            }
            if handler == 0 || matches!(signal, 9 | 19) {
                let _ = self.exit_current(live, signal as u64);
                return true;
            }

            let Some(context_index) = self.slots[current]
                .linux_signal_frames
                .iter()
                .position(|context| !context.active)
            else {
                // A bounded runtime cannot grow an arbitrary nested signal
                // frame stack. Leave the signal pending until sigreturn.
                self.slots[current].linux_pending_signals |= bit;
                return true;
            };
            let old_frame = *live;
            let Some(stack_pointer) = old_frame.sp_el0.checked_sub(256).map(|value| value & !0xf)
            else {
                let _ = self.exit_current(live, 11);
                return true;
            };
            let mut signal_frame = [0u8; 256];
            signal_frame[..4].copy_from_slice(&(signal as i32).to_ne_bytes());
            signal_frame[8..12].copy_from_slice(&1i32.to_ne_bytes());
            if user_memory::copy_to_user(stack_pointer, &signal_frame).is_err() {
                let _ = self.exit_current(live, 11);
                return true;
            }

            let saved_mask = self.slots[current].linux_signal_mask;
            self.slots[current].linux_signal_frames[context_index] = LinuxSignalFrame {
                saved_frame: old_frame,
                saved_mask,
                active: true,
            };
            let mut new_mask = saved_mask | action_mask;
            if flags & 0x4000_0000 == 0 {
                new_mask |= bit;
            }
            new_mask &= !(1u64 << (9 - 1) | 1u64 << (19 - 1));
            self.slots[current].linux_signal_mask = new_mask;
            live.sp_el0 = stack_pointer;
            live.elr_el1 = handler;
            live.x[0] = signal as u64;
            if flags & 0x4 != 0 {
                live.x[1] = stack_pointer;
                live.x[2] = stack_pointer + 128;
            } else {
                live.x[1] = 0;
                live.x[2] = 0;
            }
            live.x[30] = restorer;
            live.esr_el1 = 0;
            live.far_el1 = 0;
            if flags & 0x8000_0000 != 0 {
                self.slots[current].linux_signal_actions[bit_index] = LinuxSignalAction::EMPTY;
            }
            return true;
        }
    }

    fn linux_sigreturn(&mut self, live: &mut Aarch64TrapFrame) -> bool {
        let current = self.current;
        let Some(context_index) = self.slots[current]
            .linux_signal_frames
            .iter()
            .rposition(|context| context.active)
        else {
            live.x[0] = ERR_INVALID;
            return true;
        };
        let context = self.slots[current].linux_signal_frames[context_index];
        self.slots[current].linux_signal_frames[context_index] = LinuxSignalFrame::EMPTY;
        self.slots[current].linux_signal_mask = context.saved_mask;
        *live = context.saved_frame;
        true
    }
}

fn is_linux_default_ignored(signal: usize) -> bool {
    matches!(signal, 17 | 23 | 28)
}

fn round_memory_length(length: u64) -> Result<u64, u64> {
    if length == 0 {
        return Err(ERR_INVALID);
    }
    let rounded = length.checked_add(PAGE_SIZE - 1).ok_or(ERR_OVERFLOW)? & !(PAGE_SIZE - 1);
    (rounded <= (MAX_MEMORY_PAGES as u64) * PAGE_SIZE)
        .then_some(rounded)
        .ok_or(ERR_OVERFLOW)
}

// The first AArch64 runtime is single-core and enters this state only from
// the exception path. Keep the ownership boundary in this module so syscall
// code does not know whether the backing table is static, allocated, or per-CPU.
static mut SCHEDULER: Aarch64TaskScheduler = Aarch64TaskScheduler::EMPTY;
static mut INTERPRETER_STAGING: [u8; MAX_INTERPRETER_IMAGE] = [0; MAX_INTERPRETER_IMAGE];

pub(crate) struct ExecImageInfo {
    pub(crate) entry: u64,
    pub(crate) phdr: u64,
    pub(crate) phent: u64,
    pub(crate) phnum: u64,
    pub(crate) interpreter_entry: u64,
    pub(crate) interpreter_base: Option<u64>,
}

pub(crate) fn reset() {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).reset() }
}

pub(crate) fn install(
    index: usize,
    pid: u64,
    name: &[u8],
    address_space: usize,
    frame: Aarch64TrapFrame,
) -> bool {
    install_with_personality(
        index,
        pid,
        name,
        address_space,
        frame,
        AbiPersonality::Native,
    )
}

pub(crate) fn install_with_personality(
    index: usize,
    pid: u64,
    name: &[u8],
    address_space: usize,
    frame: Aarch64TrapFrame,
    personality: AbiPersonality,
) -> bool {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        if !scheduler.install(index, pid, name, address_space, frame) {
            return false;
        }
        scheduler.slots[index].personality = personality;
        true
    }
}

pub(crate) fn current_personality() -> AbiPersonality {
    unsafe {
        (*core::ptr::addr_of!(SCHEDULER)).slots[(*core::ptr::addr_of!(SCHEDULER)).current]
            .personality
    }
}

pub(crate) fn current_parent_pid() -> Option<u64> {
    unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let slot = &scheduler.slots[scheduler.current];
        (slot.state != TaskState::Empty).then_some(slot.parent_pid)
    }
}

pub(crate) fn set_current_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > MAX_TASK_NAME {
        return false;
    }
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.name = [0; MAX_TASK_NAME];
        slot.name[..name.len()].copy_from_slice(name);
        slot.name_len = name.len();
        true
    }
}

pub(crate) fn current_linux_brk() -> u64 {
    unsafe {
        (*core::ptr::addr_of!(SCHEDULER)).slots[(*core::ptr::addr_of!(SCHEDULER)).current].linux_brk
    }
}

pub(crate) fn set_current_linux_brk(value: u64) -> bool {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.linux_brk = value;
        true
    }
}

/// Load and install one child ELF in its own bounded TTBR0 root.
///
/// The allocator and page-table limits are intentionally explicit while the
/// generic process/resource layer is being ported. The ABI boundary is real:
/// the child receives a distinct user root, executable/data permissions, a
/// private stack page, PID, and saved EL0 frame.
pub(crate) fn spawn(
    frames: &mut PhysicalFrameAllocator,
    image: &[u8],
    name: &[u8],
    requested_supervisor_pid: u64,
    terminal_handle: u64,
) -> Result<u64, u64> {
    let (index, address_space, pid) = unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        (
            scheduler.free_slot().ok_or((-(12i64)) as u64)?,
            scheduler.free_address_space().ok_or((-(12i64)) as u64)?,
            scheduler.next_pid().ok_or((-(12i64)) as u64)?,
        )
    };
    if !mmu::reset_user_space(address_space) {
        return Err((-(12i64)) as u64);
    }
    let active_space = unsafe {
        (*core::ptr::addr_of!(SCHEDULER)).slots[(*core::ptr::addr_of!(SCHEDULER)).current]
            .address_space
    };
    mmu::activate_kernel_identity_space();
    let Some(image) = elf::load_image(address_space, image, frames) else {
        let _ = mmu::activate_user_space(active_space);
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    };
    let Some((entry, _interpreter_base)) = load_interpreter(address_space, &image, frames) else {
        let _ = mmu::activate_user_space(active_space);
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    };
    let Some(stack_page) = frames.next_frame() else {
        let _ = mmu::activate_user_space(active_space);
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    };
    if !elf::map_zeroed_user_page(address_space, STACK_ADDRESS, stack_page) {
        let _ = frames.release_frame(stack_page);
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    }
    if !mmu::activate_user_space(active_space) {
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    }
    let frame = Aarch64TrapFrame {
        x: [0; 31],
        elr_el1: entry,
        spsr_el1: 0,
        sp_el0: STACK_ADDRESS + PAGE_SIZE - 16,
        esr_el1: 0,
        far_el1: 0,
        tpidr_el0: 0,
    };
    let parent_pid = unsafe {
        (*core::ptr::addr_of!(SCHEDULER))
            .current_pid()
            .ok_or(ERR_NO_SUCH_PROCESS)?
    };
    let personality = current_personality();
    let supervisor_pid = if requested_supervisor_pid == 0 {
        parent_pid
    } else {
        let valid = unsafe {
            (*core::ptr::addr_of!(SCHEDULER)).contains_live_process_pid(requested_supervisor_pid)
        };
        if !valid {
            let _ = mmu::release_user_space(address_space, frames, &[]);
            return Err(ERR_NO_SUCH_PROCESS);
        }
        requested_supervisor_pid
    };
    let child_terminal_handle = if terminal_handle == 0 {
        0
    } else {
        match fs::inherit_terminal_handle(parent_pid, pid, terminal_handle) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = mmu::release_user_space(address_space, frames, &[]);
                return Err(error);
            }
        }
    };
    if !unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).install_child(
            index,
            pid,
            name,
            address_space,
            parent_pid,
            supervisor_pid,
            child_terminal_handle,
            frame,
            personality,
        )
    } {
        fs::drop_owner(pid);
        let _ = mmu::release_user_space(address_space, frames, &[]);
        return Err((-(12i64)) as u64);
    }
    uart::put_hex("aarch64 spawn pid=", pid);
    uart::put_hex("aarch64 spawn pages=", image.page_count as u64);
    Ok(pid)
}

/// Replace the current process image without changing its PID or parent.
///
/// The bounded address-space table has no separately allocated page-table
/// objects yet, so the new image is staged in an unreferenced TTBR0 root. The
/// old root is released only after the new ELF and stack are active; a failed
/// load therefore leaves the caller's image runnable.
pub(crate) fn exec(
    frames: &mut PhysicalFrameAllocator,
    live: &mut Aarch64TrapFrame,
    image: &[u8],
    name: &[u8],
) -> Result<ExecImageInfo, u64> {
    let (current, old_space, new_space, pid) = unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let current = scheduler.current;
        (
            current,
            scheduler.slots[current].address_space,
            scheduler.free_address_space().ok_or(ERR_OUT_OF_MEMORY)?,
            scheduler.slots[current].pid,
        )
    };
    if name.is_empty() || name.len() > MAX_TASK_NAME || new_space == old_space {
        return Err(ERR_INVALID);
    }
    if !mmu::reset_user_space(new_space) {
        return Err(ERR_OUT_OF_MEMORY);
    }

    mmu::activate_kernel_identity_space();
    let Some(loaded) = elf::load_image(new_space, image, frames) else {
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_INVALID);
    };
    let Some((interpreter_entry, interpreter_base)) = load_interpreter(new_space, &loaded, frames)
    else {
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_INVALID);
    };
    let Some(stack_page) = frames.next_frame() else {
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !elf::map_zeroed_user_page(new_space, STACK_ADDRESS, stack_page) {
        let _ = frames.release_frame(stack_page);
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_OUT_OF_MEMORY);
    }
    if !mmu::activate_user_space(new_space) {
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_OUT_OF_MEMORY);
    }
    // An exec tears down all process-local shared mappings before the old
    // root is released; the capability itself remains in the handle table.
    fs::cleanup_shared_mappings(pid);
    if !mmu::release_user_space(old_space, frames, &[]) {
        let _ = mmu::activate_user_space(old_space);
        let _ = mmu::release_user_space(new_space, frames, &[]);
        return Err(ERR_OUT_OF_MEMORY);
    }

    let mut new_frame = *live;
    new_frame.x = [0; 31];
    new_frame.elr_el1 = interpreter_entry;
    new_frame.sp_el0 = STACK_ADDRESS + PAGE_SIZE - 16;
    new_frame.esr_el1 = 0;
    new_frame.far_el1 = 0;
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        scheduler.slots[current].address_space = new_space;
        scheduler.slots[current].frame = new_frame;
        scheduler.slots[current].name = [0; MAX_TASK_NAME];
        scheduler.slots[current].name[..name.len()].copy_from_slice(name);
        scheduler.slots[current].name_len = name.len();
        scheduler.slots[current].mappings = [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS];
        let exec_context_length = scheduler.slots[current].selinux_exec_context_length;
        if exec_context_length != 0 {
            scheduler.slots[current].selinux_context =
                scheduler.slots[current].selinux_exec_context;
            scheduler.slots[current].selinux_context_length = exec_context_length;
            scheduler.slots[current].selinux_exec_context = [0; MAX_SELINUX_CONTEXT];
            scheduler.slots[current].selinux_exec_context_length = 0;
        }
    }
    *live = new_frame;
    uart::put_hex("aarch64 exec pid=", pid);
    uart::put_hex("aarch64 exec pages=", loaded.page_count as u64);
    Ok(ExecImageInfo {
        entry: loaded.entry,
        phdr: loaded.phdr,
        phent: loaded.phent,
        phnum: loaded.phnum,
        interpreter_entry,
        interpreter_base,
    })
}

/// Load the PT_INTERP image for a dynamic executable from the mounted VFS.
/// The main executable has already been copied into user pages, so the
/// bounded kernel staging buffer can be reused for linker64 without changing
/// the caller's image slice.
fn load_interpreter(
    space_id: usize,
    main_image: &elf::LoadedImage,
    frames: &mut PhysicalFrameAllocator,
) -> Option<(u64, Option<u64>)> {
    if main_image.interpreter_path_len == 0 {
        return Some((main_image.entry, None));
    }
    let path =
        core::str::from_utf8(&main_image.interpreter_path[..main_image.interpreter_path_len])
            .ok()?;
    let length = unsafe {
        fs::read_kernel_path(path, &mut *core::ptr::addr_of_mut!(INTERPRETER_STAGING)).ok()?
    };
    if length == 0 {
        return None;
    }
    let image = unsafe { &*core::ptr::addr_of!(INTERPRETER_STAGING) };
    let interpreter = elf::load_image_at(
        space_id,
        &image[..length],
        frames,
        elf::INTERPRETER_LOAD_BASE,
        false,
    )?;
    Some((interpreter.entry, Some(interpreter.load_bias)))
}

pub(crate) fn current_frame() -> *const Aarch64TrapFrame {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).current_frame() }
}

pub(crate) fn current_index() -> usize {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).current_index() }
}

pub(crate) fn current_pid() -> Option<u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).current_pid() }
}

/// Return the Linux process ID for the current task. A Linux thread has its
/// own TID, but all threads in the bounded process share the owner PID.
pub(crate) fn current_process_pid() -> Option<u64> {
    resource_owner_pid()
}

pub(crate) fn set_linux_clear_child_tid(address: u64) -> Option<u64> {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if !matches!(slot.state, TaskState::Runnable | TaskState::Blocked) {
            return None;
        }
        slot.linux_clear_child_tid = address;
        Some(slot.pid)
    }
}

pub(crate) fn linux_futex_event_key(address: u64) -> u64 {
    Aarch64TaskScheduler::linux_futex_event_key(address)
}

pub(crate) fn is_process_control_handle(handle: u64) -> bool {
    handle & PROCESS_HANDLE_TAG != 0
}

pub(crate) fn resource_owner_pid() -> Option<u64> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).resource_owner_pid() }
}

pub(crate) fn current_linux_credentials() -> Option<LinuxCredentials> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).current_linux_credentials() }
}

pub(crate) fn set_current_linux_credentials(credentials: LinuxCredentials) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).set_current_linux_credentials(credentials) }
}

fn valid_selinux_context(context: &[u8]) -> bool {
    !context.is_empty()
        && context.len() <= MAX_SELINUX_CONTEXT
        && context.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        && context.starts_with(b"u:")
        && context.iter().filter(|byte| **byte == b':').count() >= 3
}

/// Copy the current task's SELinux context without exposing the task table.
/// The caller adds the procfs NUL terminator required by Linux attr files.
pub(crate) fn current_selinux_context(destination: &mut [u8]) -> usize {
    unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let slot = &scheduler.slots[scheduler.current];
        let length = slot.selinux_context_length.min(destination.len());
        destination[..length].copy_from_slice(&slot.selinux_context[..length]);
        length
    }
}

/// Copy a pending exec context. An empty result means no transition is staged.
pub(crate) fn current_selinux_exec_context(destination: &mut [u8]) -> usize {
    unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let slot = &scheduler.slots[scheduler.current];
        let length = slot.selinux_exec_context_length.min(destination.len());
        destination[..length].copy_from_slice(&slot.selinux_exec_context[..length]);
        length
    }
}

/// Stage a validated context for the current task's next successful exec.
pub(crate) fn set_current_selinux_exec_context(context: &[u8]) -> bool {
    if !valid_selinux_context(context) {
        return false;
    }
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.selinux_exec_context = [0; MAX_SELINUX_CONTEXT];
        slot.selinux_exec_context[..context.len()].copy_from_slice(context);
        slot.selinux_exec_context_length = context.len();
        true
    }
}

pub(crate) fn contains_live_pid(pid: u64) -> bool {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).contains_live_pid(pid) }
}

pub(crate) fn contains_live_process_pid(pid: u64) -> bool {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).contains_live_process_pid(pid) }
}

pub(crate) fn address_space_for_pid(pid: u64) -> Option<usize> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).address_space_for_pid(pid) }
}

pub(crate) fn current_address_space() -> Option<usize> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).current_address_space() }
}

pub(crate) fn current_name(destination: &mut [u8]) -> usize {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).current_name(destination) }
}

pub(crate) fn current_linux_signal_action(signal: usize) -> Option<[u8; LINUX_SIGACTION_BYTES]> {
    if signal == 0 || signal > LINUX_SIGNAL_COUNT {
        return None;
    }
    unsafe {
        Some(
            (*core::ptr::addr_of!(SCHEDULER)).slots[(*core::ptr::addr_of!(SCHEDULER)).current]
                .linux_signal_actions[signal - 1]
                .bytes,
        )
    }
}

pub(crate) fn set_current_linux_signal_action(
    signal: usize,
    bytes: [u8; LINUX_SIGACTION_BYTES],
) -> bool {
    if signal == 0 || signal > LINUX_SIGNAL_COUNT {
        return false;
    }
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.linux_signal_actions[signal - 1].bytes = bytes;
        true
    }
}

pub(crate) fn current_linux_signal_mask() -> Option<u64> {
    unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let slot = &scheduler.slots[scheduler.current];
        (slot.state != TaskState::Empty).then_some(slot.linux_signal_mask)
    }
}

pub(crate) fn set_current_linux_signal_mask(mask: u64) -> bool {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.linux_signal_mask = mask;
        true
    }
}

pub(crate) fn take_current_linux_signalfd_signal(mask: u64) -> Option<u64> {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return None;
        }
        let pending = slot.linux_pending_signals & mask;
        let bit_index = pending.trailing_zeros();
        if bit_index >= LINUX_SIGNAL_COUNT as u32 {
            return None;
        }
        slot.linux_pending_signals &= !(1u64 << bit_index);
        Some(u64::from(bit_index) + 1)
    }
}

pub(crate) fn restore_current_linux_signalfd_signal(signal: u64) -> bool {
    if !(1..=LINUX_SIGNAL_COUNT as u64).contains(&signal) {
        return false;
    }
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        let slot = &mut scheduler.slots[scheduler.current];
        if slot.state == TaskState::Empty {
            return false;
        }
        slot.linux_pending_signals |= 1u64 << (signal - 1);
        true
    }
}

pub(crate) fn current_linux_signalfd_readable(mask: u64) -> bool {
    unsafe {
        let scheduler = &*core::ptr::addr_of!(SCHEDULER);
        let slot = &scheduler.slots[scheduler.current];
        slot.state != TaskState::Empty && slot.linux_pending_signals & mask != 0
    }
}

pub(crate) fn send_linux_process_signal(target_pid: u64, signal: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .queue_linux_signal(target_pid, signal, false)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn send_linux_thread_signal(target_pid: u64, signal: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .queue_linux_signal(target_pid, signal, true)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn send_linux_process_group_signal(pgid: u64, signal: u64, include_init: bool) -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .queue_linux_process_group(pgid, signal, include_init)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn send_linux_all_processes_signal(signal: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .queue_linux_all_processes(signal)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn linux_thread_belongs_to_process(tid: u64, tgid: u64) -> bool {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).linux_thread_belongs_to_process(tid, tgid) }
}

pub(crate) fn deliver_pending_linux_signal(frame: &mut Aarch64TrapFrame) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).take_linux_signal_frame(frame) }
}

pub(crate) fn linux_sigreturn(frame: &mut Aarch64TrapFrame) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).linux_sigreturn(frame) }
}

pub(crate) fn set_current_stack(live: &mut Aarch64TrapFrame, stack_pointer: u64) -> bool {
    unsafe {
        let scheduler = &mut *core::ptr::addr_of_mut!(SCHEDULER);
        if scheduler.slots[scheduler.current].state != TaskState::Runnable {
            return false;
        }
        live.sp_el0 = stack_pointer;
        scheduler.slots[scheduler.current].frame.sp_el0 = stack_pointer;
        true
    }
}

pub(crate) fn current_linux_pgid() -> Option<u64> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).current_linux_pgid() }
}

pub(crate) fn set_linux_pgid(pid: u64, pgid: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .set_linux_pgid(pid, pgid)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn linux_pgid_for(pid: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of!(SCHEDULER))
            .linux_pgid_for(pid)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn linux_sid_for(pid: u64) -> u64 {
    unsafe {
        (*core::ptr::addr_of!(SCHEDULER))
            .linux_sid_for(pid)
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn linux_setsid() -> u64 {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER))
            .linux_setsid()
            .unwrap_or_else(|error| error)
    }
}

pub(crate) fn yield_current(live: &mut Aarch64TrapFrame) -> Option<TaskSwitch> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).yield_current(live) }
}

pub(crate) fn exit_current(live: &mut Aarch64TrapFrame, status: u64) -> Option<TaskSwitch> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).exit_current(live, status) }
}

pub(crate) fn wait_syscall(
    frames: &mut PhysicalFrameAllocator,
    frame: &mut Aarch64TrapFrame,
    pid: u64,
) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).wait_current(frames, frame, pid) }
}

pub(crate) fn wait_linux_syscall(
    frames: &mut PhysicalFrameAllocator,
    frame: &mut Aarch64TrapFrame,
    pid: u64,
    status_address: u64,
) -> bool {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).wait_current_mode(
            frames,
            frame,
            pid,
            true,
            status_address,
            false,
        )
    }
}

pub(crate) fn wait_linux_nonblocking(
    frames: &mut PhysicalFrameAllocator,
    frame: &mut Aarch64TrapFrame,
    pid: u64,
    status_address: u64,
) -> bool {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).wait_current_mode(
            frames,
            frame,
            pid,
            true,
            status_address,
            true,
        )
    }
}

pub(crate) fn block_event(frame: &mut Aarch64TrapFrame, event_slot: u64, timeout_us: u64) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).block_event(frame, event_slot, timeout_us) }
}

pub(crate) fn can_block_sleep() -> bool {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).has_runnable_peer() }
}

pub(crate) fn sleep_syscall(frame: &mut Aarch64TrapFrame, duration_us: u64) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).block_sleep(frame, duration_us) }
}

pub(crate) fn wake_event(event_slot: u64, manual_reset: bool) -> usize {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).wake_event(event_slot, manual_reset) }
}

pub(crate) fn wake_event_count(event_slot: u64, maximum: usize) -> usize {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).wake_event_count(event_slot, maximum) }
}

pub(crate) fn wake_event_timeouts(now_us: u64) -> usize {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).wake_event_timeouts(now_us) }
}

pub(crate) fn map_memory(
    frames: &mut PhysicalFrameAllocator,
    addr_hint: u64,
    length: u64,
    flags: u64,
) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).map_memory(frames, addr_hint, length, flags) }
}

pub(crate) fn map_memory_fixed(
    frames: &mut PhysicalFrameAllocator,
    address: u64,
    length: u64,
    flags: u64,
) -> Result<u64, u64> {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).map_memory_fixed(frames, address, length, flags)
    }
}

pub(crate) fn unmap_memory(
    frames: &mut PhysicalFrameAllocator,
    address: u64,
    length: u64,
) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).unmap_memory(frames, address, length) }
}

pub(crate) fn protect_memory(address: u64, length: u64, protection: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).protect_memory(address, length, protection) }
}

pub(crate) fn reserve_shared_mapping(
    addr_hint: u64,
    length: u64,
    protection: u64,
) -> Result<u64, u64> {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).reserve_shared_mapping(addr_hint, length, protection)
    }
}

pub(crate) fn release_shared_mapping(address: u64, length: u64) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).release_shared_mapping(address, length) }
}

pub(crate) fn release_shared_mapping_for_pid(pid: u64, address: u64, length: u64) -> bool {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).release_shared_mapping_for_pid(pid, address, length)
    }
}

pub(crate) fn fork(
    frames: &mut PhysicalFrameAllocator,
    live: &mut Aarch64TrapFrame,
) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).fork_current(live, frames) }
}

pub(crate) fn create_thread(live: &Aarch64TrapFrame, entry: u64, stack: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).create_thread(live, entry, stack) }
}

pub(crate) fn create_linux_thread(
    live: &Aarch64TrapFrame,
    stack: u64,
    tls: u64,
    clear_child_tid: u64,
) -> Result<u64, u64> {
    unsafe {
        (*core::ptr::addr_of_mut!(SCHEDULER)).create_linux_thread(live, stack, tls, clear_child_tid)
    }
}

pub(crate) fn join_thread(frame: &mut Aarch64TrapFrame, target_pid: u64) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).join_thread(frame, target_pid) }
}

pub(crate) fn mark_thread_detached(target_pid: u64) -> bool {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).mark_thread_detached(target_pid) }
}

pub(crate) fn reap_thread_if_exited(target_pid: u64) -> Option<u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).reap_thread_if_exited(target_pid) }
}

pub(crate) fn open_process_control(pid: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).open_process_control(pid) }
}

pub(crate) fn close_process_control(handle: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).close_process_control(handle) }
}

pub(crate) fn duplicate_process_control(handle: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).duplicate_process_control(handle) }
}

pub(crate) fn transfer_process_control(target_pid: u64, handle: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).transfer_process_control(target_pid, handle) }
}

pub(crate) fn revoke_process_control(handle: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).revoke_process_control(handle) }
}

pub(crate) fn process_control_stop(handle: u64, status: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).process_control_stop(handle, status) }
}

pub(crate) fn process_control_status(handle: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).process_control_status(handle) }
}

pub(crate) fn process_control_assign(handle: u64, supervisor_pid: u64) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).process_control_assign(handle, supervisor_pid) }
}

pub(crate) fn process_control_reap(
    frames: &mut PhysicalFrameAllocator,
    handle: u64,
) -> Result<u64, u64> {
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).process_control_reap(frames, handle) }
}

/// Implement the native `YIELD` syscall without depending on the smoke
/// launcher. A future preemptive scheduler can keep this ABI and replace the
/// bounded slot selection underneath it.
pub(crate) fn yield_syscall(frame: &mut Aarch64TrapFrame) -> bool {
    let current = current_index();
    if let Some(switch) = yield_current(frame) {
        // Keep the cooperative-smoke trace useful without flooding the
        // Android-init path, whose PID 1 deliberately yields while it waits
        // for property-service clients.
        #[cfg(feature = "aarch64-user-smoke")]
        {
            uart::put_hex("user-smoke: yield task=", current as u64);
            uart::put_hex("user-smoke: switch from=", switch.from_index as u64);
            uart::put_hex("user-smoke: switch to=", switch.to_index as u64);
            uart::put_hex("user-smoke: switch from-pid=", switch.from_pid);
            uart::put_hex("user-smoke: switch to-pid=", switch.to_pid);
            uart::put_hex("user-smoke: active ttbr0=", mmu::active_ttbr0());
        }
    } else {
        frame.x[0] = ERR_NOT_SUPPORTED;
    }
    true
}

/// Retire the current task. With no runnable successor, return to an EL1h
/// continuation so the final task has a defined exit boundary.
pub(crate) fn exit_syscall(frame: &mut Aarch64TrapFrame, status: u64) -> bool {
    if let Some(switch) = exit_current(frame, status) {
        uart::put_hex("user-smoke: task exit=", switch.from_index as u64);
        uart::put_hex("user-smoke: task exit-pid=", switch.from_pid);
        uart::put_hex("user-smoke: switch to=", switch.to_index as u64);
        uart::put_hex("user-smoke: switch to-pid=", switch.to_pid);
        uart::put_hex("user-smoke: active ttbr0=", mmu::active_ttbr0());
        return true;
    }
    uart::put_hex("user-smoke: exit=", status);
    frame.x[0] = status;
    frame.elr_el1 = return_from_user as *const () as usize as u64;
    frame.spsr_el1 = (frame.spsr_el1 & !0xf) | 0x5; // EL1h
    true
}

/// Resolve a COW write fault or retire a task that caused another EL0 abort.
pub(crate) fn handle_user_fault(frame: &mut Aarch64TrapFrame) -> bool {
    let current = current_index();
    let esr = frame.esr_el1;
    let far = frame.far_el1;
    if is_user_write_permission_fault(esr) {
        if let Some(space_id) = current_address_space()
            && allocator::with_global(|frames| mmu::resolve_copy_on_write(space_id, far, frames))
                .unwrap_or(false)
        {
            uart::put_hex("user-smoke: cow fault task=", current as u64);
            uart::put_hex("user-smoke: cow fault far=", far);
            uart::puts("user-smoke: cow resolved\n");
            return true;
        }
    }
    uart::put_hex("user-smoke: fault task=", current as u64);
    uart::put_hex("user-smoke: fault esr=", esr);
    uart::put_hex("user-smoke: fault far=", far);
    if let Some(switch) = exit_current(frame, (-(14i64)) as u64) {
        uart::put_hex("user-smoke: fault switch to=", switch.to_index as u64);
        uart::put_hex("user-smoke: fault switch pid=", switch.to_pid);
        uart::put_hex("user-smoke: fault ttbr0=", mmu::active_ttbr0());
        return true;
    }
    frame.x[0] = (-(14i64)) as u64;
    frame.elr_el1 = return_from_user as *const () as usize as u64;
    frame.spsr_el1 = (frame.spsr_el1 & !0xf) | 0x5; // EL1h
    true
}

fn is_user_write_permission_fault(esr: u64) -> bool {
    let exception_class = (esr >> 26) & 0x3f;
    let fault_status = esr & 0x3f;
    matches!(exception_class, 0x24 | 0x25)
        && (esr & (1 << 6)) != 0
        && matches!(fault_status, 0x0c..=0x0f)
}

#[unsafe(no_mangle)]
pub(crate) extern "C" fn return_from_user() -> ! {
    uart::puts("user-smoke: returned to EL1h\n");
    cpu::wait_forever()
}
