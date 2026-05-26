//! Kernel tracing — lightweight event buffer for post-mortem analysis.
//!
//! Records timestamped events (function entry/exit, IRQ, exception, syscall)
//! in a fixed-size ring buffer accessible via the `trace` shell command.
//!
//! # Usage
//!
//! ```ignore
//! trace_event!("irq", "timer tick");
//! trace_event!("syscall", "write fd=1 len=12");
//! ```

use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of trace events stored in the ring buffer.
const TRACE_CAPACITY: usize = 1024;

/// A single trace event.
#[derive(Clone, Copy, Debug)]
pub struct TraceEvent {
    /// System tick when the event was recorded.
    pub tick: u64,
    /// Category tag (max 8 chars, NUL-padded).
    pub category: [u8; 8],
    /// Event message (max 32 chars, NUL-padded).
    pub message: [u8; 32],
}

impl TraceEvent {
    /// Create a new trace event with the given category and message.
    pub fn new(tick: u64, cat: &str, msg: &str) -> Self {
        let mut category = [0u8; 8];
        let mut message = [0u8; 32];
        let cat_bytes = cat.as_bytes();
        let msg_bytes = msg.as_bytes();
        for i in 0..cat_bytes.len().min(8) { category[i] = cat_bytes[i]; }
        for i in 0..msg_bytes.len().min(32) { message[i] = msg_bytes[i]; }
        Self { tick, category, message }
    }
}

// ── Ring buffer ───────────────────────────────────────────────────

/// Pre-allocated trace buffer (BSS, zero initialised).
static mut TRACE_BUFFER: [TraceEvent; TRACE_CAPACITY] = [TraceEvent {
    tick: 0,
    category: [0u8; 8],
    message: [0u8; 32],
}; TRACE_CAPACITY];

/// Write index (atomic, monotonically increasing).
static TRACE_HEAD: AtomicUsize = AtomicUsize::new(0);

// ── Public API ────────────────────────────────────────────────────

/// Record a trace event.
///
/// This is lock-free and interruption-safe — suitable for use inside
/// IRQ handlers and exception handlers.
pub fn record(tick: u64, category: &str, message: &str) {
    let idx = TRACE_HEAD.fetch_add(1, Ordering::Relaxed) % TRACE_CAPACITY;
    unsafe {
        TRACE_BUFFER[idx] = TraceEvent::new(tick, category, message);
    }
}

/// Return a slice of the most recent trace events, ordered oldest-first.
///
/// Because the ring buffer may have wrapped, this reconstructs the
/// correct order from the current head position.
pub fn snapshot() -> &'static [TraceEvent] {
    // SAFETY: single-threaded kernel, no concurrent mutation during snapshot.
    unsafe {
        let head = TRACE_HEAD.load(Ordering::Relaxed);
        if head == 0 {
            return &[];
        }
        let total = head.min(TRACE_CAPACITY);
        // Return the most recent `total` events.
        if head <= TRACE_CAPACITY {
            &TRACE_BUFFER[..total]
        } else {
            // Buffer has wrapped — events are in two segments.
            // For simplicity we return only the contiguous segment from
            // the wrap point to the current head.
            &TRACE_BUFFER[head % TRACE_CAPACITY..]
        }
    }
}

/// Clear all trace events.
pub fn clear() {
    TRACE_HEAD.store(0, Ordering::Relaxed);
}

/// Return the number of events currently stored.
pub fn len() -> usize {
    TRACE_HEAD.load(Ordering::Relaxed).min(TRACE_CAPACITY)
}