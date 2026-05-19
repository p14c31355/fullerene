//! # Early Boot Phase Namespace
//!
//! This module contains code that is **exclusively for the bootloader phase**.
//! It should NOT be used by the runtime kernel after the world switch / transition.
//!
//! ## Why separate?
//!
//! The bootloader and kernel run in fundamentally different execution contexts:
//!
//! | Aspect       | Bootloader (early)             | Kernel (runtime)               |
//! |--------------|--------------------------------|--------------------------------|
//! | Mapping      | identity / flat               | higher-half paged              |
//! | Firmware     | BootServices available        | no firmware access             |
//! | Allocator    | temporary / unstable          | permanent / managed            |
//! | Interrupts   | mostly disabled               | fully enabled (IDT, APIC)      |
//! | Concurrency  | single-thread                 | SMP, scheduler, locks          |
//!
//! Mixing these concerns silently corrupts global state — the classic
//! "initialiser leaks into runtime" bug pattern.
//!
//! ## What belongs here
//!
//! - `allocator` — frame/page allocators for boot-time only
//! - `console`   — serial + VGA text output (no framebuffer dependency)
//! - `mapper`    — identity-mapped page table construction
//! - `framebuffer` — UEFI GOP detection, VGA mode setting
//!
//! ## What does NOT belong here
//!
//! - Runtime kernel allocator (`kernel_heap`, `slab`, `vmalloc`)
//! - Framebuffer renderer / compositor
//! - VirtIO abstractions (they belong to kernel driver layer)
//! - Global logger with `log` crate
//! - `PHYSICAL_MEMORY_OFFSET` / `HEAP_START` statics

pub mod allocator;
pub mod console;
pub mod framebuffer;
pub mod mapper;
pub mod transition;
