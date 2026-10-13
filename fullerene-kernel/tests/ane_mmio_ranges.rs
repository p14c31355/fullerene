//! Host-side guard for the hi6250 Device aperture.
//!
//! `arch/aarch64/mmu.rs` only compiles for the aarch64 target, so the table it
//! consumes is verified here by including the platform module directly (the
//! same trick `aarch64_fdt.rs` uses for the FDT code) and re-stating the
//! 2 MiB block-overlap rule the MMU applies. The ANE table replaced the shared
//! Lito/QEMU list on 2026-10-04 after a boot died between step marks 9 and a
//! with the UART mapped as Normal memory; every address below is pinned to the
//! device's own tree (`firmware/ane_dtb/fdt_properties.txt`).

#[path = "../src/platform/common/mod.rs"]
mod common;

#[path = "../src/platform/ane/mod.rs"]
mod ane;

// v310/v319: the reenumerate quiet-wait calls crate::ane_tick_count() and
// super::ane_usb_dev (both exist only in the kernel roots). Stubs keep the
// host-side include compiling.
pub(crate) fn ane_tick_count() -> u32 {
    0
}

pub(crate) fn ane_conversations() -> u32 {
    0
}

#[path = "../src/blackbox.rs"]
mod blackbox;

const BLOCK_SIZE: u64 = 0x20_0000;

/// Mirrors `mmu::in_mmio_ranges`: true when the 2 MiB identity block
/// containing `physical` overlaps any Device range.
fn in_mmio_ranges(ranges: &[(u64, u64)], physical: u64) -> bool {
    let block_end = physical.saturating_add(BLOCK_SIZE - 1);
    ranges
        .iter()
        .any(|(start, end)| physical <= *end && block_end >= *start)
}

#[test]
fn ane_aperture_is_device_and_ram_stays_normal() {
    let ranges = ane::MMIO_RANGES;
    for address in [
        0xfdf0_2000u64, // PL011 console - the first post-MMU write
        0xe862_0000,    // DSS VG0
        0xe865_a000,    // DSS WCH0
        0xff10_0000,    // susb controller window
        0xfff3_5000,    // CRG
        0xffff_ffff,    // top of the 32-bit aperture
    ] {
        assert!(
            in_mmio_ranges(ranges, address),
            "{address:#x} must be Device on hi6250"
        );
    }
    for address in [
        0x3100_0000u64, // graphic carveout - the visible scan surface
        0x01ae_0000,    // the kernel's own embedded payload island
        0x0800_0000,    // plain RAM below 0xE000_0000 (legacy list entry)
        0x8000_0000,    // second RAM bank
        0x3490_0000,    // carveout region inside RAM
        0x1234_5678,    // ordinary low RAM
    ] {
        assert!(
            !in_mmio_ranges(ranges, address),
            "{address:#x} must stay Normal on hi6250"
        );
    }
}

#[test]
fn the_aperture_covers_every_peripheral_node_in_the_device_tree() {
    // Every register-bearing peripheral the early boot can touch, read off
    // the DT's node names (`@e8...`/`@fdf...`/`@fff...`); a new device node
    // must land in the aperture or be added to it deliberately. The
    // sub-0xE0000000 nodes are reserved-memory carveouts and one DSP TCM
    // window - they stay Normal by design (see the first test's list).
    let ranges = ane::MMIO_RANGES;
    for address in [
        0xe810_0000, // GIC-adjacent control block
        0xe82b_0000, // mailbox / pmctrl window
        0xe840_0000, // dss ctrl
        0xe8a0_9000, // usb2 phy
        0xfdf0_0000, // PL011 uart@fdf00000 (disabled but mapped)
        0xfdf0_3000, // PL011 uart@fdf03000
        0xfe00_0000, // hisi-usb2phy
        0xff10_0000, // dwc2/susb core window (usb_otg_ahbif is one block above)
        0xfff0_a000, // sysctrl
        0xfff3_5000, // crgctrl
    ] {
        assert!(
            in_mmio_ranges(ranges, address),
            "peripheral {address:#x} must be Device"
        );
    }
}
