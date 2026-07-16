//! ABI discovery syscall implementation.

use super::interface::{SyscallError, SyscallResult, copy_versioned_dto_to_user};

const KERNEL_CAPABILITIES: fullerene_abi::CapabilitySet = fullerene_abi::CapabilitySet::EMPTY
    .with(fullerene_abi::Capability::NativeSyscall)
    .with(fullerene_abi::Capability::LinuxCompat)
    .with(fullerene_abi::Capability::MultiWindow)
    .with(fullerene_abi::Capability::EventSystem)
    .with(fullerene_abi::Capability::Threading)
    .with(fullerene_abi::Capability::IpcChannels)
    .with(fullerene_abi::Capability::IpcPipes)
    .with(fullerene_abi::Capability::TimerSystem)
    .with(fullerene_abi::Capability::DeviceEnumeration);

pub(crate) fn syscall_abi_query(info_buf: *mut u8, buf_size: usize) -> SyscallResult {
    if info_buf.is_null() {
        return if buf_size == 0 {
            Ok(fullerene_abi::AbiVersion::CURRENT.pack())
        } else {
            Err(SyscallError::InvalidArgument)
        };
    }

    let bytes = fullerene_abi::AbiInfo::new(KERNEL_CAPABILITIES).to_ne_bytes();
    copy_versioned_dto_to_user(
        info_buf,
        buf_size,
        fullerene_abi::AbiInfo::MIN_BYTE_SIZE,
        &bytes,
    )
}
