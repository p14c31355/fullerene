# Workspace Structure

The project is structured as a Cargo workspace with the following crates:

- **`bellows`**: The UEFI bootloader. Responsible for loading the kernel and setting up the framebuffer configuration.

- **`fullerene-kernel`**: The core kernel. Handles hardware-policy integration, process scheduling via `SchedulerContext` (`SCHEDULER` singleton), VDSO read-only metadata pages, GUI integration, and enters the main shell loop. Its device registry leases block devices to Genome while retaining stable `/dev` identities.

- **`fullerene-abi`**: A dependency-free `no_std` leaf crate defining the native syscall numbers, error codes, stable `#[repr(C)]` data-transfer types, ABI version, and capability bits shared by the kernel, Petroleum, and Toluene.

- **`vdso`** (`fullerene-kernel/vdso`): A standalone `no_std` VDSO-layout/helper crate used by the kernel-side VDSO implementation and its tests.

- **`flasks`**: The build and task runner. Builds the kernel and bootloader, creates a bootable ISO, and launches QEMU for emulation.

- **`lattice`**: A no_std GUI framework providing compositing window system, desktop, window manager, scene graph, and terminal surface rendering.

- **`nozzle`**: A no_std interactive shell runtime with line editor, history, command dispatch, and terminal abstraction.

- **`resonance`**: A no_std event system with dispatcher, event queue, event sources, and typed event handlers.

- **`chronoline`**: A no_std timer management primitive for deadline tracking and timer scheduling.

- **`carrier`**: A no_std I/O abstraction layer providing the `Terminal` trait, command dispatch with streaming pipeline support, and pipe mechanism for shell pipeline chaining.

- **`genome`**: A no_std file system / VFS framework providing the `FileSystem` trait, `MemFileSystem`, `Vfs` dispatcher with mount-table routing, path normalization, and typed `FsError`. The kernel crate re-exports Genome types and adds the singleton `VfsContext`.

- **`petroleum`**: A no_std library providing common EFI types, page table management, graphics primitives, serial/early boot utilities, VirtIO driver helpers, the raw syscall instruction, and VDSO layout definition. It re-exports syscall numbers from `fullerene-abi` for compatibility.

- **`sealant`**: A no_std capability library for checked RAM, MMIO, user-memory, DMA, framebuffer, and physical-address access. It is used at low-level memory boundaries by drivers and security-sensitive helpers.

- **`ligand`** (`DriverKit`): A user-space C ABI IPC client for driver processes. It wraps ABI discovery, device enumeration/open, bounded block I/O, channel framing, kernel-owned shared-buffer mapping, and capability-handle revocation without exposing Rust-specific types across the boundary. Shared buffers are ordinary RAM capabilities; DMA ownership is a separate future layer.

- **`bonder`**: A no_std network protocol stack implementing Ethernet frame handling, IPv4 packet processing, and UDP socket abstraction with iwlwifi integration.

- **`nitrogen`**: A hardware abstraction and device driver library providing PCI enumeration, APIC/PIC interrupt controllers, PS/2 keyboard/mouse drivers, HDA audio, VirtIO block/net/gpu drivers, USB (xHCI/EHCI), NVMe/AHCI storage, Intel wireless (iwlwifi), and framebuffer management. It owns PCI power/decode transitions and MMIO preflight for matched devices.

- **`solvent`**: The runtime/orchestration layer coordinating runtime state, input translation, event dispatch, services, frame pacing, window lifecycle, file explorer, and viewers on top of Lattice and Nozzle. Its crate root is a stable API facade over context-specific modules.

- **`toluene`**: The user-space SDK and example binary. Its typed syscall wrappers consume the shared `fullerene-abi` contract directly.

- **`toluene/apps`**: Standalone WASI application sources embedded by the kernel build (`hello_wasi.rs` and `startup_sound.rs`). `toluene/viewer` and `toluene/emulsion` are separate nested application workspaces built by `fullerene-kernel/build.rs`, not members of the root workspace.

The root workspace currently has 21 members. The `default-members` list omits
the bootloader, ABI/VDSO helper crates, and `solvent/wasi` only to keep the
usual host development commands focused; `cargo check --workspace` includes
all members. `toluene/cargo` is a vendored third-party Cargo source tree and is
not a root-workspace member.

The current tracked-source census (2026-08-23 UTC / 2026-08-24 JST), excluding
`target/` and the vendored `toluene/cargo`, `toluene/busybox`, `toluene/netsurf`,
`toluene/freedoom`, and `toluene/vscodium` trees, is 420 Rust files and
140,646 Rust LOC. These are repository counts, not generated-artifact sizes;
the nested `toluene/viewer` and `toluene/emulsion` applications remain
separate workspaces. Refactor LOC claims should name the excluded subtrees
and use this reproducible command (the two output lines are the file count and
total Rust LOC):

```sh
rust_files="$(git ls-files -- '*.rs' \
  ':(exclude)toluene/cargo/**' \
  ':(exclude)toluene/busybox/**' \
  ':(exclude)toluene/netsurf/**' \
  ':(exclude)toluene/freedoom/**' \
  ':(exclude)toluene/vscodium/**')"
printf '%s\n' "$rust_files" | sed '/^$/d' | wc -l
printf '%s\n' "$rust_files" | sed '/^$/d' | xargs wc -l | tail -n 1
```

Generated and vendored inputs are not
Fullerene architecture-refactor targets. The Linux personality has a single
source tree at `solvent/linux`; the kernel integrates it through the explicitly
named `solvent_linux` module path, with no kernel-side symlink or compatibility
alias.
The large `toluene/busybox` and `toluene/vscodium` trees are vendored inputs,
not Fullerene-owned architecture layers; changes to their integration belong
in `fullerene-kernel/build.rs`, the ELF loader, and the runtime terminal bridge.
