//! Checked copies across the first AArch64 user-memory boundary.

use super::mmu;

/// Copy kernel-owned bytes into a mapped EL0-writable range.
///
/// This is intentionally a bounded page-table walk. It does not recover from
/// a concurrent unmap or install a page-fault continuation yet; those belong
/// to the per-process address-space layer.
pub(crate) fn copy_to_user(address: u64, source: &[u8]) -> Result<(), ()> {
    let mut copied = 0usize;
    while copied < source.len() {
        let virtual_address = address.checked_add(copied as u64).ok_or(())?;
        let page_address = virtual_address & !0xfff;
        let physical_address = mmu::user_page_physical(page_address).ok_or(())?;
        let page_offset = (virtual_address - page_address) as usize;
        let chunk_size = (4096 - page_offset).min(source.len() - copied);
        let active_space = mmu::active_user_space();
        mmu::activate_kernel_identity_space();
        unsafe {
            core::ptr::copy_nonoverlapping(
                source.as_ptr().add(copied),
                (physical_address + page_offset as u64) as *mut u8,
                chunk_size,
            );
        }
        if !mmu::activate_user_space(active_space) {
            return Err(());
        }
        copied += chunk_size;
    }
    Ok(())
}

/// Copy an EL0-readable range into kernel memory after validating every page
/// through the active TTBR0 root. This is the input side used by SPAWN and
/// WRITE; arbitrary user pointers are never treated as kernel addresses.
pub(crate) fn copy_from_user(address: u64, destination: &mut [u8]) -> Result<(), ()> {
    let mut copied = 0usize;
    while copied < destination.len() {
        let virtual_address = address.checked_add(copied as u64).ok_or(())?;
        let page_address = virtual_address & !0xfff;
        let physical_address = mmu::user_page_physical_read(page_address).ok_or(())?;
        let page_offset = (virtual_address - page_address) as usize;
        let chunk_size = (4096 - page_offset).min(destination.len() - copied);
        let active_space = mmu::active_user_space();
        mmu::activate_kernel_identity_space();
        unsafe {
            core::ptr::copy_nonoverlapping(
                (physical_address + page_offset as u64) as *const u8,
                destination.as_mut_ptr().add(copied),
                chunk_size,
            );
        }
        if !mmu::activate_user_space(active_space) {
            return Err(());
        }
        copied += chunk_size;
    }
    Ok(())
}
