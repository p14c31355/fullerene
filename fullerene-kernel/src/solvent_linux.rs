//! Solvent's Linux personality integration point.
//!
//! `build.rs` materializes this module from the canonical sources in
//! `solvent/linux`. Keeping this include inside the kernel source tree lets
//! Rust Analyzer resolve the module without a symlink or an out-of-crate path.

include!(concat!(env!("OUT_DIR"), "/solvent-linux.rs"));
