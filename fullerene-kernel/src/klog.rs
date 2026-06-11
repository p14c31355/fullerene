//! Kernel log buffer — persistent text ring buffer for kernel messages.
//!
//! Captures `log::info!()` / `log::warn!()` / `log::error!()` calls so they
//! can be viewed from the Nozzle shell via `dmesg`, even when a serial
//! console is not attached (e.g. on real hardware).
//!
//! # Usage
//!
//! ```ignore
//! klog!("Sound: codec inventory dump\n");
//! klog_fmt!(format_args!("Sound: DAC 0x{:x} amp=0x{:08x}\n", dac, amp));
//! ```

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// Maximum number of bytes in the ring buffer.
const KLOG_CAPACITY: usize = 65536;

/// A fixed-size byte ring buffer for kernel log lines.
static KLOG_BUF: Mutex<KLogRing> = Mutex::new(KLogRing {
    buf: [0u8; KLOG_CAPACITY],
    head: 0,
    len: 0,
});

struct KLogRing {
    buf: [u8; KLOG_CAPACITY],
    head: usize,
    len: usize,
}

/// Reentrancy guard: set to `true` while `write_fmt` or `write_bytes`
/// is holding the `KLOG_BUF` lock.  Nested calls (e.g. a log message
/// emitted while formatting another log message) are forwarded to the
/// serial port instead of attempting to re-acquire the mutex.
static IN_KLOG: AtomicBool = AtomicBool::new(false);

/// Write a formatted message to the kernel log buffer.
///
/// This is the primary entry point.  Use it like `write!`:
///
/// ```ignore
/// klog_fmt!(format_args!("Sound: Hello {}\n", name));
/// ```
pub fn write_fmt(args: fmt::Arguments<'_>) {
    if IN_KLOG.swap(true, Ordering::Acquire) {
        // Reentrant call — fall back to serial output to avoid deadlock.
        petroleum::serial::serial_log(args);
        return;
    }
    let mut guard = KLOG_BUF.lock();
    let ring = &mut *guard;
    let mut writer = KLogWriter { ring, pos: 0 };
    let _ = fmt::Write::write_fmt(&mut writer, args);
    drop(guard);
    IN_KLOG.store(false, Ordering::Release);
}

/// Write a raw byte slice to the kernel log buffer.
pub fn write_bytes(bytes: &[u8]) {
    if IN_KLOG.swap(true, Ordering::Acquire) {
        // Reentrant call — fall back to serial output.
        petroleum::serial::serial_log(format_args!("{}", core::str::from_utf8(bytes).unwrap_or("(binary)")));
        return;
    }
    let mut guard = KLOG_BUF.lock();
    let ring = &mut *guard;
    for &b in bytes {
        if ring.len < KLOG_CAPACITY {
            let idx = (ring.head + ring.len) % KLOG_CAPACITY;
            ring.buf[idx] = b;
            ring.len += 1;
        } else {
            // Overwrite oldest byte
            ring.buf[ring.head] = b;
            ring.head = (ring.head + 1) % KLOG_CAPACITY;
        }
    }
    drop(guard);
    IN_KLOG.store(false, Ordering::Release);
}

/// Return the entire kernel log as an owned `Vec<u8>`.
pub fn snapshot() -> alloc::vec::Vec<u8> {
    let guard = KLOG_BUF.lock();
    let ring = &*guard;
    let mut result = alloc::vec::Vec::with_capacity(ring.len);
    if ring.len > 0 {
        let end = ring.head + ring.len;
        if end <= KLOG_CAPACITY {
            result.extend_from_slice(&ring.buf[ring.head..end]);
        } else {
            result.extend_from_slice(&ring.buf[ring.head..KLOG_CAPACITY]);
            result.extend_from_slice(&ring.buf[0..(end % KLOG_CAPACITY)]);
        }
    }
    result
}

/// Write kernel log to a `Terminal`-compatible writer.
///
/// This is called from the `dmesg` shell command handler.
/// Invalid UTF-8 sequences (which can occur when the ring buffer
/// wraps during multi-byte character writes) are replaced with the
/// Unicode replacement character ``.
pub fn write_to<W: fmt::Write>(writer: &mut W) -> fmt::Result {
    let snap = snapshot();
    let mut chunk = &snap[..];
    while !chunk.is_empty() {
        match core::str::from_utf8(chunk) {
            Ok(s) => {
                writer.write_str(s)?;
                break;
            }
            Err(e) => {
                let valid_len = e.valid_up_to();
                if valid_len > 0 {
                    // SAFETY: the first `valid_len` bytes are valid UTF-8.
                    writer.write_str(unsafe {
                        core::str::from_utf8_unchecked(&chunk[..valid_len])
                    })?;
                }
                writer.write_str("")?;
                let error_len = e.error_len().unwrap_or(1);
                chunk = &chunk[valid_len + error_len..];
            }
        }
    }
    Ok(())
}

/// Return the current size (in bytes) of the kernel log buffer.
pub fn len() -> usize {
    KLOG_BUF.lock().len
}

struct KLogWriter<'a> {
    ring: &'a mut KLogRing,
    pos: usize,
}

impl<'a> fmt::Write for KLogWriter<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.ring.len < KLOG_CAPACITY {
                let idx = (self.ring.head + self.ring.len) % KLOG_CAPACITY;
                self.ring.buf[idx] = b;
                self.ring.len += 1;
            } else {
                self.ring.buf[self.ring.head] = b;
                self.ring.head = (self.ring.head + 1) % KLOG_CAPACITY;
            }
        }
        self.pos += s.len();
        Ok(())
    }
}

/// Convenience macro for writing formatted strings to the kernel log buffer.
#[macro_export]
macro_rules! klog_fmt {
    ($($arg:tt)*) => {{
        $crate::klog::write_fmt(format_args!($($arg)*));
    }};
}