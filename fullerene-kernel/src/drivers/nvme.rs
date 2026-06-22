//! NVMe driver — thin kernel wrapper around `nitrogen::storage::nvme`.
//!
//! The actual driver implementation lives in nitrogen.  This module simply
//! provides the `KernelDriverContext` and delegates to nitrogen.

use crate::driver_context_impl::KernelDriverContext;

/// Initialise all NVMe controllers found on the PCI bus.
pub fn init() {
    let ctx = KernelDriverContext;
    nitrogen::storage::nvme::init(&ctx);
}
