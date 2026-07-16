use alloc::vec;

use crate::map_handle;
use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult, copy_user_string};
use super::process::{alloc_handle, with_handle_mut};
use super::types::*;
use crate::contexts::kernel;

pub(crate) fn syscall_enumerate_devices(
    class: u64,
    buf: *mut u8,
    buf_size: usize,
) -> SyscallResult {
    if buf.is_null() || buf_size == 0 || buf_size > (1 << 20) {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, buf_size, false)?;

    let slice = UserSlice::new(buf, buf_size, true).map_err(|_| SyscallError::InvalidArgument)?;

    let mut kernel_buf = vec![0u8; buf_size];
    let count = kernel::with_kernel(|k| {
        let devices = match class {
            1 => &k.pci.devices,
            _ => {
                return 0usize;
            }
        };

        let mut offset = 0;
        for _dev in devices.iter().take(buf_size / 16) {
            if offset + 16 > buf_size {
                break;
            }
            kernel_buf[offset..offset + 4].copy_from_slice(&(class as u32).to_ne_bytes());
            kernel_buf[offset + 4..offset + 8].copy_from_slice(&0_u32.to_ne_bytes());
            kernel_buf[offset + 8..offset + 12].copy_from_slice(&0_u32.to_ne_bytes());
            kernel_buf[offset + 12..offset + 16].copy_from_slice(&0_u32.to_ne_bytes());
            offset += 16;
        }
        devices.len()
    })
    .unwrap_or(0);

    unsafe { slice.copy_to_user(&kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;
    Ok(count as u64)
}

pub(crate) fn syscall_open_device(device_id: *const u8) -> SyscallResult {
    let id_str = unsafe { copy_user_string(device_id, 128)? };
    if id_str.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }
    alloc_handle(KernelObject::Device(DeviceState {}))
}

pub(crate) fn syscall_device_ioctl(handle: u64, _cmd: u64, _arg: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    with_handle_mut(h, |obj| {
        let _device = map_handle!(obj, Device, _d);
        Err(SyscallError::NotSupported)
    })
}
