//! Boot-phase UEFI GOP discovery.

use crate::common::{EfiSystemTable, FullereneFramebufferConfig};

pub struct EarlyFramebufferInfo {
    pub config: FullereneFramebufferConfig,
    pub from_uefi: bool,
}

/// # Safety
/// `system_table` must remain valid and BootServices must still be active.
pub unsafe fn detect_uefi_gop(
    system_table: *mut EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    let table = unsafe { system_table.as_ref() }?;
    crate::graphics::uefi::init_gop_framebuffer(table)
}

/// # Safety
/// Any supplied system table must satisfy [`detect_uefi_gop`]'s contract.
pub unsafe fn init_early_framebuffer(
    system_table: Option<*mut EfiSystemTable>,
) -> Option<EarlyFramebufferInfo> {
    let config = unsafe { detect_uefi_gop(system_table?) }?;
    Some(EarlyFramebufferInfo {
        config,
        from_uefi: true,
    })
}
