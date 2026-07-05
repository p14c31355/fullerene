//! Kernel tracing — lightweight event buffer for post-mortem analysis.
//!
//! Records timestamped events (function entry/exit, IRQ, exception, syscall)
//! in a fixed-size ring buffer accessible via the `trace` shell command.
//!
//! # Safety: single-core assumption
//!
//! The ring buffer uses a single `static mut TRACE_BUFFER` protected only
//! by an atomic head index.  This is safe ONLY under the current
//! single‑core / single‑CPU assumption.  On a multi‑CPU system, concurrent
//! `record()` calls from different cores would race on both the head index
//! and the buffer slots, causing lost/corrupted events.
//!
//! To add multi‑CPU support, replace `TRACE_BUFFER` with per‑CPU buffers
//! or a lock‑free MPSC queue and make `TRACE_HEAD` per‑CPU.
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
    /// Monotonically increasing sequence number (for ordering / reorder detection).
    pub seq: u64,
    /// System tick when the event was recorded.
    pub tick: u64,
    /// Category tag (max 8 chars, NUL-padded).
    pub category: [u8; 8],
    /// Event message (max 32 chars, NUL-padded).
    pub message: [u8; 32],
}

impl TraceEvent {
    /// Create a new trace event with the given category and message.
    pub fn new(seq: u64, tick: u64, cat: &str, msg: &str) -> Self {
        let mut category = [0u8; 8];
        let mut message = [0u8; 32];
        let cat_bytes = cat.as_bytes();
        let msg_bytes = msg.as_bytes();
        for i in 0..cat_bytes.len().min(8) {
            category[i] = cat_bytes[i];
        }
        for i in 0..msg_bytes.len().min(32) {
            message[i] = msg_bytes[i];
        }
        Self {
            seq,
            tick,
            category,
            message,
        }
    }
}

// ── Ring buffer ───────────────────────────────────────────────────

/// Pre-allocated trace buffer (BSS, zero initialised).
static mut TRACE_BUFFER: [TraceEvent; TRACE_CAPACITY] = [TraceEvent {
    seq: 0,
    tick: 0,
    category: [0u8; 8],
    message: [0u8; 32],
}; TRACE_CAPACITY];

/// Write index (atomic, monotonically increasing).
static TRACE_HEAD: AtomicUsize = AtomicUsize::new(0);

/// Global sequence counter (monotonically increasing, never wraps).
static TRACE_SEQ: AtomicUsize = AtomicUsize::new(0);

// ── Public API ────────────────────────────────────────────────────

/// Record a trace event.
///
/// This is lock-free and interruption-safe — suitable for use inside
/// IRQ handlers and exception handlers.  The sequence number helps
/// detect reorder or loss in a future multi‑CPU implementation.
pub fn record(tick: u64, category: &str, message: &str) {
    let idx = TRACE_HEAD.fetch_add(1, Ordering::Relaxed) % TRACE_CAPACITY;
    let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed) as u64;
    unsafe {
        TRACE_BUFFER[idx] = TraceEvent::new(seq, tick, category, message);
    }
}

/// Return all trace events in chronological order as an owned vector.
///
/// The returned `Vec` is a snapshot — safe to hold across interrupt
/// boundaries because it owns its data.
pub fn snapshot() -> alloc::vec::Vec<TraceEvent> {
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let head = TRACE_HEAD.load(Ordering::Relaxed);
        if head == 0 {
            return alloc::vec::Vec::new();
        }
        let mut result = alloc::vec::Vec::new();
        if head <= TRACE_CAPACITY {
            result.extend_from_slice(&TRACE_BUFFER[..head]);
        } else {
            let start = head % TRACE_CAPACITY;
            result.extend_from_slice(&TRACE_BUFFER[start..]);
            result.extend_from_slice(&TRACE_BUFFER[..start]);
        }
        result
    })
}

/// Clear all trace events.
pub fn clear() {
    TRACE_HEAD.store(0, Ordering::Relaxed);
}

/// Return the number of events currently stored.
pub fn len() -> usize {
    TRACE_HEAD.load(Ordering::Relaxed).min(TRACE_CAPACITY)
}
