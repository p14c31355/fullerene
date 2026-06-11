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

/// Write a formatted message to the kernel log buffer.
///
/// This is the primary entry point.  Use it like `write!`:
///
/// ```ignore
/// klog_fmt!(format_args!("Sound: Hello {}\n", name));
/// ```
pub fn write_fmt(args: fmt::Arguments<'_>) {
    let mut guard = KLOG_BUF.lock();
    let ring = &mut *guard;
    let mut writer = KLogWriter { ring, pos: 0 };
    let _ = fmt::Write::write_fmt(&mut writer, args);
}

/// Write a raw byte slice to the kernel log buffer.
pub fn write_bytes(bytes: &[u8]) {
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
}

/// Return the entire kernel log as an owned `Vec<u8>`.
pub fn snapshot() -> alloc::vec::Vec<u8> {
    let guard = KLOG_BUF.lock();
    let ring = &*guard;
    let mut result = alloc::vec::Vec::with_capacity(ring.len);
    for i in 0..ring.len {
        let idx = (ring.head + i) % KLOG_CAPACITY;
        result.push(ring.buf[idx]);
    }
    result
}

/// Write kernel log to a `Terminal`-compatible writer.
///
/// This is called from the `dmesg` shell command handler.
pub fn write_to<W: fmt::Write>(writer: &mut W) -> fmt::Result {
    let guard = KLOG_BUF.lock();
    let ring = &*guard;
    for i in 0..ring.len {
        let idx = (ring.head + i) % KLOG_CAPACITY;
        let b = ring.buf[idx];
        // Safety: we only store valid UTF-8 sequences
        // (all input goes through fmt::Write or is valid ASCII).
        // Write one byte at a time through fmt::Write
        let mut tmp = [0u8; 1];
        tmp[0] = b;
        let s = core::str::from_utf8(&tmp[..]).map_err(|_| fmt::Error)?;
        writer.write_str(s)?;
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