//! aarch64 (ANE) subset: the names the desktop path asks of nitrogen.
//!
//! There is no PC bus hardware on the phone yet, so these modules provide the
//! interfaces with no hardware behind them - enough for the shared desktop
//! (solvent) code to compile and for the ANE input/bus bring-up to fill in
//! later without touching the x86 tree.

#[cfg(not(nitrogen_no_iwlwifi))]
pub mod iwlwifi;
#[cfg(not(nitrogen_no_ps2))]
pub mod ps2;

pub mod i2c_hid;
