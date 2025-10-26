# Fullerene

---

![](docs/assets/fullerene-image1.jpg)

---

Fullerene is a complete operating system kernel written in Rust, targeting x86_64 architecture with UEFI booting. It explores modern systems programming concepts including process scheduling, virtual memory management, filesystem abstraction, and syscall interfaces, all implemented in a safe, no_std environment.

Fullerene provides a full-featured kernel with multitasking capabilities, running in both QEMU and VirtualBox virtual machines. The system includes a bootloader, kernel scheduler, process management, memory allocation, device drivers, and user-space support scaffolding.

## Features

- **UEFI Bootloader (Bellows)**: A no_std UEFI application that loads the kernel ELF, initializes framebuffer graphics via Graphics Output Protocol (GOP), sets up custom configuration tables, and transitions to kernel execution after exiting boot services.

- **Full-Featured Kernel (Fullerene-Kernel)** with components including:
  - **Memory Management**: Virtual memory with page tables, heap allocation (linked-list allocator), and physical memory tracking
  - **Process Management**: Full process creation, scheduling, and context switching capabilities
  - **Scheduler**: Preemptive round-robin scheduler with interrupt-driven task switching
  - **Syscall Interface**: Complete system call implementation for user-kernel communication
  - **Filesystem**: Abstraction layer for file operations (currently in-memory implementation)
  - **Graphics Support**: VGA text mode and framebuffer graphics with embedded-graphics integration
  - **Hardware Interfaces**: Keyboard input, serial output, and interrupt handling (APIC/PIC)
  - **Shell**: Basic command-line interface for process interaction

- **Common Library (Petroleum)**: Shared no_std utilities for UEFI types, serial logging, memory operations, graphics primitives, and bare-metal hardware detection.

- **Build System (Flasks)**: Automated task runner for building, ISO creation (using isobemak crate), and virtualization with optional VirtualBox support.

- **Userland placeholder (Toluene)**: Scaffolding for user-space programs in Rust.

The system boots from UEFI firmware, initializes all hardware interfaces, and runs a kernel scheduler that manages multiple processes concurrently. User interaction occurs through a shell interface, with full debugging support via serial logging.

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
- (Optional) VirtualBox: For VirtualBox support, install VirtualBox and create a VM named "fullerene-vm".

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
   - 4GB RAM.
   - Cirrus VGA with GTK display.
   - Serial output to stdout (for logs).
   - OVMF firmware for UEFI booting.
   - Boot from the ISO.

Expected output:
- Serial logs from bootloader: Heap init, GOP init, kernel load.
- VGA/graphics framebuffer initialization and basic graphics setup.
- Shell interface becomes available after scheduler starts running processes.
- System runs multi-tasking kernel with shell interaction available via serial/graphics.

To debug:
- QEMU logs are written to `qemu_log.txt` (interrupts and other debug info).
- Use `RUST_LOG=debug cargo run --bin flasks` for more verbose output.

For release builds, edit `flasks/src/main.rs` to use `--profile release` or run `cargo build --release`.

### VirtualBox Support

To run in VirtualBox instead of QEMU:

```bash
cargo run --bin flasks -- --virtualbox
```

This requires:
- A VirtualBox VM named "fullerene-vm" (created automatically if missing).
- The VM will be configured for UEFI booting with 4GB RAM and appropriate settings.
- Serial output is available on TCP port 6000 for debugging.
- Use `--gui` flag for GUI mode instead of headless.

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
     -m 4G \
     -cpu qemu64,+smap,-invtsc \
     -smp 1 \
     -M q35 \
     -vga cirrus \
     -display gtk,gl=off,window-close=on,zoom-to-fit=on \
     -serial stdio \
     -accel tcg,thread=single \
     -d guest_errors,unimp \
     -D qemu_log.txt \
     -monitor none \
     -drive if=pflash,format=raw,unit=0,readonly=on,file=flasks/ovmf/RELEASEX64_OVMF_CODE.fd \
     -drive if=pflash,format=raw,unit=1,file=flasks/ovmf/RELEASEX64_OVMF_VARS.fd \
     -drive file=fullerene.iso,media=cdrom,if=ide,format=raw \
     -no-reboot \
     -no-shutdown \
     -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
     -rtc base=utc \
     -boot menu=on,order=d \
     -nodefaults
   ```

## Development

- **Toolchain**: Use `rust-toolchain.toml` for pinning nightly.
- **Panic Policy**: Aborts in dev/release for no_std compatibility.
- **Memory Allocation**: Uses `linked_list_allocator` for heap management with frame allocation tracking.
- **Testing**: Run in QEMU as above. For unit tests, add `#[test]` in crates (note no_std limitations).
- **Debugging**: Use serial output and QEMU logging. For GDB debugging, enable QEMU GDB stub with `-s -S`.

## TODO / Next Steps

- Expand filesystem support (block device drivers, persistent storage, FAT filesystem implementation).
- Enhance userspace with full process isolation and multiple user programs.
- Implement advanced memory management (copy-on-write, shared memory, virtual filesystem).
- Add network stack support and device drivers.
- Improve graphics and GUI framework.
- Performance optimizations and kernel hardening.
- Add comprehensive test suite and fuzzing.

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
