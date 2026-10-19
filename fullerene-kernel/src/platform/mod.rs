//! Platform code, grouped on disk by SoC/board.
//!
//! `common/` holds what the platforms here share: the volatile MMIO
//! accessors, the Hisilicon PMIC byte protocol and the GICv3 helper.
//! `ane/` is the Huawei/Kirin 659 board (P20 Lite) and `bramble/` the
//! Pixel 4a (5G).
//!
//! Disk layout vs module tree: the files live inside `ane/` and
//! `bramble/`, but their modules stay siblings of those directories -
//! `ane_emmc` is `platform::ane_emmc`, not `platform::ane::emmc`. The
//! `#[path]` declarations below do that on purpose. These files
//! reference one another through `super::` and are compiled from more
//! than one crate root (`arch/aarch64/main.rs` inside the kernel bin,
//! `arch/aarch64/usb_probe.rs`), so an `#[path]` move relocates bytes
//! only and leaves every `super::` target meaning exactly what it did.
//! Re-parenting them under `ane::` or `bramble::` would rewrite every
//! cross-reference in files that must build for every root; the same
//! rule keeps `arch/aarch64/usb/` flat (see the note in
//! `arch/aarch64/main.rs`).

pub mod common;

pub mod ane;

/// `ane_dwmmc` etc. live beside `ane/mod.rs` on disk but are declared
/// here, at the same module level, for the reason in the module header.
#[path = "ane/ane_battery.rs"]
pub mod ane_battery;
#[path = "ane/ane_dwmmc.rs"]
pub mod ane_dwmmc;
#[path = "ane/ane_emmc.rs"]
pub mod ane_emmc;
#[path = "ane/ane_emmc_diag.rs"]
pub mod ane_emmc_diag;
#[path = "ane/ane_i2c.rs"]
pub mod ane_i2c;
#[path = "ane/ane_fb.rs"]
pub mod ane_fb;
#[path = "ane/ane_touch.rs"]
pub mod ane_touch;
#[path = "ane/ane_usb_dev.rs"]
pub mod ane_usb_dev;
#[path = "ane/fastboot_session.rs"]
pub mod fastboot_session;

pub mod bramble;
pub mod qemu_virt;
