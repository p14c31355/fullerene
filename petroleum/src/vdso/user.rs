use core::sync::atomic::{AtomicUsize, Ordering};

use crate::vdso::layout::*;

/// Global VDSO page pointer, set once during process initialization.
/// In user-space processes, this points to `VDSO_USER_BASE`.
/// In kernel processes, this is set by the kernel during boot.
#[allow(static_mut_refs)]
static VDSO_PAGE: AtomicUsize = AtomicUsize::new(0);

/// Initialize the VDSO pointer.
/// Must be called once at process start.
pub unsafe fn init_vdso(page: *const VdsoPage) {
    VDSO_PAGE.store(page as usize, Ordering::Release);
}

/// Check whether the VDSO pointer has been initialized.
pub fn vdso_ptr_initialized() -> bool {
    VDSO_PAGE.load(Ordering::Acquire) != 0
}

fn vdso() -> Option<&'static VdsoPage> {
    let address = VDSO_PAGE.load(Ordering::Acquire);
    if address == 0 {
        None
    } else {
        Some(unsafe { &*(address as *const VdsoPage) })
    }
}

// ── Fast read-only accessors (zero syscall) ──────────────────────

/// Get monotonic uptime in microseconds — no kernel transition.
pub fn vdso_uptime_us() -> u64 {
    vdso()
        .map(|p| p.uptime_us.load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Get current wall-clock time in microseconds — no kernel transition.
pub fn vdso_time_us() -> u64 {
    vdso()
        .map(|p| p.time_us.load(Ordering::Acquire))
        .unwrap_or(0)
}

/// Get current PID — no kernel transition.
pub fn vdso_pid() -> u64 {
    vdso().map(|p| p.pid).unwrap_or(0)
}
