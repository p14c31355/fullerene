# Development

## Toolchain

Use `rust-toolchain.toml` for pinning nightly (currently `nightly-2026-06-01`).

## Panic Policy

Aborts in dev/release for no_std compatibility.

## Building

```bash
# Host check (catches most compilation errors)
cargo check -p fullerene-kernel

# Full UEFI build
cargo build -Zbuild-std=core,alloc -p fullerene-kernel --target x86_64-unknown-uefi

# Run in QEMU
cargo run -q -p flasks -- --vga std
```

## Testing

Run unit tests for library crates with `cargo test -p <crate>` (chronoline,
resonance, nozzle, lattice, petroleum, genome, carrier have host-runnable
tests).  Kernel tests require a UEFI target.

## Debugging

Use serial output and QEMU logging. For GDB debugging, enable QEMU GDB
stub with `-s -S`.  On real hardware (InsydeH2O), a framebuffer panic
screen replaces serial: the boot stage is encoded as a coloured screen
at the top of the display even before the GUI initialises.