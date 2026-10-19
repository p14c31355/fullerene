//! The fastboot session state, separated from the dwc2 driver.
//!
//! `ane_usb_dev` owns bytes: FIFOs, endpoints, the transfer state machine.
//! What those bytes *mean* for a fastboot session - the command line
//! arriving on EP1 OUT and the `download:` payload staged for the write
//! path - is state, not transport, and lives here so that the driver's
//! endpoint code reads as "a byte arrived" instead of pointer arithmetic
//! over three `static mut`s.
//!
//! The module is deliberately dumb: it holds the buffers, decides whether
//! an arriving byte belongs to the payload or to the command line, and
//! hands out slices. Dispatch and replies stay in `ane_usb_dev` for now;
//! moving those is the next step and needs the device to verify.

/// One fastboot command line. The real client sends no terminator, so the
/// transfer boundary (OUTDONE) is what ends a command.
pub(crate) const CMD_MAX: usize = 128;

/// Staging for a `download:` payload. The kernel image is ~1.2 MB gzipped
/// and the partition ceiling is 24 MB, so 12 MiB of .bss covers it.
pub(crate) const DOWNLOAD_MAX: usize = 12 << 20;

static mut CMD: [u8; CMD_MAX] = [0; CMD_MAX];
static mut CMD_LEN: usize = 0;

static mut BUF: [u8; DOWNLOAD_MAX] = [0; DOWNLOAD_MAX];
/// 0 idle, 1 receiving, 2 complete-and-unreplied.
static mut DL_STATE: u8 = 0;
static mut DL_REMAIN: usize = 0;
static mut DL_LEN: usize = 0;
/// Last progress timestamp (uptime_us) for the download timeout.
static mut DL_T0: u64 = 0;

/// A USB reset or a reconnect starts the session over.
pub fn reset() {
    unsafe {
        core::ptr::addr_of_mut!(CMD_LEN).write(0);
        core::ptr::addr_of_mut!(DL_STATE).write(0);
        core::ptr::addr_of_mut!(DL_REMAIN).write(0);
        core::ptr::addr_of_mut!(DL_LEN).write(0);
    }
}

pub fn cmd_len() -> usize {
    unsafe { core::ptr::addr_of!(CMD_LEN).read() }
}

pub fn clear_cmd() {
    unsafe { core::ptr::addr_of_mut!(CMD_LEN).write(0) }
}

/// The command line as received. Valid until the next `take_byte`.
pub fn cmd() -> &'static [u8] {
    unsafe {
        let n = core::ptr::addr_of!(CMD_LEN).read();
        core::slice::from_raw_parts(core::ptr::addr_of!(CMD) as *const u8, n)
    }
}

pub fn dl_state() -> u8 {
    unsafe { core::ptr::addr_of!(DL_STATE).read() }
}

pub fn dl_len() -> usize {
    unsafe { core::ptr::addr_of!(DL_LEN).read() }
}

pub fn dl_remain() -> usize {
    unsafe { core::ptr::addr_of!(DL_REMAIN).read() }
}

/// The staged payload. Valid while the state says complete.
pub fn dl_data() -> &'static [u8] {
    unsafe {
        let n = core::ptr::addr_of!(DL_LEN).read();
        core::slice::from_raw_parts(core::ptr::addr_of!(BUF) as *const u8, n)
    }
}

/// Announce an incoming `download:` payload of `len` bytes.
pub fn dl_begin(len: usize) {
    unsafe {
        core::ptr::addr_of_mut!(DL_LEN).write(0);
        core::ptr::addr_of_mut!(DL_REMAIN).write(len.min(DOWNLOAD_MAX));
        core::ptr::addr_of_mut!(DL_T0).write(crate::timer::uptime_us());
        core::ptr::addr_of_mut!(DL_STATE).write(1);
    }
}

/// Drop the payload: the buffer is not cleared, only the bookkeeping.
pub fn dl_reset() {
    unsafe {
        core::ptr::addr_of_mut!(DL_STATE).write(0);
        core::ptr::addr_of_mut!(DL_REMAIN).write(0);
        core::ptr::addr_of_mut!(DL_LEN).write(0);
    }
}

/// The `download:` timeout reads this.
pub fn dl_t0() -> u64 {
    unsafe { core::ptr::addr_of!(DL_T0).read() }
}

/// Keep a long download from looking abandoned.
pub fn dl_touch() {
    unsafe { core::ptr::addr_of_mut!(DL_T0).write(crate::timer::uptime_us()) }
}

/// One byte arrived on EP1 OUT. Returns true when it belonged to a
/// download payload, false when it is part of the command line.
pub fn take_byte(b: u8) -> bool {
    unsafe {
        let remain = core::ptr::addr_of!(DL_REMAIN).read();
        if remain > 0 {
            // A payload owns EP1 OUT until its last byte; the trailing
            // padding of the final word is dropped by the caller's count.
            let len = core::ptr::addr_of!(DL_LEN).read();
            if len < DOWNLOAD_MAX {
                (core::ptr::addr_of_mut!(BUF) as *mut u8).add(len).write(b);
                core::ptr::addr_of_mut!(DL_LEN).write(len + 1);
            }
            core::ptr::addr_of_mut!(DL_T0).write(crate::timer::uptime_us());
            let next = remain - 1;
            core::ptr::addr_of_mut!(DL_REMAIN).write(next);
            if next == 0 {
                core::ptr::addr_of_mut!(DL_STATE).write(2);
            }
            true
        } else {
            let len = core::ptr::addr_of!(CMD_LEN).read();
            if len < CMD_MAX {
                (core::ptr::addr_of_mut!(CMD) as *mut u8).add(len).write(b);
                core::ptr::addr_of_mut!(CMD_LEN).write(len + 1);
            }
            false
        }
    }
}

/// The download a client is waiting on, when the state says complete.
pub fn dl_complete() -> Option<&'static [u8]> {
    if dl_state() == 2 {
        Some(dl_data())
    } else {
        None
    }
}
