//! Host reproduction harness for the ANE-LX2J reserved-memory walk.
//!
//! The bring-up stopped between the step marks `h` (from_boot_info entered)
//! and `i` (the reserved-memory walk and its push loop returned) on
//! 2026-10-04. Rather than burn another device power cycle on a guess, this
//! test runs the real walker over the device's real flattened tree (dumped
//! from `/sys/firmware/fdt` on the ANE-LX2J, 475,616 bytes) on the host.
//!
//! If the walk hangs, this test hangs too - run it with a timeout:
//!     timeout 60 cargo test -p fullerene-kernel --test ane_fdt_walk

#![allow(dead_code)]

extern crate alloc;

#[path = "../src/arch/aarch64/fdt.rs"]
mod fdt;

/// `fdt.rs` emits the ANE bring-up step marks through
/// `entry::ane_step_byte`; the host harness runs the walker without the
/// boot entry, so the marks are no-ops here. Signature must stay in step
/// with `src/arch/aarch64/entry.rs`.
mod entry {
    pub(crate) unsafe fn ane_step_byte(_byte: u8) {}
}

const ANE_DTB: &str = "/home/placeless/dev/p20-root/firmware/ane_dtb/fdt.dtb";

#[test]
fn ane_device_tree_inspects() {
    let data = std::fs::read(ANE_DTB).expect("ANE dtb present");
    let header = fdt::inspect(data.as_ptr() as u64).expect("ANE dtb header parses");
    assert!(header.total_size as usize <= data.len());
    assert!(header.structure_size > 0 && header.strings_size > 0);
}

#[test]
fn ane_device_tree_memory_regions_match_the_device_dump() {
    let data = std::fs::read(ANE_DTB).expect("ANE dtb present");
    let mut regions = [fdt::Region { base: 0, size: 0 }; 8];
    let count = fdt::find_memory_regions(data.as_ptr() as u64, &mut regions);
    assert!(
        count >= 3,
        "ANE /memory@0 has three entries, parsed {count}"
    );
    // The three entries the device itself reports.
    assert_eq!(regions[0].base, 0x0040_0000);
    assert_eq!(regions[0].size, 0x7fc0_0000);
    assert_eq!(regions[1].base, 0x8000_0000);
    assert_eq!(regions[1].size, 0x6000_0000);
    assert_eq!(regions[2].base, 0x1_0000_0000);
    assert_eq!(regions[2].size, 0x2000_0000);
}

#[test]
fn ane_device_tree_reserved_walk_terminates_and_finds_regions() {
    let data = std::fs::read(ANE_DTB).expect("ANE dtb present");
    let mut regions = [fdt::Region { base: 0, size: 0 }; fdt::MAX_RESERVED_MEMORY_REGIONS];
    let count = fdt::find_reserved_memory_regions(data.as_ptr() as u64, &mut regions);
    assert!(
        count >= 5,
        "the ANE tree carries many reserved-memory children; parsed {count}"
    );
    assert!(count <= fdt::MAX_RESERVED_MEMORY_REGIONS);
    // Every returned region must be well-formed.
    for region in regions.iter().take(count) {
        assert!(
            region.size > 0,
            "zero-size reserved region {:#x}",
            region.base
        );
    }
}

#[test]
fn ane_active_i2c_controller_is_i2c2() {
    let data = std::fs::read(ANE_DTB).expect("ANE dtb present");
    let region = fdt::find_compatible(data.as_ptr() as u64, b"hisilicon,designware-i2c")
        .expect("ANE active DT must expose a DesignWare I2C controller");
    assert_eq!(region.base, 0xFFD7_3000);
    assert_eq!(region.size, 0x1000);
}
