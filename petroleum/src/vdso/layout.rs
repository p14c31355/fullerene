use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const VDSO_RING_SIZE: usize = 32;

/// Per-request status values
pub const VDSO_FREE: u64 = 0;
pub const VDSO_PENDING: u64 = 1;

// VDSO page will be mapped at this virtual address in every user process.
// We reserve a fixed address in the user-accessible half of the address space.
pub const VDSO_USER_BASE: u64 = 0x7000_0000_0000;

/// A single VDSO request slot
#[repr(C, align(128))]
pub struct VdsoRequest {
    pub syscall_num: u64,
    pub args: [u64; 6],
    /// state transition: FREE(0) → user writes PENDING(1) → kernel writes result (≥2) → user reads & resets to FREE
    pub state: AtomicU64,
}

/// Shared VDSO page structure.
/// Mapped into every user process at `VDSO_USER_BASE`.
/// Kernel maps the same physical page in its higher-half.
#[repr(C, align(4096))]
pub struct VdsoPage {
    // ── Read-only data (kernel → user) ──
    pub time_us: AtomicU64,
    pub uptime_us: AtomicU64,
    pub pid: u64,
    pub _pad: [u64; 5],

    // ── Request slots (user submits, kernel processes) ──
    pub requests: [VdsoRequest; VDSO_RING_SIZE],
}

impl VdsoPage {
    pub const fn new() -> Self {
        const INIT: VdsoRequest = VdsoRequest {
            syscall_num: 0,
            args: [0; 6],
            state: AtomicU64::new(0),
        };
        const INIT_REQS: [VdsoRequest; VDSO_RING_SIZE] = [INIT; VDSO_RING_SIZE];
        VdsoPage {
            time_us: AtomicU64::new(0),
            uptime_us: AtomicU64::new(0),
            pid: 0,
            _pad: [0; 5],
            requests: INIT_REQS,
        }
    }

    /// Try to claim a free slot.
    /// Returns `Some(slot_index)` on success, `None` if all slots are busy.
    pub fn try_claim_slot(&self) -> Option<usize> {
        for i in 0..VDSO_RING_SIZE {
            let state = self.requests[i].state.load(Ordering::Relaxed);
            if state == VDSO_FREE {
                if self.requests[i]
                    .state
                    .compare_exchange_weak(VDSO_FREE, VDSO_PENDING, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Spin until a slot is free, then claim it.
    /// In the full-VDSO model this should be called with yield_now() rather than
    /// busy-waiting. This variant exists for non-async contexts.
    pub fn claim_slot_spin(&self) -> usize {
        loop {
            if let Some(slot) = self.try_claim_slot() {
                return slot;
            }
            core::hint::spin_loop();
        }
    }

    /// Fill and commit a request in one step.
    pub fn submit_request(&self, slot: usize, syscall_num: u64, args: [u64; 6]) {
        let req = &self.requests[slot];
        req.syscall_num = syscall_num;
        req.args = args;
        // Release barrier: all writes above are visible before state change
        req.state.store(VDSO_PENDING, Ordering::Release);
    }

    /// Check if a request has completed.
    /// Returns `None` if still pending, `Some(result)` if done.
    pub fn poll_completion(&self, slot: usize) -> Option<u64> {
        let state = self.requests[slot].state.load(Ordering::Acquire);
        if state >= 2 {
            let result = self.requests[slot].args[0]; // reuse args[0] for result
            self.requests[slot].state.store(VDSO_FREE, Ordering::Release);
            Some(result)
        } else {
            None
        }
    }
}
