pub mod bios_entry;
pub mod paging;
pub mod uefi_entry;
pub mod uefi_init;
pub mod uefi_main;

use core::sync::atomic::AtomicU64;

/// RSDP physical address discovered from the UEFI Configuration Table.
/// 0 = not yet discovered or not available.
pub static UEFI_RSDP_ADDRESS: AtomicU64 = AtomicU64::new(0);
