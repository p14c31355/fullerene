//! Saved user contexts for the first AArch64 cooperative scheduler boundary.

use super::{
    allocator, allocator::PhysicalFrameAllocator, cpu, elf, exceptions::Aarch64TrapFrame, fs, mmu,
    timer, uart,
};

pub(crate) const MAX_TASKS: usize = 8;
const MAX_TASK_NAME: usize = 16;
const ERR_NOT_SUPPORTED: u64 = (-(95i64)) as u64;
const ERR_PERMISSION_DENIED: u64 = (-(1i64)) as u64;
const ERR_NO_SUCH_PROCESS: u64 = (-(3i64)) as u64;
const ERR_BAD_HANDLE: u64 = (-(104i64)) as u64;
const ERR_WOULD_BLOCK: u64 = (-(140i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OUT_OF_MEMORY: u64 = (-(12i64)) as u64;
const ERR_OVERFLOW: u64 = (-(75i64)) as u64;
const ERR_TIMED_OUT: u64 = (-(110i64)) as u64;
const PROCESS_HANDLE_TAG: u64 = 1 << 63;
const PROCESS_HANDLE_OWNER_SHIFT: u64 = 32;
const PROCESS_HANDLE_GENERATION_SHIFT: u64 = 16;
const PROCESS_HANDLE_GENERATION_MASK: u64 = 0xffff;
const PROCESS_HANDLE_PID_MASK: u64 = 0xffff;
const MAX_PROCESS_CONTROLS: usize = 8;
const INIT_PID: u64 = 1;
const STACK_ADDRESS: u64 = 0x41ff_0000;
const PAGE_SIZE: u64 = 4096;
const DYNAMIC_MEMORY_START: u64 = 0x4002_0000;
// Keep this below mmu::USER_SPACE_END. The stack occupies the final page of
// the bounded user window, leaving the lower range available for mappings.
const DYNAMIC_MEMORY_END: u64 = STACK_ADDRESS;
const MAX_MEMORY_MAPPINGS: usize = 8;
const MAX_MEMORY_PAGES: usize = 64;

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
    is_thread: bool,
    thread_detached: bool,
    name: [u8; MAX_TASK_NAME],
    name_len: usize,
    parent_pid: u64,
    supervisor_pid: u64,
    wait_target: u64,
    thread_wait_target: u64,
    wait_event: u64,
    sleep_wait: bool,
    wait_deadline_us: u64,
    exit_status: u64,
    terminal_handle: u64,
    process_controls: [ProcessControlEntry; MAX_PROCESS_CONTROLS],
    next_process_control_generation: u16,
    mappings: [UserMapping; MAX_MEMORY_MAPPINGS],
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
        },
        pid: 0,
        address_space: 0,
        is_thread: false,
        thread_detached: false,
        name: [0; MAX_TASK_NAME],
        name_len: 0,
        parent_pid: 0,
        supervisor_pid: 0,
        wait_target: 0,
        thread_wait_target: 0,
        wait_event: 0,
        sleep_wait: false,
        wait_deadline_us: 0,
        exit_status: 0,
        terminal_handle: 0,
        process_controls: [ProcessControlEntry::EMPTY; MAX_PROCESS_CONTROLS],
        next_process_control_generation: 1,
        mappings: [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS],
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
        slot.is_thread = false;
        slot.thread_detached = false;
        slot.name = [0; MAX_TASK_NAME];
        slot.name[..name.len()].copy_from_slice(name);
        slot.name_len = name.len();
        slot.parent_pid = 0;
        slot.supervisor_pid = 0;
        slot.wait_target = 0;
        slot.thread_wait_target = 0;
        slot.wait_event = 0;
        slot.sleep_wait = false;
        slot.wait_deadline_us = 0;
        slot.exit_status = 0;
        slot.terminal_handle = 0;
        slot.process_controls = [ProcessControlEntry::EMPTY; MAX_PROCESS_CONTROLS];
        slot.next_process_control_generation = 1;
        slot.mappings = [UserMapping::EMPTY; MAX_MEMORY_MAPPINGS];
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
    ) -> bool {
        if !self.install(index, pid, name, address_space, frame) {
            return false;
        }
        self.slots[index].parent_pid = parent_pid;
        self.slots[index].supervisor_pid = supervisor_pid;
        self.slots[index].terminal_handle = terminal_handle;
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
        self.slots[index].parent_pid = owner_pid;
        self.slots[index].supervisor_pid = owner_pid;
        self.slots[index].terminal_handle = self
            .slots
            .iter()
            .find(|slot| slot.pid == owner_pid && !slot.is_thread)
            .map(|slot| slot.terminal_handle)
            .unwrap_or(0);
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
        let mut direct_wait = false;
        if !is_thread {
            for slot in &mut self.slots {
                if slot.state == TaskState::Blocked
                    && !slot.is_thread
                    && (slot.wait_target == from_pid || slot.wait_target == u64::MAX)
                {
                    slot.frame.x[0] = status;
                    slot.wait_target = 0;
                    slot.wait_event = 0;
                    slot.sleep_wait = false;
                    slot.wait_deadline_us = 0;
                    slot.state = TaskState::Runnable;
                    direct_wait = true;
                }
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
                        self.slots[init_index].frame.x[0] = self.slots[child_index].exit_status;
                        self.slots[init_index].wait_target = 0;
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
            let status = self.slots[child_index].exit_status;
            if self.reap_exited(child_index, frames).is_err() {
                frame.x[0] = ERR_OUT_OF_MEMORY;
            } else {
                frame.x[0] = status;
            }
            return true;
        }

        let current = self.current;
        self.slots[current].frame = *frame;
        self.slots[current].wait_target = pid;
        self.slots[current].wait_event = 0;
        self.slots[current].sleep_wait = false;
        self.slots[current].wait_deadline_us = 0;
        self.slots[current].state = TaskState::Blocked;
        let Some(next) = self.next_active(current) else {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_target = 0;
            frame.x[0] = ERR_NOT_SUPPORTED;
            return true;
        };
        if !mmu::activate_user_space(self.slots[next].address_space) {
            self.slots[current].state = TaskState::Runnable;
            self.slots[current].wait_target = 0;
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
            if !manual_reset {
                break;
            }
        }
        woken
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

    fn map_memory(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        addr_hint: u64,
        length: u64,
        flags: u64,
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
                || self.mapping_overlaps(task_index, addr_hint, rounded_length)
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
        if !mmu::activate_user_space(address_space) {
            for index in 0..page_count {
                if let Some(physical) =
                    mmu::unmap_user_page(address_space, base + index as u64 * PAGE_SIZE)
                {
                    let _ = frames.release_frame(physical);
                }
            }
            self.slots[task_index].mappings[mapping_index] = UserMapping::EMPTY;
            return Err(ERR_NOT_SUPPORTED);
        }
        Ok(base)
    }

    fn unmap_memory(
        &mut self,
        frames: &mut PhysicalFrameAllocator,
        address: u64,
        length: u64,
    ) -> Result<u64, u64> {
        let length = round_memory_length(length)?;
        let task_index = self.current;
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
        Ok(0)
    }

    fn protect_memory(&mut self, address: u64, length: u64, protection: u64) -> Result<u64, u64> {
        let length = round_memory_length(length)?;
        if protection & !0x7 != 0 {
            return Err(ERR_INVALID);
        }
        let task_index = self.current;
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
        let name = self.slots[parent_index].name;
        let name_len = self.slots[parent_index].name_len;
        let mappings = self.slots[parent_index].mappings;
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
        ) {
            fs::drop_owner(child_pid);
            let _ =
                mmu::release_user_space(child_space, frames, &shared_ranges[..shared_range_count]);
            return Err(ERR_OUT_OF_MEMORY);
        }
        self.slots[child_index].mappings = mappings;
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
    unsafe { (*core::ptr::addr_of_mut!(SCHEDULER)).install(index, pid, name, address_space, frame) }
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
        elr_el1: image.entry,
        spsr_el1: 0,
        sp_el0: STACK_ADDRESS + PAGE_SIZE - 16,
        esr_el1: 0,
        far_el1: 0,
    };
    let parent_pid = unsafe {
        (*core::ptr::addr_of!(SCHEDULER))
            .current_pid()
            .ok_or(ERR_NO_SUCH_PROCESS)?
    };
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
) -> Result<(), u64> {
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
    new_frame.elr_el1 = loaded.entry;
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
    }
    *live = new_frame;
    uart::put_hex("aarch64 exec pid=", pid);
    uart::put_hex("aarch64 exec pages=", loaded.page_count as u64);
    Ok(())
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

pub(crate) fn is_process_control_handle(handle: u64) -> bool {
    handle & PROCESS_HANDLE_TAG != 0
}

pub(crate) fn resource_owner_pid() -> Option<u64> {
    unsafe { (*core::ptr::addr_of!(SCHEDULER)).resource_owner_pid() }
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
        uart::put_hex("user-smoke: yield task=", current as u64);
        uart::put_hex("user-smoke: switch from=", switch.from_index as u64);
        uart::put_hex("user-smoke: switch to=", switch.to_index as u64);
        uart::put_hex("user-smoke: switch from-pid=", switch.from_pid);
        uart::put_hex("user-smoke: switch to-pid=", switch.to_pid);
        uart::put_hex("user-smoke: active ttbr0=", mmu::active_ttbr0());
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
