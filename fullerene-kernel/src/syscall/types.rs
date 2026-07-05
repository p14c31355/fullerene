use alloc::sync::Arc;
use alloc::vec::Vec;
use bitflags::bitflags;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(u64);

impl Handle {
    pub const SLOT_MASK: u64 = 0x0000_FFFF_0000_0000;
    pub const GEN_MASK: u64 = 0xFFFF_0000_0000_0000;
    pub const PERM_MASK: u64 = 0x0000_00FF_0000_0000;
    pub const SLOT_SHIFT: u64 = 32;
    pub const GEN_SHIFT: u64 = 48;
    pub const PERM_SHIFT: u64 = 24;

    pub fn new(slot: u16, generation: u16, perms: u8) -> Self {
        let val = (generation as u64) << 48
            | (slot as u64) << 32
            | (perms as u64) << 24;
        Handle(val)
    }

    pub fn slot(&self) -> u16 {
        ((self.0 >> 32) & 0xFFFF) as u16
    }

    pub fn generation(&self) -> u16 {
        ((self.0 >> 48) & 0xFFFF) as u16
    }

    pub fn permissions(&self) -> u8 {
        ((self.0 >> 24) & 0xFF) as u8
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
