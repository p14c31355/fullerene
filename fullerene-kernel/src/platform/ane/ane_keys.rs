//! The key bank, read-only.
//!
//! The vendor DTS puts the side keys on `hisilicon,gpio-key` pins in the
//! gpio25 bank (`gpio-keyup`/`gpio-keydown` at pins 1 and 2 in the generic
//! UDP config), and the power key comes from the PMIC rather than from a
//! GPIO. Which pins this unit actually uses is a question for the device, so
//! this module does not guess: it reads the whole 8-bit data register of the
//! three banks around it and lets an operator press a key and diff.
//!
//! Reading is safe at any time - PL061 `DATA` at base+0x3FC is a plain
//! register, and the pclk for these banks is already on (gpio18 is used for
//! the touch reset, so the GPIO clock domain is alive).

use super::common::mmio::rd32;

/// PL061 data register (masked by the pin bits).
const PL061_DATA: usize = 0x3fc;
/// PL061 direction register; unused here but named so the two are not
/// confused when this grows a poll.
const _PL061_DIR: usize = 0x400;

/// gpio24, gpio25, gpio26 - the banks the key nodes sit in.
const BANK24: usize = 0xfff0_d000;
const BANK25: usize = 0xfff0_e000;
const BANK26: usize = 0xfff0_f000;

/// (gpio24, gpio25, gpio26) data registers, low 8 bits each.
pub fn scan() -> (u8, u8, u8) {
    unsafe {
        (
            rd32(BANK24, PL061_DATA) as u8,
            rd32(BANK25, PL061_DATA) as u8,
            rd32(BANK26, PL061_DATA) as u8,
        )
    }
}
