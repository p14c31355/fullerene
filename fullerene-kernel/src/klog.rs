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
        petroleum::serial::serial_log(format_args!(
            "{}",
            core::str::from_utf8(bytes).unwrap_or("(binary)")
        ));
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
                writer.write_str("\u{FFFD}")?;
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

// ── VFS flush / boot-log persistence ───────────────────────────────

/// Flush the kernel log ring buffer into `/bootlog.txt` on the VFS
/// (tmpfs).  Also appends a `LastStage=…` line at the end.
///
/// Returns `Ok(())` if the file was written successfully, or `Err(())`
/// if the VFS is not yet initialised or an I/O error occurred.
pub fn flush_to_vfs() -> Result<(), ()> {
    let snap = snapshot();
    let stage_line = crate::boot_stage::last_stage_line();

    // Estimate capacity: snapshot + stage_line + a few framing lines.
    let mut out = alloc::vec::Vec::with_capacity(snap.len() + stage_line.len() + 128);
    out.extend_from_slice(b"=== Fullerene boot log ===\n");
    out.extend_from_slice(&snap);
    if !snap.is_empty() && snap.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(stage_line.as_bytes());
    if stage_line.as_bytes().last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(b"=== End boot log ===\n");

    // Create /bootlogs/ if it doesn't exist
    if crate::vfs::exists("/bootlogs") {
        // Rotate previous bootlog if present
        if crate::vfs::exists("/bootlog.txt") {
            rotate_bootlog();
        }
    } else {
        // Bootlogs directory doesn't exist yet — try to create it
        // (ignore failure; we can still write /bootlog.txt)
        let _ = crate::vfs::mkdir("/bootlogs");
    }

    // Write to /bootlog.txt via the VFS low-level API.
    // We go through create+write+close manually for error resilience.
    let fd_info = crate::vfs::create("/bootlog.txt").map_err(|_| ())?;
    crate::vfs::write(fd_info.fd, &out).map_err(|_| {
        let _ = crate::vfs::close(fd_info.fd);
    })?;
    let _ = crate::vfs::close(fd_info.fd);
    Ok(())
}

/// Panic-safe variant: best-effort flush, ignores all errors.
///
/// Called from the panic handler — the VFS lock may be held by the
/// panicking thread, so we must not attempt to acquire it if it's
/// already poisoned.  We try `flush_to_vfs()` but swallow any error.
pub fn flush_to_vfs_safe() {
    if crate::contexts::vfs::vfs_try_accessible() {
        let _ = flush_to_vfs();
    }
}

/// Move `/bootlog.txt` → `/bootlogs/YYYY-MM-DD-XX.txt` with
/// a simple ascending counter for the current date.
fn rotate_bootlog() {
    // Build date string.  We don't have a real-time clock yet,
    // so we fall back to a counter-based scheme.
    // Try YYYY-MM-DD from a not-yet-existing RTC; for now use
    // a simple "boot-N.txt" style.
    let mut idx: u32 = 1;
    let mut path;
    loop {
        path = alloc::format!("/bootlogs/boot-{}.txt", idx);
        if !crate::vfs::exists(&path) {
            break;
        }
        idx += 1;
        if idx > 9999 {
            // Sanity guard — give up rotation.
            return;
        }
    }
    // Read old content, write to rotated path, then unlink original.
    if let Ok(fd) = crate::vfs::open("/bootlog.txt", 0) {
        let mut buf = [0u8; 512];
        let mut all = alloc::vec::Vec::new();
        loop {
            match crate::vfs::read(fd.fd, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
            }
        }
        let _ = crate::vfs::close(fd.fd);
        // Write rotated copy
        if let Ok(fd2) = crate::vfs::create(&path) {
            if crate::vfs::write(fd2.fd, &all).is_ok() {
                let _ = crate::vfs::close(fd2.fd);
                let _ = crate::vfs::unlink("/bootlog.txt");
            } else {
                let _ = crate::vfs::close(fd2.fd);
            }
        }
    }
}
