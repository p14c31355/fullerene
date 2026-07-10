# Fullerene
> **Fullerene is a Rust operating system for x86_64 UEFI featuring a graphical desktop, multitasking kernel, interactive shell, and real hardware support.**


---

![Fullerene desktop screenshot showing graphical compositor with terminal window](docs/history/fullerene_202606182121_desktop.png)
![Fullerene desktop screenshot showing early development version](docs/history/fullerene_202606100455_desktop.png)

[development_history](docs/history)

[Discord Community](https://discord.gg/FfAbRaUA26)
The community is still new, but we welcome you!

---

Fullerene is an operating system kernel written in Rust, targeting x86_64 architecture with UEFI booting. It explores modern systems programming concepts including process scheduling, virtual memory management, filesystem abstraction, syscall interfaces, GUI compositing, and event-driven shell interaction, all implemented in a safe, no_std environment.

Fullerene provides a full-featured kernel with multitasking capabilities, running in QEMU virtual machine. The system includes a bootloader, kernel scheduler, process management, memory allocation, device drivers, GUI windowing system, interactive shell, and user-space support scaffolding.

---

## Design Goals

- **REAL HARDWARE FIRST**
  - Features are validated on physical machines, not only in QEMU.

---

- **Context-oriented architecture** - Large Rust context structures reduce architectural cognitive load.
- **Minimizing inline assembly maximizes code stability** - Hardware-specific code is isolated to improve maintainability.
- **Maximizing the use of the bare metal Rust ecosystem** - Prefer reusable no_std crates over custom implementations whenever possible.

## Features

- **UEFI Bootloader (Bellows)**: A no_std UEFI application that loads the kernel ELF, initializes framebuffer graphics via Graphics Output Protocol (GOP), sets up custom configuration tables, and transitions to kernel execution after exiting boot services.

- **Full-Featured Kernel (Fullerene-Kernel)** with components including:
  - **Memory Management**: Virtual memory with page tables, heap allocation (linked-list allocator), and physical memory tracking
  - **Process Management**: Full process creation, termination, and per-process resource tracking (fd tables, handle tables with generation counters)
  - **Scheduler**: Tick-driven round-robin scheduler via `SchedulerContext` (`SCHEDULER` singleton). All scheduling state (process list, tick counter, NMI recovery) is owned by a single struct with an explicit lock hierarchy independent of the `KERNEL` context lock.
  - **Syscall Interface**: Complete system call implementation for user-kernel communication; VDSO provides zero-copy read-only access to time and PID (no async ring buffer)
  - **Filesystem**: Abstraction layer via `Genome` (standalone VFS crate) with `MemFileSystem`, FAT32, and exFAT backends
  - **GUI Windowing System**: Lattice-based compositor, desktop, window management, and font rendering with cursor blink and terminal surface
  - **Hardware Interfaces**: Keyboard input, serial output, APIC/PIC interrupt handling, VirtIO-GPU support, USB (XHCI/EHCI), NVMe/AHCI storage, Intel WiFi (iwlwifi), HDA audio
  - **Shell**: Nozzle-based interactive shell with line editing, command history, and extensible built-in commands

- **Common Library (Petroleum)**: Shared no_std utilities used by both kernel and userspace — page table management, graphics primitives, syscall ABI numbers, VDSO layout definition, serial logging, VirtIO drivers.

- **GUI Framework (Lattice)**: A no_std compositing window system providing desktop environment, window manager, scene graph, surface rendering, terminal surface with bitmap font, and cursor support.

- **Event System (Resonance)**: A no_std event-driven framework with dispatcher, event queue, event sources, and typed event handlers for decoupled component communication.

- **Time Management (Chronoline)**: A no_std timer management primitive with deadline tracking, tick-based clock advancement, and sorted timer event queue for scheduler integration.

- **Shell Runtime (Nozzle)**: A no_std interactive shell runtime providing line editor with history, command parser, extensible command interface, prompt, and terminal abstraction. Used by the kernel's shell and accessible via both serial and GUI terminal.

- **I/O Abstraction (Carrier)**: A no_std I/O abstraction layer providing the `Terminal` trait, command dispatch with streaming pipeline support, separating data transport from data processing.

- **File System Framework (Genome)**: A no_std VFS layer providing the `FileSystem` trait, `MemFileSystem`, `Vfs` dispatcher with mount-table routing, and typed `FsError` — all framework-agnostic and used by the kernel through its `VfsContext`.

- **Hardware Abstraction Layer (Nitrogen)**: Driver and hardware abstraction library providing PCI enumeration, APIC/PIC interrupt controllers, PS/2 keyboard/mouse, HDA audio, VirtIO block/net/gpu, USB XHCI, NVMe/AHCI storage, iwlwifi, and framebuffer management.

- **Application Framework (Solvent)**: File explorer, image/audio viewers, menu actions, and handler infrastructure for building user-facing applications on Lattice and Nozzle.

- **Build System (Flasks)**: Automated task runner for building bootloader and kernel, ISO creation (using isobemak crate), and QEMU virtualization with configurable VGA and display backends.

- **Networking (Bonder)**: A no_std network protocol stack with Ethernet, IPv4, UDP socket abstraction, and iwlwifi integration.

- **Userland Placeholder (Toluene)**: Scaffolding for user-space programs in Rust (currently minimal).

The system boots from UEFI firmware, initializes all hardware interfaces, and runs a kernel scheduler that manages multiple processes concurrently. User interaction occurs through a GUI terminal or serial shell interface, with full debugging support via serial logging.

## Quick Start

```bash
cargo run -q -p flasks -- --vga std
```

For detailed build instructions, QEMU options, and manual build steps, see [docs/BUILD.md](docs/BUILD.md).

## Documentation

| Document | Description |
|----------|-------------|
| [docs/BUILD.md](docs/BUILD.md) | Prerequisites, build instructions, QEMU options, manual build steps |
| [docs/WORKSPACE.md](docs/WORKSPACE.md) | Cargo workspace structure and crate descriptions |
| [docs/HARDWARE.md](docs/HARDWARE.md) | Real hardware compatibility notes |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Toolchain, testing, debugging, and development guidelines |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Architecture overview |

## TODO / Next Steps

See [docs/fullerene_todo.md](docs/fullerene_todo.md) for the full prioritized checklist aligned with the architecture and improvement roadmap.

Priority convention:
- **P0** = memory safety / process isolation
- **P1** = structural improvement (ownership, types, tests)
- **P2** = developer experience, performance

## Contributing

Bug reports, feature suggestions, and pull requests are welcome. Please see [CONTRIBUTING.md](docs/CONTRIBUTING.md) for guidelines on submitting contributions.

- Fork the repo and create a feature branch.
- Ensure tests pass and the build runs in QEMU.
- Submit a PR with detailed description.

## License

This project is licensed under either of:

- [Apache License, Version 2.0](docs/LICENSE-APACHE)
- [MIT License](docs/LICENSE-MIT)

at your option.

### Contribution Requirements

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Fullerene by you shall be dual-licensed as above, without any additional terms or conditions.