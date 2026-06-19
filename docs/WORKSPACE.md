# Workspace Structure

The project is structured as a Cargo workspace with the following crates:

- **`bellows`**: The UEFI bootloader. Responsible for loading the kernel and setting up the framebuffer configuration.

- **`fullerene-kernel`**: The core kernel. Handles low-level hardware initialization, process scheduling, and enters the main shell loop.

- **`flasks`**: The build and task runner. Builds the kernel and bootloader, creates a bootable ISO, and launches QEMU for emulation.

- **`lattice`**: A no_std GUI framework providing compositing window system, desktop, window manager, scene graph, and terminal surface rendering.

- **`nozzle`**: A no_std interactive shell runtime with line editor, history, command dispatch, and terminal abstraction.

- **`resonance`**: A no_std event system with dispatcher, event queue, event sources, and typed event handlers.

- **`chronoline`**: A no_std timer management primitive for deadline tracking and timer scheduling.

- **`petroleum`**: A library providing common EFI types, utilities, serial output macros, graphics primitives, page table management, and VirtIO drivers for no_std environments.

- **`bonder`**: A no_std network protocol stack implementing Ethernet frame handling, IPv4 packet processing, and UDP socket abstraction with a logger subsystem.

- **`nitrogen`**: A hardware abstraction and device driver library providing PCI enumeration, APIC/PIC interrupt controllers, PS/2 keyboard/mouse drivers, HDA audio, VirtIO block/net/gpu drivers, USB (XHCI), NVMe/AHCI storage, Intel wireless (iwlwifi), and framebuffer management. The kernel's primary driver layer.

- **`solvent`**: An application framework providing file explorer, viewers (image/audio), menu actions, and handler infrastructure for building user-facing applications on top of Lattice and Nozzle.

- **`toluene`**: Placeholder for userland components (e.g., user-space binaries). Currently minimal.

- **`rle_player`** (toluene/rle_player): An RLE-encoded video player that decodes and renders animation frames, used for the Bad Apple demo.
