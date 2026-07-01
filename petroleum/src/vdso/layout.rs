use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

pub const VDSO_RING_SIZE: usize = 31;

pub const VDSO_FREE: u64 = 0;
pub const VDSO_CLAIMED: u64 = 1;
pub const VDSO_PENDING: u64 = 2;
pub const VDSO_COMPLETE: u64 = 3;

pub const VDSO_USER_BASE: u64 = 0x7000_0000_0000;

#[repr(C, align(128))]
pub struct VdsoRequest {
    syscall_num: UnsafeCell<u64>,
    args: UnsafeCell<[u64; 6]>,
    pub state: AtomicU64,
}

unsafe impl Sync for VdsoRequest {}

impl VdsoRequest {
    pub fn syscall_num(&self) -> u64 {
        unsafe { *self.syscall_num.get() }
    }
    pub fn set_syscall_num(&self, val: u64) {
        unsafe { *self.syscall_num.get() = val; }
    }
    pub fn args(&self) -> [u64; 6] {
        unsafe { *self.args.get() }
    }
    pub fn set_args(&self, val: [u64; 6]) {
        unsafe { *self.args.get() = val; }
    }
    pub fn result(&self) -> u64 {
        self.args()[0]
    }
    pub fn set_result(&self, val: u64) {
        let mut a = self.args();
        a[0] = val;
        self.set_args(a);
    }
}

#[repr(C, align(4096))]
pub struct VdsoPage {
    pub time_us: AtomicU64,
    pub uptime_us: AtomicU64,
    pub pid: u64,
    _pad: [u64; 5],
    pub requests: [VdsoRequest; VDSO_RING_SIZE],
}

impl VdsoPage {
    pub const fn new() -> Self {
        const INIT: VdsoRequest = VdsoRequest {
            syscall_num: UnsafeCell::new(0),
            args: UnsafeCell::new([0; 6]),
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

    pub fn try_claim_slot(&self) -> Option<usize> {
        for i in 0..VDSO_RING_SIZE {
            let state = self.requests[i].state.load(Ordering::Relaxed);
            if state == VDSO_FREE {
                if self.requests[i]
                    .state
                    .compare_exchange(VDSO_FREE, VDSO_CLAIMED, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    return Some(i);
                }
            }
        }
        None
    }

    pub fn claim_slot_spin(&self) -> usize {
        loop {
            if let Some(slot) = self.try_claim_slot() {
                return slot;
            }
            core::hint::spin_loop();
        }
    }

    pub fn submit_request(&self, slot: usize, syscall_num: u64, args: [u64; 6]) {
        let req = &self.requests[slot];
        req.set_syscall_num(syscall_num);
        req.set_args(args);
        req.state.store(VDSO_PENDING, Ordering::Release);
    }

    pub fn poll_completion(&self, slot: usize) -> Option<u64> {
        let state = self.requests[slot].state.load(Ordering::Acquire);
        if state == VDSO_COMPLETE {
            let result = self.requests[slot].result();
            self.requests[slot].state.store(VDSO_FREE, Ordering::Release);
            Some(result)
        } else {
            None
        }
    }
}
