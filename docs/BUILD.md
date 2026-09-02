# Building and Running

## Prerequisites

- Rust nightly toolchain (required for no_std and UEFI targets): Install via `rustup toolchain install nightly`.
- The `aarch64-unknown-none`, `wasm32-wasip1`, and `x86_64-unknown-linux-musl` Rust targets (required by the AArch64 bootstrap, embedded WASM, and Linux fixture builds) are installed by the toolchain file or `rustup target add`.
- QEMU: Install on Linux/macOS via package manager (e.g., `apt install qemu-system-x86` on Ubuntu).
- AArch64 QEMU (`qemu-system-aarch64`) is required for the `qemu-virt` regression path.
- OVMF (UEFI firmware): Included in `flasks/ovmf/` (RELEASEX64 files). If missing, run with `--clone-ovmf` to copy from system installation or download from [TianoCore releases](https://github.com/tianocore/edk2/releases).

## Application Ports

### Intel Wi-Fi firmware submodule

The `bonder/iwlwifi` submodule tracks `linux-firmware`, but the kernel only
consumes files below `intel/iwlwifi`. Initialize it shallowly and enable the
same sparse checkout used by CI:

```bash
git submodule update --init bonder/iwlwifi
git -C bonder/iwlwifi sparse-checkout set intel/iwlwifi
```

The UEFI build requires the matching firmware files to be present in that
directory.

For a local 7265D firmware experiment, keep the replacement outside the
submodule and override it only for that build:

```bash
FULLERENE_IWLWIFI_7265D_FW=/path/to/iwlwifi-7265D-29.ucode \
  cargo run -p flasks --bin flasks -- --iso-only
```

Without the environment variable, the tracked submodule firmware is used.

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

### AArch64 QEMU and Bramble artifacts

The AArch64 path is a separate bare-metal artifact under the same
`fullerene-kernel` package:

```bash
rustup target add aarch64-unknown-none
cargo run -q -p flasks -- build --arch aarch64 --platform qemu-virt
cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt
```

The Bramble build emits an ELF, flat binary, Linux arm64 `Image`, and a
LZ4-frame `Image.lz4` compatible with the Android kernel loader:

```bash
cargo run -q -p flasks -- build --arch aarch64 --platform bramble
```

### ESP32/Xtensa bring-up artifacts

The ESP32 path uses the Rust ESP toolchain and Flasks for build, image, flash,
and monitor orchestration. Install `espup`, run `espup install`, then build:

```bash
cargo run -q -p flasks -- build --arch xtensa --platform esp32-xh32s
```

Flash (default device discovery) or explicitly select a serial device:

```bash
cargo run -q -p flasks -- flash --arch xtensa --platform esp32-xh32s
cargo run -q -p flasks -- flash --arch xtensa --platform esp32-xh32s   --serial /dev/ttyUSB0
```

`run` flashes and then attaches the serial monitor. `monitor` attaches without
flashing and requires `--serial`:

```bash
cargo run -q -p flasks -- run --arch xtensa --platform esp32-xh32s   --serial /dev/ttyUSB0
cargo run -q -p flasks -- monitor --arch xtensa --platform esp32-xh32s   --serial /dev/ttyUSB0
```

Flasks invokes Cargo with `xtensa-esp32-none-elf`, builds
`fullerene-kernel-esp32`, and converts the ELF into a native ESP32 flash image.
The image includes only file-backed `PT_LOAD` data; startup clears BSS using the
`__bss_start`/`__bss_end` symbols supplied by linker placement options. There is
no Fullerene linker script. The Xtensa target also compiles without the windowed
register ABI so the first scheduler has an explicit call0 context-switch
protocol. Optional independent audit:

```bash
esptool image-info target/xtensa-esp32-none-elf/release/fullerene-kernel-esp32.bin
```

The ESP32 build succeeds when ELF and image generation succeed. It does not
imply LCD, touch, SDMMC, scheduler, timer interrupt, or desktop hardware
bring-up is complete.

An Android v3 boot template can be patched with the generated `Image.lz4`:

```bash
cargo run -q -p flasks -- build --arch aarch64 --platform bramble \
  --boot-template /path/to/boot.img \
  --boot-output /path/to/fullerene-boot.img
```

The Bramble image path performs a preflight audit before it is reported as
ready: it re-reads the Android v3 header, verifies the generated kernel bytes,
checks page padding, and compares the ramdisk and trailing vendor data with
the stock template. `Image` and `Image.lz4` are also decoded and checked at
creation time. To run the QEMU shared USB protocol self-test before the
Bramble build (and before a `run`/Fastboot handoff), add:

```bash
cargo run -q -p flasks -- run --arch aarch64 --platform bramble \
  --boot-template /path/to/boot.img \
  --qemu-preflight
```

This QEMU preflight uses `virt` for the generic Rust/DWC3 protocol model. It
does not claim to emulate Bramble's SM7250 PHY, Qualcomm Type-C glue, or SMMU;
those remain hardware-only checks.

The patcher keeps the existing ramdisk and removes stale AVB metadata because
it cannot sign the resulting image. Use an unlocked development device; this
is a temporary `fastboot boot` image, not a partition-flashing artifact.
For Android v3 devices the DTB is supplied by the companion
`vendor_boot.img`; this path leaves vendor_boot untouched and consumes the DTB
passed by the bootloader in the AArch64 entry registers.

With a Bramble in Fastboot mode, Flasks can inspect the USB device and send a
non-destructive RAM boot. The command checks `getvar:product` and accepts only
`bramble`; it refuses multiple connected Fastboot devices:

```bash
cargo run -q -p flasks -- device
cargo run -q -p flasks -- boot --arch aarch64 --platform bramble \
  /path/to/fullerene-boot.img
```

The `boot` action does not write a partition. `flash` and `erase` are not
exposed by Flasks yet.

For a repeatable, non-destructive Bramble USB experiment, use the Rust
host-side harness below while the phone is on the red-triangle Fastboot
screen:

```bash
cargo run -q -p flasks --bin bramble-usb -- loop
```

The harness checks `product=bramble`, builds with the QEMU protocol preflight,
audits the Android v3 boot image, and invokes only `fastboot boot`. After the
handoff it waits for the bootloader's `18d1:4ee0` device to disappear and
accepts a result only when the Fullerene gadget's `1234:0001` identity appears.
It then captures `lsusb -v`, holds the gadget for a bounded interval, and
saves build, boot, and kernel logs under a temporary run directory. A failed
handoff is reported as a failure rather than being confused with a still-live
bootloader session.

Useful comparisons are:

```bash
cargo run -q -p flasks --bin bramble-usb -- loop --uncompressed
cargo run -q -p flasks --bin bramble-usb -- loop --normal
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu
cargo run -q -p flasks --bin bramble-usb -- loop --no-core-reset
cargo run -q -p flasks --bin bramble-usb -- loop --bare-pullup
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 4
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 9
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 10
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 6
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 11
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --stop-after-stage 12
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --no-transfer-resource
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --android-resource-order
cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu --reuse-fastboot-dma
cargo run -q -p flasks --bin bramble-usb -- matrix
```

The first bypasses generated `Image.lz4`; the second exercises the normal
Bramble AArch64 kernel instead of the dedicated probe. `--no-smmu` must be
combined with `--usb-gadget-handoff-probe`; it leaves the Apps SMMU untouched
and relies on Fastboot's existing physical=IOVA bypass as a hardware
differential. `--no-core-reset` keeps the halted-controller handoff but omits
the DWC3 device soft reset, isolating whether CSFTRST destroys the inherited
PHY/session state. Neither mode flashes, erases, or reboots a partition. After an
enumeration timeout the harness waits up to 150 seconds for the probe watchdog
to return to Fastboot. A probe image built before the watchdog fix can still
leave the phone with no USB device and require manual recovery.

`--reuse-fastboot-dma` is restricted to `--no-smmu` and reuses the event-ring
page that Fastboot had already exposed to DWC3 for the EP0 event ring, setup
packet, TRB, and response buffer. It is a diagnostic only; a successful
enumeration would show that the linker-reserved `.usb_dma` address was not
visible through the firmware-owned SMMU context.

If the temporary boot falls back to Android, the harness recognizes the
`18d1:4ee7` charging/debug identity immediately and saves its USB descriptor,
ADB state, slot, build fingerprint, and kernel version. This is recorded as a
stock fallback, not as Fullerene enumeration. The harness does not reboot the
phone or issue any other recovery command; it waits for host-visible Fastboot
to return before a subsequent probe. The only device-side image operation is
`fastboot boot`, and partitions are left untouched.

The `--stop-after-stage` probes publish the known physical USB2 pull-up after
one handoff boundary and then let the watchdog recover. Stages 1--4 cover
pre-EP0 setup, stage 5 covers both EP0 directions, stage 6 covers the first
SETUP `STARTTRANSFER`, and stage 7 covers Run/Stop. Stage 11 isolates SETUP
TRB publication before `STARTTRANSFER`, while stage 12 stops immediately
after that command. Stage 8 splits the two EP0 directions; stage 9 stops
after EP0 OUT `SETEPCONFIG`, and stage 10 stops after its
`SETTRANSFRESOURCE`. `--no-transfer-resource` removes resource
commands, while `--android-resource-order` tests the older Android msm order
that allocates resources before `SETEPCONFIG`.

The Rust `bramble-usb matrix` command runs the five bounded IRQ-route variants in sequence
and proceeds to the next one only after the probe watchdog has restored
host-visible Fastboot. It stops at the first successful Fullerene gadget. When
a case has already fallen back to Android, matrix only waits for Fastboot and
does not reboot the phone; it never flashes or erases a partition.

`--bare-pullup` is the minimal physical comparison: it omits DWC3 reset,
SMMU, DMA, and EP0 setup, so a host-side descriptor timeout is expected and
does not count as Fullerene enumeration.

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
with at least 64 MiB available after LBA 2048, writes a GPT-partitioned FAT32
EFI System Partition (including the backup GPT at the end of the disk), and
copies the running ISO's `BOOTX64.EFI` and `KERNEL.EFI` payloads into
`EFI/BOOT/`. NVMe targets and BIOS-only boots are not supported by this
installer yet.

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

The default machine is `q35,usb=off`. It exposes the emulated PS/2
keyboard/mouse and the ICH9 SMBus controller (`8086:2930`), but it does not
expose an Intel LPSS DesignWare I²C controller (`8086:54e8`) or an
HID-over-I²C touch device. QEMU's optional `i2c-echo` device is only an I²C
echo peripheral on the ICH9 SMBus; adding it does not emulate the HID-over-I²C
descriptor, reset acknowledgement, or input reports.

Therefore QEMU regression runs validate common boot, PCI, scheduler,
filesystem, and existing PS/2 input paths. The generic I²C-HID transport and
report path are covered by strict Nitrogen tests, while a real N150 probe
still requires the platform-supplied ACPI controller/address/timing
description and hardware.

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

### QEMU xHCI USB rescan smoke test

The USB smoke test adds QEMU's `qemu-xhci` controller and a deterministic
16 MiB USB mass-storage image, then runs `usb_rescan` through the real Nozzle
command path. It passes only when the rescan returns and `usb_info` observes
`/dev/usb0`:

```bash
FULLERENE_USB_XHCI_SMOKE=1 \
  cargo run -p flasks -- --display none --vga none --timeout 30
```

The timeout is intentional: it turns a rescan that never returns into a
failed test instead of leaving QEMU running indefinitely. A successful run
prints:

```text
[usb-xhci-smoke] PASS: usb_rescan registered /dev/usb0
```

### QEMU USB EP0 protocol self-test

The AArch64 self-test runs the shared Fullerene control-endpoint protocol on
QEMU's `virt` machine. It uses QEMU's PL011 serial console for diagnostics
and ARM semihosting to terminate the emulator automatically:

```bash
cargo run -q -p flasks -- run \
  --arch aarch64 \
  --platform qemu-virt \
  --qemu-usb-sim
```

The test covers DWC3 endpoint configuration, SETUP/DATA/STATUS TRBs,
device and endpoint event encoding, EP0 re-arming, device/configuration
descriptors, and the status completion of `SET_ADDRESS` and
`SET_CONFIGURATION`. It models the DWC3 device-mode register protocol but
does not emulate the SM7250 PHY, Qualcomm Type-C glue, or SMMU; those remain
hardware-only.

### Bramble USB handoff loop

With the phone already in the red-triangle Fastboot screen and visible to the
host, the non-destructive hardware loop is:

```bash
cargo run -q -p flasks --bin bramble-usb -- loop
```

It builds the AArch64 probe, runs the QEMU preflight and Android boot-image
audit, sends the image with `fastboot boot`, then requires the bootloader
identity (`18d1:4ee0`) to disappear and the Fullerene identity (`1234:0001`)
to appear and remain present. It saves the build, boot, kernel, descriptor,
USB-tree, and timing records in a temporary run directory, and waits briefly
for the Fastboot USB node to appear before starting the build. The optional
SuperSpeed comparison is:

```bash
cargo run -q -p flasks --bin bramble-usb -- loop --super-speed
```

That variant selects the QMP/SuperSpeed handoff probe and additionally
requires a `5000M` or `10000M` link in `lsusb -t`. Neither mode invokes
`fastboot flash`, `erase`, or reboot; if the probe watchdog must recover the
phone, it waits for the bootloader USB device to return.

The Qualcomm platform IRQ boundaries can be compared without changing the
image workflow, for example:

```bash
cargo run -q -p flasks --bin bramble-usb -- loop --irq-route power
cargo run -q -p flasks --bin bramble-usb -- loop --irq-route typec-role
cargo run -q -p flasks --bin bramble-usb -- loop --irq-route smmu
```

The accepted routes are `power`, `typec`, `typec-role`, `pdc`, and `smmu`.

When the Fullerene gadget is visible, the retained trace can be read without
UART:

```bash
cargo run -q -p flasks --bin bramble-usb -- trace
```

This sends the bounded vendor control request page by page and prints the
decoded `FUTR` records. Use `--serial` to select a specific gadget or
`--timeout` to change the per-page transfer timeout.

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

## Current verification gate (2026-08-16)

The repository's warning gate is:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --exclude bellows --exclude fullerene-kernel
```

The host test gate covers every host-runnable workspace member. `bellows` and
`fullerene-kernel` are UEFI-only; validate them with the UEFI/QEMU smoke paths
below or with a physical hardware run instead of attempting to link their
`no_std` examples into a host test binary.

The retained `fullerene-kernel/examples/native_ipc_rate.rs` is a native user ELF
benchmark for the Fullerene syscall boundary; it is embedded/run by the kernel,
not linked as a host process. It remains available for repeatable on-system
measurements. Release artifacts should be inspected with `size`/`stat` after a
target build; the root release profile already enables LTO, one codegen unit,
symbol stripping, and `panic=abort`.

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
