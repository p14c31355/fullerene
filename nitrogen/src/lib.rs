#![no_std]
//! # Nitrogen — Hardware Mechanism Layer
//!
//! Nitrogen is a standalone, `no_std` crate providing **pure hardware mechanism**
//! abstractions for x86-64 systems. It has zero dependency on the kernel or
//! petroleum boot crate. All device-driver-level code (Port I/O, PCI, APIC,
//! PIC, VirtIO, etc.) lives here; higher-level policy (memory management,
//! scheduling, graphics compositing) belongs in other crates.
//!
//! ## Design principle
//!
//! - **Hardware mechanism only** — raw register access, capability scanning,
//!   interrupt-controller programming, DMA setup. No memory allocator policy,
//!   no page-table logic, no process scheduling.
//! - **Fully isolated** — depends only on `x86_64`, `spin`, and `core`/`alloc`.
//!   No dependency on `petroleum`, `fullerene-kernel`, or any other workspace crate.
//! - **Callback-friendly** — where memory allocation or MMIO mapping is required
//!   (e.g. VirtIO queue setup), the caller provides pre‑allocated physical pages
//!   and virtual addresses. Nitrogen never owns the allocator.

extern crate alloc;

pub mod apic;
pub mod apic_controller;
pub mod audio;
pub mod hda;
pub mod ioapic;
pub mod iwlwifi;
pub mod pci;
pub mod pic;
pub mod port;
pub mod ps2;
pub mod virtio;
