use alloc::sync::Arc;
use alloc::vec::Vec;
use bitflags::bitflags;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::process;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct HandlePerms: u8 {
        const READ      = 0b0001;
        const WRITE     = 0b0010;
        const SIGNAL    = 0b0100;
        const DUPLICATE = 0b1000;
        const TRANSFER  = 0b0001_0000;
    }
}

/// Per-boot secret used for cryptographically signing handles.
/// Initialised once during kernel init with entropy from the CPU or system timer.
/// Never exposed to user space.  A leaked secret lets attackers forge handles.
static HANDLE_SECRET: AtomicU64 = AtomicU64::new(0);

/// Initialise the handle signing secret.  Called once during boot.
/// `seed` should be derived from hardware entropy (RDRAND, TSC, etc.).
pub fn init_handle_secret(seed: u64) {
    // Mix the seed through a simple splitmix64-style avalanche to
    // avoid degenerate states (e.g. all-zero seed → all-zero MAC).
    let mut h = seed.wrapping_add(0x9E3779B97F4A7C15);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D049BB133111EB);
    h ^= h >> 31;
    HANDLE_SECRET.store(h, Ordering::Relaxed);
}

/// Keyed hash for handle integrity.
/// Maps (slot, generation, perms) → 40-bit MAC using the per-boot secret.
/// The MAC is positioned at bits [63:24] of the 64-bit handle value.
fn handle_mac(slot: u8, generation: u8, perms: u8) -> u64 {
    let secret = HANDLE_SECRET.load(Ordering::Relaxed);
    if secret == 0 {
        return 0; // uninitialised — allow legacy handles during early boot
    }
    // Pack the non-secret data into a single 64-bit value.
    let data = (slot as u64) << 16 | (generation as u64) << 8 | perms as u64;
    // SplitMix64 mixing: two rounds with the secret as additive constant.
    let mut h = data.wrapping_add(secret);
    h ^= h >> 31;
    h = h.wrapping_mul(0x9E3779B97F4A7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 32;
    // Return top 40 bits as MAC (bits 63..24).
    h & 0xFFFFFFFFFF000000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(u64);

impl Handle {
    const SLOT_SHIFT: u64 = 16;
    const PERM_SHIFT: u64 = 8;
    const MAC_MASK: u64 = 0xFFFF_FFFF_FF00_0000;

    /// Create a new signed handle.  The MAC is derived from the
    /// slot index, generation counter, permissions, and the per-boot secret.
    /// Only the kernel can create valid handles.
    pub fn new(slot: u8, generation: u8, perms: u8) -> Self {
        let mac = handle_mac(slot, generation, perms);
        let val = mac
            | (slot as u64) << Self::SLOT_SHIFT
            | (perms as u64) << Self::PERM_SHIFT
            | generation as u64;
        Handle(val)
    }

    pub fn slot(&self) -> u8 {
        ((self.0 >> Self::SLOT_SHIFT) & 0xFF) as u8
    }

    pub fn generation(&self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    pub fn permissions(&self) -> u8 {
        ((self.0 >> Self::PERM_SHIFT) & 0xFF) as u8
    }

    /// Validate the handle's MAC against the per-boot secret.
    /// Returns true if the handle was created by the kernel and has not
    /// been corrupted or forged.
    pub fn is_valid(&self) -> bool {
        let mac = self.0 & Self::MAC_MASK;
        let expected = handle_mac(self.slot(), self.generation(), self.permissions());
        mac == expected
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn from_raw(val: u64) -> Self {
        Handle(val)
    }
}

impl From<Handle> for u64 {
    fn from(h: Handle) -> u64 {
        h.0
    }
}

// ── Kernel object types ──────────────────────────────────────

pub enum KernelObject {
    Event(EventState),
    Thread(ThreadState),
    Window(WindowState),
    Device(DeviceState),
    Channel(ChannelState),
    Pipe(PipeState),
    Timer(TimerState),
}

pub struct EventInner {
    pub signaled: bool,
    pub manual_reset: bool,
    pub waiters: Vec<process::ProcessId>,
}

pub struct EventState {
    pub inner: Arc<Mutex<EventInner>>,
}

pub struct ThreadInner {
    pub pid: process::ProcessId,
    pub detached: bool,
    pub exit_code: Option<i32>,
    pub waiters: Vec<process::ProcessId>,
}

pub struct ThreadState {
    pub inner: Arc<Mutex<ThreadInner>>,
}

pub struct WindowState {
    pub window_id: crate::contexts::window::WindowId,
    pub pid: process::ProcessId,
}

pub struct DeviceState {}

pub struct ChannelInner {
    pub messages: Vec<Vec<u8>>,
    pub waiters: Vec<process::ProcessId>,
    pub max_messages: usize,
}

pub struct ChannelState {
    pub inner: Arc<Mutex<ChannelInner>>,
}

pub struct PipeState {
    pub buffer: Arc<Mutex<Vec<u8>>>,
    pub is_read_end: bool,
}

pub struct TimerState {
    pub deadline_ns: u64,
    pub event_handle: Handle,
    pub fired: bool,
}

#[macro_export]
macro_rules! map_handle {
    ($obj:expr, $variant:ident, $name:ident) => {
        match $obj {
            $crate::syscall::types::KernelObject::$variant($name) => $name,
            _ => return Err($crate::syscall::interface::SyscallError::BadHandle),
        }
    };
}
