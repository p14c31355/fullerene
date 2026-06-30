use core::future::Future;
use core::pin::Pin;
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
    VDSO_PAGE = page;
}

/// Check whether the VDSO pointer has been initialized.
pub fn vdso_ptr_initialized() -> bool {
    !unsafe { VDSO_PAGE }.is_null()
}

fn vdso() -> &'static VdsoPage {
    unsafe { &*VDSO_PAGE }
}

// ── Synchronous (blocking) calls ─────────────────────────────────

/// Blocking VDSO call — spins until completion.
/// Use only when the async executor is not available.
pub fn vdso_call_blocking(syscall_num: u64, args: [u64; 6]) -> u64 {
    let page = vdso();
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
    let page = vdso();
    let slot = page.try_claim_slot()?;
    page.submit_request(slot, syscall_num, args);
    page.poll_completion(slot)
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

impl Future for VdsoFuture {
    type Output = u64;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u64> {
        let this = unsafe { self.get_unchecked_mut() };
        let page = vdso();

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
                        // All slots full — register waker and retry
                        // In the full-VDSO model, the kernel will process slots
                        // and the scheduler will re-poll us.
                        // We use cx.waker() to ensure we get polled again.
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }
            }
        };

        // Phase 2: check completion
        match page.poll_completion(slot) {
            Some(result) => Poll::Ready(result),
            None => {
                // Register waker so kernel can wake us when done
                // The kernel will call cx.waker().wake() when processing
                // completes. For now, we use yield_now()-style wake-up.
                cx.waker().wake_by_ref();
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
    vdso().uptime_us.load(Ordering::Relaxed)
}

/// Get current wall-clock time in microseconds — no kernel transition.
pub fn vdso_time_us() -> u64 {
    vdso().time_us.load(Ordering::Acquire)
}

/// Get current PID — no kernel transition.
pub fn vdso_pid() -> u64 {
    vdso().pid
}
