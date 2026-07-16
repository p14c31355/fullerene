# Workspace Structure

The project is structured as a Cargo workspace with the following crates:

- **`bellows`**: The UEFI bootloader. Responsible for loading the kernel and setting up the framebuffer configuration.

- **`fullerene-kernel`**: The core kernel. Handles hardware-policy integration, process scheduling via `SchedulerContext` (`SCHEDULER` singleton), VDSO read-only metadata pages, GUI integration, and enters the main shell loop. Its device registry leases block devices to Genome while retaining stable `/dev` identities.

- **`fullerene-abi`**: A dependency-free no_std leaf crate defining typed native syscall numbers, error codes, stable `#[repr(C)]` DTOs, ABI versioning, and capability bits. Both the kernel and Toluene SDK depend on this contract directly.

- **`flasks`**: The build and task runner. Builds the kernel and bootloader, creates a bootable ISO, and launches QEMU for emulation.

- **`lattice`**: A no_std GUI framework providing compositing window system, desktop, window manager, scene graph, and terminal surface rendering.

- **`nozzle`**: A no_std interactive shell runtime with line editor, history, command dispatch, and terminal abstraction.

- **`resonance`**: A no_std event system with dispatcher, event queue, event sources, and typed event handlers.

- **`chronoline`**: A no_std timer management primitive for deadline tracking and timer scheduling.

- **`carrier`**: A no_std I/O abstraction layer providing the `Terminal` trait, command dispatch with streaming pipeline support, and pipe mechanism for shell pipeline chaining.

- **`genome`**: A no_std file system / VFS framework providing the `FileSystem` trait, `MemFileSystem`, `Vfs` dispatcher with mount-table routing, path normalization, and typed `FsError`. The kernel crate re-exports Genome types and adds the singleton `VfsContext`.

- **`petroleum`**: A no_std library providing common EFI types, page table management, graphics primitives, serial/early boot utilities, VirtIO driver helpers, the raw syscall instruction, and VDSO layout definition. It re-exports syscall numbers from `fullerene-abi` for compatibility.

- **`bonder`**: A no_std network protocol stack implementing Ethernet frame handling, IPv4 packet processing, and UDP socket abstraction with iwlwifi integration.

- **`nitrogen`**: A hardware abstraction and device driver library providing PCI enumeration, APIC/PIC interrupt controllers, PS/2 keyboard/mouse drivers, HDA audio, VirtIO block/net/gpu drivers, USB (xHCI/EHCI), NVMe/AHCI storage, Intel wireless (iwlwifi), and framebuffer management. It owns PCI power/decode transitions and MMIO preflight for matched devices.

- **`solvent`**: An application framework providing file explorer, viewers (image/audio), menu actions, and handler infrastructure for building user-facing applications on top of Lattice and Nozzle.

- **`toluene`**: The user-space SDK and example binary. Its typed syscall wrappers consume the shared `fullerene-abi` contract directly.

- **`rle_player`** (toluene/rle_player): An RLE-encoded video player that decodes and renders animation frames, used for the Bad Apple demo.
