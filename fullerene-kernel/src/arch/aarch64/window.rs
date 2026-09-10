//! Bounded AArch64 native window capability boundary.
//!
//! The current AArch64 bring-up has no display controller backend.  Keep the
//! window ABI real nevertheless: handles have the same owner/generation
//! lifetime as files and devices, geometry is validated, and PRESENT produces
//! one checked redraw event.  A future framebuffer/compositor can replace the
//! slot contents without changing the user syscall contract.

use fullerene_abi::{WindowEvent, window_event};

use super::{fs, task, user_memory};

const ERR_ADDRESS: u64 = (-(14i64)) as u64;
const ERR_BAD_FD: u64 = (-(9i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OUT_OF_MEMORY: u64 = (-(12i64)) as u64;
const ERR_WOULD_BLOCK: u64 = (-(11i64)) as u64;
const MAX_WINDOWS: usize = 8;
const MAX_DIMENSION: u32 = 16_384;

#[derive(Clone, Copy)]
struct WindowSlot {
    id: u64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    flags: u64,
    visible: bool,
    redraw_pending: bool,
    references: u16,
    active: bool,
}

impl WindowSlot {
    const EMPTY: Self = Self {
        id: 0,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        flags: 0,
        visible: false,
        redraw_pending: false,
        references: 0,
        active: false,
    };
}

static mut WINDOWS: [WindowSlot; MAX_WINDOWS] = [WindowSlot::EMPTY; MAX_WINDOWS];
static mut NEXT_WINDOW_ID: u64 = 1;

pub(crate) fn init() {
    unsafe {
        WINDOWS = [WindowSlot::EMPTY; MAX_WINDOWS];
        NEXT_WINDOW_ID = 1;
    }
}

pub(crate) fn retain(slot: u8) -> bool {
    unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return false;
        };
        if !window.active {
            return false;
        }
        window.references = window.references.saturating_add(1);
        true
    }
}

pub(crate) fn release(slot: u8) {
    unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return;
        };
        window.references = window.references.saturating_sub(1);
        if window.references == 0 {
            *window = WindowSlot::EMPTY;
        }
    }
}

pub(crate) fn create(x: i32, y: i32, width: u32, height: u32, flags: u64) -> u64 {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return ERR_INVALID;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(slot) =
        (0..MAX_WINDOWS).find(|&index| unsafe { !(*core::ptr::addr_of!(WINDOWS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    let id = unsafe {
        let id = (*core::ptr::addr_of!(NEXT_WINDOW_ID)).max(1);
        *core::ptr::addr_of_mut!(NEXT_WINDOW_ID) = id.wrapping_add(1).max(1);
        (*core::ptr::addr_of_mut!(WINDOWS))[slot] = WindowSlot {
            id,
            x,
            y,
            width,
            height,
            flags,
            active: true,
            ..WindowSlot::EMPTY
        };
        id
    };
    match fs::install_window_handle(owner_pid, slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(WINDOWS))[slot] = WindowSlot::EMPTY;
            }
            error
        }
    }
}

pub(crate) fn destroy(handle: u64) -> u64 {
    let Some(slot) = fs::window_slot(handle) else {
        return ERR_BAD_FD;
    };
    unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return ERR_BAD_FD;
        };
        if !window.active {
            return ERR_BAD_FD;
        }
        window.visible = false;
        window.redraw_pending = false;
    }
    let _ = fs::close(handle);
    0
}

pub(crate) fn resize(handle: u64, width: u32, height: u32) -> u64 {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return ERR_INVALID;
    }
    let Some(slot) = fs::window_slot(handle) else {
        return ERR_BAD_FD;
    };
    unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return ERR_BAD_FD;
        };
        if !window.active {
            return ERR_BAD_FD;
        }
        window.width = width;
        window.height = height;
    }
    0
}

pub(crate) fn present(handle: u64) -> u64 {
    let Some(slot) = fs::window_slot(handle) else {
        return ERR_BAD_FD;
    };
    unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return ERR_BAD_FD;
        };
        if !window.active {
            return ERR_BAD_FD;
        }
        window.visible = true;
        window.redraw_pending = true;
    }
    0
}

pub(crate) fn get_event(handle: u64, buffer_address: u64, buffer_size: u64) -> u64 {
    if buffer_address == 0 || buffer_size < WindowEvent::MIN_BYTE_SIZE as u64 {
        return ERR_INVALID;
    }
    let Some(slot) = fs::window_slot(handle) else {
        return ERR_BAD_FD;
    };
    let event = unsafe {
        let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) else {
            return ERR_BAD_FD;
        };
        if !window.active {
            return ERR_BAD_FD;
        }
        if !window.redraw_pending {
            return ERR_WOULD_BLOCK;
        }
        window.redraw_pending = false;
        WindowEvent {
            kind: window_event::REDRAW,
            flags: if window.visible { 1 } else { 0 },
            window_id: window.id,
            data: [
                window.x as u64,
                window.y as u64,
                window.width as u64,
                window.height as u64,
                window.flags,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        }
    };
    if user_memory::copy_to_user(buffer_address, &event.to_ne_bytes()).is_err() {
        // Do not consume the event when the destination was invalid.  The
        // bounded implementation cannot hold a second queue, so restore the
        // single pending bit after the checked copy fails.
        unsafe {
            if let Some(window) = (*core::ptr::addr_of_mut!(WINDOWS)).get_mut(slot as usize) {
                if window.active {
                    window.redraw_pending = true;
                }
            }
        }
        ERR_ADDRESS
    } else {
        0
    }
}
