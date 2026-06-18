//! AHCI driver — thin kernel wrapper around `nitrogen::storage::ahci`.
//!
//! The actual driver implementation lives in nitrogen.  This module simply
//! provides the `KernelDriverContext` and delegates to nitrogen.

use crate::driver_context_impl::KernelDriverContext;

/// Initialise all AHCI controllers found on the PCI bus.
pub fn init() {
    let ctx = KernelDriverContext;
    nitrogen::storage::ahci::init(&ctx);
}