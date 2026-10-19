#![no_std]
#![cfg_attr(
    all(target_arch = "xtensa", feature = "esp32"),
    feature(asm_experimental_arch)
)]

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
extern crate alloc;

pub mod adb_wire;
pub mod fastboot_session;
#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod arch;
pub mod blackbox;
pub mod cdc_acm;
