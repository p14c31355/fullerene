# Fullerene

---

![](docs/assets/fullerene-image1.jpg)

---

Fullerene is an experimental operating system kernel written in Rust, targeting x86_64 architecture with UEFI booting. It aims to explore low-level systems programming concepts in a safe and modern language. Currently, it provides a basic UEFI-compatible bootloader, kernel initialization, and support for essential hardware interfaces like VGA text mode, serial output, and a framebuffer.

This project is in its early stages and serves as a learning platform for OS development in Rust.

## Features

- **UEFI Bootloader (Bellows)**: A no_std UEFI application that loads the kernel ELF, initializes a Graphics Output Protocol (GOP) framebuffer, installs a custom configuration table for the kernel, and jumps to the kernel entry point after exiting boot services.
- **Kernel Initialization (Fullerene-Kernel)**:
  - Global Descriptor Table (GDT) setup.
  - Interrupt Descriptor Table (IDT) and Programmable Interrupt Controller (PIC) initialization.
  - Basic VGA text mode output for early logging.
  - Serial port output (COM1) for debugging.
  - Detection of a custom framebuffer configuration table passed from the bootloader.
- **Build and Emulation Support**: Automated build process using Cargo, creating a bootable ISO, and running in QEMU with OVMF UEFI firmware.
- **Interrupts**: Basic handling of hardware interrupts (though currently enters a halt loop after init).

The kernel currently enters an infinite halt loop after initialization. Future work includes implementing a pager, real memory allocator, process scheduling, and userland support.

## Workspace Structure

The project is structured as a Cargo workspace with the following crates:

- **`bellows`**: The UEFI bootloader. Responsible for loading the kernel and setting up the framebuffer configuration.
- **`fullerene-kernel`**: The core kernel. Handles low-level hardware initialization and enters the main loop.
- **`flasks`**: The build and task runner. Builds the kernel and bootloader, creates a bootable ISO, and launches QEMU for emulation.
- **`petroleum`**: A library providing common EFI types, utilities, and serial output macros for no_std environments.
- **`toluene`**: Placeholder for userland components (e.g., user-space binaries). Currently minimal.

Dependencies include `x86_64` for architecture-specific code, `uefi` for bootloader interaction, and custom utilities in `petroleum`.

## Building and Running

### Prerequisites

- Rust nightly toolchain (required for `no_std` and UEFI targets): Install via `rustup toolchain install nightly`.
- QEMU: Install on Linux/macOS via package manager (e.g., `apt install qemu-system-x86` on Ubuntu).
- OVMF (UEFI firmware): Included in `flasks/ovmf/` (RELEASEX64 files). If missing, download from [TianoCore releases](https://github.com/tianocore/edk2/releases).

The project uses a custom target `x86_64-unknown-uefi` (ensure it's available via `rustup target add x86_64-unknown-uefi`).

### Build and Run

Run the task runner, which handles building, ISO creation, and QEMU emulation:

```bash
cargo run --bin flasks
```

This command:
1. Builds `fullerene-kernel` and `bellows` for the UEFI target.
2. Creates a FAT image and ISO (`fullerene.iso`) with the bootloader and kernel.
3. Launches QEMU with:
   - 512MB RAM.
   - Std VGA.
   - Serial output to stdout (for logs).
   - OVMF firmware for UEFI booting.
   - Boot from the ISO (order=d).

Expected output:
- Serial logs from bootloader: Heap init, GOP init, kernel load.
- VGA text: Kernel init messages, framebuffer config (if found), "Initialization complete. Entering kernel main loop."
- The VM will boot into the kernel and halt (no further activity).

To debug:
- QEMU logs are written to `qemu_log.txt` (interrupts and other debug info).
- Use `RUST_LOG=debug cargo run --bin flasks` for more verbose output.

For release builds, edit `flasks/src/main.rs` to use `--profile release` or run `cargo build --release`.

### Manual Build

If you prefer manual steps:

1. Build bootloader:
   ```bash
   cargo +nightly build -Zbuild-std=core,alloc --package bellows --target x86_64-unknown-uefi
   ```

2. Build kernel:
   ```bash
   cargo +nightly build -Zbuild-std=core,alloc --package fullerene-kernel --target x86_64-unknown-uefi
   ```

3. Create ISO: Modify/use tools like `isobemak` (dependency in flasks) or manual EFI filesystem creation.

4. Run in QEMU:
   ```bash
   qemu-system-x86_64 \
     -m 512M \
     -cpu qemu64,+smap \
     -vga std \
     -serial stdio \
     -drive if=pflash,format=raw,readonly=on,file=flasks/ovmf/RELEASEX64_OVMF_CODE.fd \
     -drive if=pflash,format=raw,unit=1,file=flasks/ovmf/RELEASEX64_OVMF_VARS.fd \
     -cdrom fullerene.iso \
     -boot order=d \
     -no-reboot \
     -d int \
     -D qemu_log.txt
   ```

## Development

- **Toolchain**: Use `rust-toolchain.toml` for pinning nightly.
- **Panic Policy**: Aborts in dev/release for no_std compatibility.
- **Allocator**: Currently a dummy that panics; implement a real one (e.g., linked list or bump) next.
- **Testing**: Run in QEMU as above. For unit tests, add `#[test]` in crates (note no_std limitations).
- **Debugging**: Use serial output and QEMU's `-s -S` for GDB attachment.

## TODO / Next Steps

- Implement a real global allocator and memory management (pager, page frames).
- Handle interrupts properly (e.g., timer, keyboard).
- Port or develop userland applications in `toluene`.
- Add syscall interface between kernel and userland.
- Support for file systems (e.g., FAT from ISO).
- Graphics mode beyond VGA text (use framebuffer).

See issues on GitHub for tracked tasks.

## Contributing

Bug reports, feature suggestions, and pull requests are welcome! Please see [CONTRIBUTING.md](docs/CONTRIBUTING.md) for guidelines on submitting contributions.

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
