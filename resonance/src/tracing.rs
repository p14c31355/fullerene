//! Kernel tracing — lightweight event buffer for post-mortem analysis.
//!
//! Records timestamped events (function entry/exit, IRQ, exception, syscall)
//! in a fixed-size ring buffer accessible via the `trace` shell command.
//!
//! # Concurrency
//!
//! All ring state is owned by one spin lock. Kernel builds disable local
//! interrupts before taking that lock, preventing an IRQ from interrupting a
//! lock holder and re-entering [`record`] on the same CPU. Writers on other
//! CPUs serialize through the same lock.
//!
//! Recording from NMI context is unsupported: an NMI can interrupt a CPU that
//! already owns the lock and cannot safely spin waiting for itself.
//!
//! # Usage
//!
//! ```ignore
//! trace_event!("irq", "timer tick");
//! trace_event!("syscall", "write fd=1 len=12");
//! ```

use spin::Mutex;

/// Maximum number of trace events stored in the ring buffer.
const TRACE_CAPACITY: usize = 1024;

/// A single trace event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        let category_len = cat_bytes.len().min(category.len());
        category[..category_len].copy_from_slice(&cat_bytes[..category_len]);
        let message_len = msg_bytes.len().min(message.len());
        message[..message_len].copy_from_slice(&msg_bytes[..message_len]);
        Self {
            seq,
            tick,
            category,
            message,
        }
    }
}

const EMPTY_EVENT: TraceEvent = TraceEvent {
    seq: 0,
    tick: 0,
    category: [0u8; 8],
    message: [0u8; 32],
};

struct TraceBuffer {
    events: [TraceEvent; TRACE_CAPACITY],
    next_index: usize,
    len: usize,
    next_sequence: u64,
}

impl TraceBuffer {
    const fn new() -> Self {
        Self {
            events: [EMPTY_EVENT; TRACE_CAPACITY],
            next_index: 0,
            len: 0,
            next_sequence: 0,
        }
    }

    fn record(&mut self, tick: u64, category: &str, message: &str) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.events[self.next_index] = TraceEvent::new(sequence, tick, category, message);
        self.next_index = (self.next_index + 1) % TRACE_CAPACITY;
        self.len = self.len.saturating_add(1).min(TRACE_CAPACITY);
    }

    fn snapshot(&self) -> alloc::vec::Vec<TraceEvent> {
        let mut result = alloc::vec::Vec::with_capacity(self.len);
        if self.len < TRACE_CAPACITY {
            result.extend_from_slice(&self.events[..self.len]);
        } else {
            result.extend_from_slice(&self.events[self.next_index..]);
            result.extend_from_slice(&self.events[..self.next_index]);
        }
        result
    }

    fn clear(&mut self) {
        self.next_index = 0;
        self.len = 0;
    }
}

static TRACE_BUFFER: Mutex<TraceBuffer> = Mutex::new(TraceBuffer::new());

fn with_trace_buffer<R>(f: impl FnOnce(&mut TraceBuffer) -> R) -> R {
    #[cfg(target_os = "uefi")]
    {
        x86_64::instructions::interrupts::without_interrupts(|| f(&mut TRACE_BUFFER.lock()))
    }
    #[cfg(not(target_os = "uefi"))]
    {
        f(&mut TRACE_BUFFER.lock())
    }
}

/// Record a trace event.
///
/// This is allocation-free and safe for normal IRQ/exception handlers. It
/// must not be called from NMI context because the interrupted CPU may already
/// own the trace lock.
pub fn record(tick: u64, category: &str, message: &str) {
    with_trace_buffer(|buffer| buffer.record(tick, category, message));
}

/// Return all trace events in chronological order as an owned vector.
///
/// The returned `Vec` is safe to hold across interrupt boundaries because it
/// owns its data and is copied while the trace lock is held.
pub fn snapshot() -> alloc::vec::Vec<TraceEvent> {
    with_trace_buffer(|buffer| buffer.snapshot())
}

/// Clear all trace events without reusing sequence numbers.
pub fn clear() {
    with_trace_buffer(TraceBuffer::clear);
}

/// Return the number of events currently stored.
pub fn len() -> usize {
    with_trace_buffer(|buffer| buffer.len)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::thread;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn snapshot_keeps_chronological_order_after_wrap() {
        let _test = TEST_LOCK.lock().unwrap();
        clear();
        for tick in 0..TRACE_CAPACITY + 5 {
            record(tick as u64, "test", "wrapped");
        }

        let events = snapshot();
        assert_eq!(events.len(), TRACE_CAPACITY);
        assert_eq!(events.first().unwrap().tick, 5);
        assert_eq!(events.last().unwrap().tick, (TRACE_CAPACITY + 4) as u64);
        assert!(events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    }

    #[test]
    fn clear_empties_events_without_reusing_sequence_numbers() {
        let _test = TEST_LOCK.lock().unwrap();
        clear();
        record(1, "test", "before clear");
        let previous_sequence = snapshot()[0].seq;

        clear();
        assert_eq!(len(), 0);
        assert!(snapshot().is_empty());
        record(2, "test", "after clear");
        assert!(snapshot()[0].seq > previous_sequence);
    }

    #[test]
    fn concurrent_writers_publish_complete_ordered_events() {
        let _test = TEST_LOCK.lock().unwrap();
        clear();
        let writers: alloc::vec::Vec<_> = (0..4)
            .map(|writer| {
                thread::spawn(move || {
                    for event in 0..400 {
                        record(writer * 400 + event, "worker", "concurrent event");
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let events = snapshot();
        assert_eq!(events.len(), TRACE_CAPACITY);
        assert!(events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
        assert!(events.iter().all(|event| &event.category[..6] == b"worker"));
        assert!(
            events
                .iter()
                .all(|event| &event.message[..16] == b"concurrent event")
        );
    }
}
