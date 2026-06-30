use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::Ordering;
use core::task::{Context, Poll};

use crate::vdso::layout::*;

/// Global VDSO page pointer, set once during process initialization.
/// In user-space processes, this points to `VDSO_USER_BASE`.
/// In kernel processes, this is set by the kernel during boot.
#[allow(static_mut_refs)]
static mut VDSO_PAGE: *const VdsoPage = core::ptr::null();

/// Initialize the VDSO pointer.
/// Must be called once at process start.
pub unsafe fn init_vdso(page: *const VdsoPage) {
    unsafe { VDSO_PAGE = page; }
}

/// Check whether the VDSO pointer has been initialized.
pub fn vdso_ptr_initialized() -> bool {
    !unsafe { VDSO_PAGE }.is_null()
}

fn vdso() -> Option<&'static VdsoPage> {
    let ptr = unsafe { VDSO_PAGE };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

// ── Synchronous (blocking) calls ─────────────────────────────────

/// Blocking VDSO call — spins until completion.
/// Use only when the async executor is not available.
pub fn vdso_call_blocking(syscall_num: u64, args: [u64; 6]) -> u64 {
    let page = vdso().expect("VDSO not initialized");
    let slot = page.claim_slot_spin();
    page.submit_request(slot, syscall_num, args);
    loop {
        if let Some(result) = page.poll_completion(slot) {
            return result;
        }
        core::hint::spin_loop();
    }
}

/// Non-blocking try: submit and check once.
pub fn vdso_try_call(syscall_num: u64, args: [u64; 6]) -> Option<u64> {
    let page = vdso()?;
    let slot = page.try_claim_slot()?;
    page.submit_request(slot, syscall_num, args);
    let result = page.poll_completion(slot);
    if result.is_none() {
        // Slot remains claimed but incomplete — caller gets no handle.
        // Reset to free to avoid leaking the slot.
        page.requests[slot].state.store(VDSO_FREE, Ordering::Release);
    }
    result
}

// ── Async calls ──────────────────────────────────────────────────

/// Future returned by `vdso_call_async`.
pub struct VdsoFuture {
    slot: Option<usize>,
    syscall_num: u64,
    args: [u64; 6],
}

impl VdsoFuture {
    pub fn new(syscall_num: u64, args: [u64; 6]) -> Self {
        VdsoFuture { slot: None, syscall_num, args }
    }
}

impl Drop for VdsoFuture {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            if let Some(page) = vdso() {
                page.requests[slot].state.store(VDSO_FREE, Ordering::Release);
            }
        }
    }
}

impl Future for VdsoFuture {
    type Output = u64;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u64> {
        let this = unsafe { self.get_unchecked_mut() };
        let page = match vdso() {
            Some(p) => p,
            None => return Poll::Pending,
        };

        // Phase 1: claim a slot
        let slot = match this.slot {
            Some(s) => s,
            None => {
                match page.try_claim_slot() {
                    Some(s) => {
                        page.submit_request(s, this.syscall_num, this.args);
                        this.slot = Some(s);
                        s
                    }
                    None => {
                        // All slots full — return Pending without waking.
                        // The kernel's periodic poll_all_vdso_rings() will
                        // eventually free a slot, and the executor will
                        // re-poll us on the next scheduler tick.
                        return Poll::Pending;
                    }
                }
            }
        };

        // Phase 2: check completion
        match page.poll_completion(slot) {
            Some(result) => Poll::Ready(result),
            None => {
                // Not yet complete.  The kernel processes VDSO requests
                // in its idle loop; once completed, the slot state
                // transitions to VDSO_COMPLETE.  We return Pending and
                // rely on the executor to re-poll us (e.g. on timer tick).
                // TODO: wire real waker notification from kernel completion path.
                Poll::Pending
            }
        }
    }
}

/// Submit a VDSO request and get a Future for completion.
pub fn vdso_call_async(syscall_num: u64, args: [u64; 6]) -> VdsoFuture {
    VdsoFuture::new(syscall_num, args)
}

// ── Fast read-only accessors (zero syscall) ──────────────────────

/// Get monotonic uptime in microseconds — no kernel transition.
pub fn vdso_uptime_us() -> u64 {
    vdso().map(|p| p.uptime_us.load(Ordering::Relaxed)).unwrap_or(0)
}

/// Get current wall-clock time in microseconds — no kernel transition.
pub fn vdso_time_us() -> u64 {
    vdso().map(|p| p.time_us.load(Ordering::Acquire)).unwrap_or(0)
}

/// Get current PID — no kernel transition.
pub fn vdso_pid() -> u64 {
    vdso().map(|p| p.pid).unwrap_or(0)
}
