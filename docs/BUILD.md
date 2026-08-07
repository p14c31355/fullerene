# Building and Running

## Prerequisites

- Rust nightly toolchain (required for no_std and UEFI targets): Install via `rustup toolchain install nightly-2026-08-01`.
- `wasm32-wasip1` Rust target (required by the kernel's embedded WASM build): Install with `rustup target add --toolchain nightly-2026-08-01 wasm32-wasip1`.
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
Built binaries are cached at `target/ports/<name>/app.bin` and reused on
subsequent builds.

**Source builds are opt-in.**  By default, `cargo build` only uses cached
binaries.  To build ports from their submodule sources, set:

```bash
FULLERENE_BUILD_PORTS=1 cargo build -p fullerene-kernel --target x86_64-unknown-uefi
```

To force a rebuild from source, delete a cached binary and rebuild with
the env var:

```bash
rm target/ports/<name>/app.bin
FULLERENE_BUILD_PORTS=1 cargo build -p fullerene-kernel --target x86_64-unknown-uefi
```

Prerequisites per port:

- **cargo** – Rust toolchain (`cargo` + `rustc`)
- **freedoom** – `make`, `python3`, `deutex`, `curl` (engine download)
- **netsurf** – `make`, gtk3-dev, libcurl4-openssl-dev, libxml2-dev, …
- **vscodium** – npm, build toolchain (see `toluene/vscodium/build.sh`)

Missing optional port caches are silently skipped during ordinary checks and
builds, so a clean clone does not pollute `cargo check` with warnings. To
request source builds and their diagnostics explicitly, set
`FULLERENE_BUILD_PORTS=1`. You can also place a manually‑compiled ELF at
`target/ports/<name>/app.bin`.

The kernel build also compiles the WASI fixtures and the nested
`toluene/viewer` and `toluene/emulsion` workspaces for `wasm32-wasip1`. These
WASM release builds use size optimization, LTO, one codegen unit, and symbol
stripping; their cached outputs are copied into the kernel `OUT_DIR` before
the applications are embedded. The normal kernel build and workspace warning-
free gate include these embedded WASM outputs, so install the target above
before running them; the optional ELF application ports remain separate.

When the kernel boots, ports are unpacked from the initramfs into
`/packages/` and launched with `app run <name>`.

### Manual runtime installation

Ports can also be installed at runtime without a kernel rebuild:

```console
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
1. Builds optimized `fullerene-kernel` and `bellows` artifacts with the `release` profile for the UEFI target `x86_64-unknown-uefi`.
2. Creates a FAT image and ISO (`fullerene.iso`) with the bootloader and kernel.
3. Launches QEMU with:
   - 4GB RAM.
   - VirtIO-GPU with SDL display (1024x768 default resolution).
   - Serial output to stdout (for logs).
   - OVMF firmware for UEFI booting.
   - Boot from the ISO.

To rebuild only the ISO without opening QEMU, use the long argument:

```bash
cargo run -p flasks --bin flasks -- --iso-only
```

This still rebuilds `fullerene-kernel` and `bellows`, then writes
`fullerene.iso` and exits before preparing OVMF variables or launching QEMU.
Use `--debug` when unoptimized development artifacts are needed; this writes
UEFI outputs under `target/x86_64-unknown-uefi/debug`.

### Installing Fullerene on a SATA SSD

Boot the generated UEFI ISO on the test machine with the target SATA SSD
connected. The AHCI probe registers identified disks as `/dev/sataNpN`. The
desktop's **Install Fullerene** icon opens a graphical wizard that lists the
available disks, shows the destructive warning, and asks for confirmation.
For headless/debug sessions, the equivalent shell command is:

```console
install_fullerene list
install_fullerene /dev/sata0p0 --confirm
```

Installation is deliberately destructive. It requires a 512-byte-sector disk
with at least 64 MiB available after LBA 2048, writes a small MBR-partitioned
FAT32 EFI System Partition, and copies the running ISO's `BOOTX64.EFI` and
`KERNEL.EFI` payloads into `EFI/BOOT/`. NVMe targets and BIOS-only boots are
not supported by this installer yet.

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
| `--iso-only` | false | Rebuild `fullerene.iso` and exit without launching QEMU |
| `--debug` | false | Use Cargo's `dev` profile instead of the default `release` profile |

Examples:
```bash
# Rebuild only fullerene.iso without opening QEMU
cargo run -p flasks --bin flasks -- --iso-only

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

### Linux-musl Rust `std` smoke test

Install the official static-musl target once:

```bash
rustup target add --toolchain nightly x86_64-unknown-linux-musl
```

Then build an ordinary Rust `std` program, embed it at
`/bin/rust-std-hello`, boot it through Solvent's Linux personality, and stop
QEMU automatically when the process exits successfully:

```bash
FULLERENE_LINUX_MUSL_SMOKE=1 \
  cargo run -p flasks -- --display none --vga none --timeout 70
```

The smoke test dispatches `exec /bin/rust-std-hello` through Nozzle. It
only asks QEMU to exit successfully after observing the expected stdout,
exit status 0, and the shell resuming. The end-to-end success markers on the
serial console are:

```text
Hello from Rust std on musl!
[linux-smoke] PASS: fixture output observed, exit=0, shell resumed
```

Without the smoke environment variable, the same embedded executable can be
started from the Fullerene shell with `exec /bin/rust-std-hello`. The source fixture is
kept in `fullerene-kernel/examples/linux_musl_hello.rs`; it uses the official
`x86_64-unknown-linux-musl` `std` and does not depend on a Fullerene-specific
standard library. Linux stdout and stderr are mirrored to the serial console
and the interactive Lattice terminal, so the Hello line appears in the shell
before the next prompt.

Nozzle exposes the Linux and WASI launchers through this single command:

```text
exec /bin/hello_linux             # embedded Linux ABI fixture
exec /bin/rust-std-hello          # static Rust std/musl ELF
exec /bin/busybox                 # interactive BusyBox sh
exec /apps/hello.wasm             # embedded WASI fixture
exec /apps/viewer.wasm <image>    # WASI image viewer
exec /apps/emulsion.wasm         # desktop capture (defaults to capture)
```

Paths ending in `.wasm` use the WASI runtime; other paths are loaded as Linux
ELF binaries. BusyBox keeps its dedicated terminal window and starts `sh`.

Expected output:
- Serial logs from bootloader: Heap init, GOP init, kernel load.
- VGA/graphics framebuffer initialization and Lattice compositor startup.
- Shell interface becomes available after scheduler starts running processes (via GUI terminal or serial).
- System runs multi-tasking kernel with shell interaction available.

### Dynamically linked glibc BusyBox

`exec /bin/busybox` launches a dynamically linked x86_64 glibc BusyBox as
`busybox sh`.
The kernel's Toluene build step builds the checked-in `toluene/busybox`
submodule with the Rust build orchestration, validates its glibc `PT_INTERP`
and `libc.so.6` dependency, and embeds it automatically. Initialize the
submodule and install `make` plus `gcc` first:

```bash
git submodule update --init --recursive
```

Set `FULLERENE_BUSYBOX` to use an existing dynamically linked glibc x86_64
BusyBox instead of building the submodule, or set `FULLERENE_BUSYBOX_CC` to
choose the compiler used for the submodule build. The build defaults to
`gcc`; a static or musl-linked binary is rejected.

```bash
cargo run -p flasks -- --iso-only
```

This single command performs the dynamic glibc BusyBox release build, kernel
embedding, and ISO creation. The retained BusyBox artifact is
`target/busybox/busybox`. The kernel build keeps its private out-of-tree
objects under Cargo's `OUT_DIR` for reuse and concurrent-build isolation.
When invoking the standalone `busybox-build` command, its default
`target/busybox-build/` directory is retained unless `--clean` is supplied.

Then enter `exec /bin/busybox` in the Nozzle shell. Fullerene opens a focused
`BusyBox` window and attaches the Linux process's stdin/stdout/stderr to that
window, so typing there is delivered to `busybox sh` while the original Nozzle
terminal remains available. The shell receives a minimal Linux environment
(`PATH`, `HOME`, `SHELL`, and `TERM`) and remains a normal Linux ELF process
under the shared Linux personality layer.

For a headless end-to-end check that exercises the Nozzle command, interactive
BusyBox `sh`, terminal stdin/stdout, blocking input wait, exit status,
scheduler handoff, window cleanup, and shell resumption, run:

```bash
FULLERENE_BUSYBOX_SMOKE=1 \
  cargo run -p flasks -- --display none --vga none --timeout 900
```

When `/dev/kvm` is available, `FULLERENE_QEMU_ACCEL=kvm` may be set to use
QEMU hardware acceleration; the default remains single-threaded TCG.

The smoke build uses the exact generated applet contract: it checks the
`busybox --help`/`--list` dispatcher and count, then runs every listed name
through the bundled BusyBox shell's standalone applet dispatch. QEMU is
accepted only with the smoke debug-exit status after the success marker, exit
status 0, terminal cleanup, and shell resumption.

For the corresponding real-hardware check, build the same smoke image without
launching QEMU:

```bash
FULLERENE_BUSYBOX_SMOKE=1 \
  cargo run -p flasks -- --iso-only
```

Boot the resulting `fullerene.iso` on the target UEFI machine and keep the
serial log or Klog Live open. Accept the run only after seeing
`[busybox-smoke] PASS: all bundled applets ran, exit=0, shell resumed` and
the BusyBox terminal-owner exit marker with `code=0 terminal closed`. This
uses the same embedded binary and generated BusyBox contract
(`busybox-applets.txt` and its generated count) as the strict QEMU run. The
QEMU-only `FULLERENE_BUSYBOX_SMOKE_QEMU_EXIT` flag is
injected automatically by Flasks for this QEMU path. It is omitted for
hardware, so the physical image never writes to the `isa-debug-exit` port.
The BusyBox window shown during this scripted run is a smoke-test terminal, not
an interactive prompt. It may remain visually unchanged while the script is
running, but the run must eventually emit both success markers; a window that
stays there without `PASS` is a failed smoke run.

To debug:
- QEMU logs are written to `qemu_log.txt` (interrupts and other debug info).
- Use `RUST_LOG=debug cargo run --bin flasks` for more verbose output.

For release builds, use `cargo build --release` to compile with optimizations.
The workspace release profile uses aborting panics, LTO, one codegen unit, and
strips debug information from the produced binaries; none of these changes
alter the runtime ABI or framebuffer behavior.

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
