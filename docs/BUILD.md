# Building and Running

## Prerequisites

- Rust nightly toolchain (required for no_std and UEFI targets): Install via `rustup toolchain install nightly`.
- QEMU: Install on Linux/macOS via package manager (e.g., `apt install qemu-system-x86` on Ubuntu).
- OVMF (UEFI firmware): Included in `flasks/ovmf/` (RELEASEX64 files). If missing, run with `--clone-ovmf` to copy from system installation or download from [TianoCore releases](https://github.com/tianocore/edk2/releases).

## Application Ports

The repository includes third‑party application port definitions that are
automatically built from submodule sources and embedded into the kernel
via a CPIO initramfs archive.

| Port | Submodule | Runtime | Build method |
|------|-----------|---------|--------------|
| Cargo | `toluene/cargo` | Linux ELF | `cargo build --release` (cold ~2 min, cached) |
| FREEDOOM | `toluene/freedoom` | Linux ELF | `make` + Chocolate Doom download |
| NetSurf | `toluene/netsurf` | Linux ELF | `make` (requires gtk3, libcurl, …) |
| VSCodium | `toluene/vscodium` | Linux ELF | Manual overlay (needs Microsoft/vscode) |

The kernel's `build.rs` runs each port's build step during compilation.
Built binaries are cached at `toluene/<name>/app.bin` and reused on
subsequent builds.  To force a rebuild, delete the cached binary:

```bash
rm toluene/<name>/app.bin
cargo build -p fullerene-kernel --target x86_64-unknown-uefi
```

Prerequisites per port:

- **cargo** – Rust toolchain (`cargo` + `rustc`)
- **freedoom** – `make`, `python3`, `deutex`, `curl` (engine download)
- **netsurf** – `make`, gtk3-dev, libcurl4-openssl-dev, libxml2-dev, …
- **vscodium** – npm, build toolchain (see `toluene/vscodium/build.sh`)

A port whose build prerequisites are missing is silently skipped.  You
can place a manually‑compiled ELF at `toluene/<name>/app.bin` as well.

When the kernel boots, ports are unpacked from the initramfs into
`/packages/` and launched with `app run <name>`.

### Manual runtime installation

Ports can also be installed at runtime without a kernel rebuild:

```
app install <name> <path-to-elf>
app run <name>
app remove <name>
```

## Build and Run

Run the task runner, which handles building, ISO creation, and QEMU emulation:

```bash
cargo run -p flasks --bin flasks
```

This command:
1. Builds `fullerene-kernel` and `bellows` for the UEFI target with `x86_64-unknown-uefi`.
2. Creates a FAT image and ISO (`fullerene.iso`) with the bootloader and kernel.
3. Launches QEMU with:
   - 4GB RAM.
   - VirtIO-GPU with SDL display (1024x768 default resolution).
   - Serial output to stdout (for logs).
   - OVMF firmware for UEFI booting.
   - Boot from the ISO.

## QEMU Options

Flasks supports dynamic VGA/display configuration via CLI arguments:

| Argument | Default | Description |
|----------|---------|-------------|
| `--vga <type>` | `virtio-gpu` | VGA device: `virtio-gpu`, `std`, `qxl`, `cirrus`, `none` |
| `--display <backend>` | `sdl` | Display backend: `gtk`, `sdl`, `none`, `curses` |
| `--resolution <WxH>` | `1024x768` | Screen resolution (virtio-gpu/qxl only) |
| `--headless` | false | Run QEMU in headless mode (no GUI) |
| `--timeout <seconds>` | none | Timeout for QEMU execution in seconds |
| `--clone-ovmf` | false | Copy OVMF binaries from system installation to project |

Examples:
```bash
# std-vga (Bochs VBE) for framebuffer debugging
cargo run --bin flasks -- --vga std

# QXL with SDL backend
cargo run --bin flasks -- --vga qxl --display sdl

# Headless mode (serial only, no GUI)
cargo run --bin flasks -- --display none

# Custom resolution with virtio-gpu
cargo run --bin flasks -- --resolution 1280x720

# Run with a timeout
cargo run --bin flasks -- --timeout 30
```

Expected output:
- Serial logs from bootloader: Heap init, GOP init, kernel load.
- VGA/graphics framebuffer initialization and Lattice compositor startup.
- Shell interface becomes available after scheduler starts running processes (via GUI terminal or serial).
- System runs multi-tasking kernel with shell interaction available.

To debug:
- QEMU logs are written to `qemu_log.txt` (interrupts and other debug info).
- Use `RUST_LOG=debug cargo run --bin flasks` for more verbose output.

For release builds, use `cargo build --release` to compile with optimizations.

## Manual Build Steps

For manual building without the task runner:

1. Build bootloader:
   ```bash
   cargo +nightly build -Zbuild-std=core,alloc --package bellows --target x86_64-unknown-uefi
   ```

2. Build kernel (repeat for updated kernel binary):
   ```bash
   cargo +nightly build -Zbuild-std=core,alloc --package fullerene-kernel --target x86_64-unknown-uefi
   ```

3. Create ISO: The build process copies the kernel binary into the bootloader, then creates a UEFI-bootable ISO using tools like `isobemak`.

4. Run in QEMU:
   ```bash
   qemu-system-x86_64 \
     -m 4G \
     -cpu qemu64,+smap,+invtsc \
     -smp 1 \
     -M q35,usb=off,pcspk-audiodev=speaker \
     -vga none \
     -device virtio-gpu-pci,disable-legacy=on,disable-modern=off,xres=1024,yres=768 \
     -display sdl,gl=off \
     -serial stdio \
     -accel tcg,thread=single \
     -d int,cpu_reset,guest_errors,unimp \
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
     -audiodev pa,id=speaker,out.mixing-engine=off \
     -audiodev pa,id=hda,timer-period=1000,out.mixing-engine=off \
     -device intel-hda,debug=0 \
     -device hda-duplex,audiodev=hda
