use alloc::vec;

use crate::map_handle;
use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, check_handle_permission, with_handle_mut};
use super::types::*;
use crate::contexts::kernel;
use crate::process;

pub(crate) fn syscall_create_window(x: i32, y: i32, width: u32, height: u32, _flags: u64) -> SyscallResult {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SyscallError::InvalidArgument);
    }

    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let win_id = kernel::with_kernel_mut(|k| {
        let win_id = k.window.next_window_id();
        let win = crate::contexts::window::Window::new(win_id, "New Window", x, y, width, height);
        k.window.add_window(win);
        win_id
    })
    .ok_or(SyscallError::OutOfMemory)?;

    let state = WindowState {
        window_id: win_id,
        pid,
    };
    alloc_handle(KernelObject::Window(state))
}

pub(crate) fn syscall_destroy_window(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::WRITE)?;
    with_handle_mut(h, |obj| {
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.visible = false;
            }
        });
        Ok(0)
    })
}

pub(crate) fn syscall_resize_window(handle: u64, width: u32, height: u32) -> SyscallResult {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SyscallError::InvalidArgument);
    }
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::WRITE)?;
    with_handle_mut(h, |obj| {
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.width = width;
                win.height = height;
            }
        });
        Ok(0)
    })
}

pub(crate) fn syscall_present_window(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::WRITE)?;
    with_handle_mut(h, |obj| {
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.visible = true;
                k.event.push(resonance::Event::Window(
                    resonance::event::WindowEvent::Redraw(win.id.0),
                ));
            }
        });
        Ok(0)
    })
}

pub(crate) fn syscall_get_window_event(handle: u64, buf: *mut u8, buf_size: usize) -> SyscallResult {
    if buf.is_null() || buf_size < 128 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, buf_size, false)?;

    let h = Handle::from_raw(handle);
    with_handle_mut(h, |obj| {
        let _window = map_handle!(obj, Window, _w);

        let has_event = kernel::with_kernel(|k| k.event.has_pending()).unwrap_or(false);
        if has_event {
            let slice = UserSlice::new(buf, 8, true)
                .map_err(|_| SyscallError::InvalidArgument)?;
            let kernel_buf = [0u8; 8];
            unsafe { slice.copy_to_user(&kernel_buf) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            Ok(8)
        } else {
            Err(SyscallError::Again)
        }
    })
}
