# Changelog

This document summarizes the development history and significant changes made to the Fullerene project.

## Kernel Core & Process Management
- Implemented user-mode process switching using `iretq`.
- Introduced system call interface and handler infrastructure.
- Implemented APIC (Advanced Programmable Interrupt Controller) initialization and control.
- Set up IDT (Interrupt Descriptor Table) for exception and interrupt handling.
- Transitioned process management from a global list to a structured `ProcessManager`.

## Memory Management & Paging
- Implemented kernel initialization with a new L4 page table.
- Added page table cloning functionality to support independent process address spaces.
- Utilized 1GiB huge pages for efficient kernel area mapping.
- Implemented physical frame management using `BitmapFrameAllocator`.
- Optimized `UnifiedMemoryManager` and `PageTableManager` initialization flows.
- Centralized physical memory offset management within the `petroleum` crate.

## Bootloader & System Transition
- Developed the `bellows` bootloader for UEFI-to-kernel transition.
- Implemented `KernelArgs` structure for passing boot information via SysV ABI.
- Enabled passing of framebuffer configurations from the bootloader to the kernel.
- Refactored the kernel jump mechanism using assembly-based structures for improved stability.
- Improved UEFI memory map parsing to ensure correct higher-half mapping before 4KB splits.

## Hardware Support & Drivers
- Implemented serial port drivers for kernel debugging output.
- Added support for VGA text buffers and framebuffer-based graphics output.
- Integrated hardware device management for PCI and other peripherals.

## Utilities & Infrastructure
- Created the `petroleum` crate as a common utility library for memory, paging, and hardware abstractions.
- Implemented shared panic and allocation error handlers.
- Established a structured transition frame for the landing zone during boot.

## Stability & Bug Fixes
- Resolved deadlocks in the global allocator during UEFI initialization.
- Fixed APIC initialization to use virtual addresses, preventing memory access violations.
- Corrected GDT loading and memory address calculations.
- Resolved conflicts between bootloader huge page mappings and kernel mappings.
- Fixed stack alignment to 16 bytes to ensure compatibility with SSE/AVX instructions.
- Improved serial port initialization and removed redundant writers.

## Initial Development
- Project initialization and migration of foundational code from `Rust_practice`.
- Early experimentation with Rust `no_std` environment and basic OS primitives.