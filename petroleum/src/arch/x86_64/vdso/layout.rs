use core::sync::atomic::AtomicU64;

/// VDSO page mapped into every user-space process at `VDSO_USER_BASE`.
///
/// # Layout
///
/// The page is **read-only from user space** and **write-only from kernel
/// space**.  No ring / slot machinery — just metadata that the kernel
/// publishes and user-space reads atomically.
///
/// ```text
/// Offset  │ Contents
/// ────────┼────────────────────────────────────────
///    0    │ time_us   (AtomicU64 — wall clock µs)
///    8    │ uptime_us (AtomicU64 — monotonic µs)
///   16    │ pid       (u64 — process identifier)
///   24–511│ pad / reserved
/// ```
pub const VDSO_USER_BASE: u64 = 0x7000_0000_0000;

#[repr(C, align(4096))]
pub struct VdsoPage {
    /// Wall-clock time in microseconds (kernel writes, user reads via
    /// `Ordering::Acquire`).
    pub time_us: AtomicU64,

    /// Monotonic uptime in microseconds (kernel writes, user reads via
    /// `Ordering::Relaxed`).
    pub uptime_us: AtomicU64,

    /// Process ID, set once at creation and never modified.
    pub pid: u64,

    /// Reserved for future read-only fields.
    _reserved: [u64; 509],
}

impl VdsoPage {
    pub const fn new() -> Self {
        VdsoPage {
            time_us: AtomicU64::new(0),
            uptime_us: AtomicU64::new(0),
            pid: 0,
            _reserved: [0; 509],
        }
    }
}
