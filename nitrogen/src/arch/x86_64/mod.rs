//! x86_64 hardware drivers: the full PC mechanism set.
//!
//! Only compiled on x86 targets. lib.rs globs this tree into the crate root,
//! so historic paths (`nitrogen::pci::...`, `crate::ps2::...`) are unchanged.
//! Optional drivers keep their `nitrogen_no_*` feature gates here.

pub mod acpi;
pub mod apic;
pub mod apic_controller;
#[cfg(not(nitrogen_no_audio))]
pub mod audio;
pub mod driver_api;
#[cfg(not(nitrogen_no_framebuffer))]
pub mod framebuffer;
pub mod hbd;
#[cfg(not(nitrogen_no_hda))]
pub mod hda;
pub mod i2c_hid;
#[cfg(not(nitrogen_no_ioapic))]
pub mod ioapic;
#[cfg(not(nitrogen_no_iommu))]
pub mod iommu;
#[cfg(not(nitrogen_no_iwlwifi))]
pub mod iwlwifi;
pub mod mmio;
pub mod pci;
pub mod pci_error;
pub mod pci_health;
#[cfg(not(nitrogen_no_pic))]
pub mod pic;
pub mod port;
#[cfg(not(nitrogen_no_ps2))]
pub mod ps2;
#[cfg(not(nitrogen_no_storage))]
pub mod storage;
pub mod timing;
#[cfg(not(nitrogen_no_usb))]
pub mod usb;
#[cfg(not(nitrogen_no_virtio))]
pub mod virtio;
#[cfg(not(nitrogen_no_wifi))]
pub mod wifi;
