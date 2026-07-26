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

The normal warning-free host gate is:

```bash
cargo check --workspace --all-targets
cargo test --workspace
```

The optional port binaries are not required for this gate. Build them only
when testing the packaged application path with `FULLERENE_BUILD_PORTS=1`.

For rendering changes, the reusable host example compares the compositor's
pixel output and can be run with:

```bash
cargo run -p lattice --example render_ppm
# Compare full-frame and disjoint dirty-region composition
cargo run -p lattice --release --example bench_render
```

## Debugging

Use serial output and QEMU logging. For GDB debugging, enable QEMU GDB
stub with `-s -S`.  On real hardware (InsydeH2O), a framebuffer panic
screen replaces serial: the boot stage is encoded as a coloured screen
at the top of the display even before the GUI initialises.

## Verification (2026-07-04 Refactoring)

The following commands were used to verify the VFS/mount refactoring:

```text
cargo test -p genome --locked
  6 passed; 0 failed

cargo check --workspace --exclude bellows --exclude fullerene-kernel --locked
  OK

cargo build -Z build-std=core,alloc \
  --package fullerene-kernel \
  --target x86_64-unknown-uefi \
  --locked
  OK (no compiler warnings)

cargo check --package fullerene-kernel \
  --target x86_64-unknown-uefi \
  --tests \
  --locked
  OK (no warnings including added kernel tests)

cargo clippy --tests -p genome -- -D warnings
  OK (isolated Genome workspace)

git diff --name-only --diff-filter=AM -- '*.rs' | xargs rustfmt --check
  OK

git diff --check
  OK
```

## Current rendering path (2026-07-27)

The kernel owns framebuffer acquisition and scanout submission. Solvent owns
frame pacing, the persistent RAM back buffer, cursor-only updates, and the
runtime-to-desktop bridge. Lattice receives an immutable `Scene` and performs
the layered composition. When dirty regions are present, Lattice recomposes
each clipped region independently; it does not expand disjoint updates into a
single bounding rectangle. Solvent then copies only the queued regions to the
hardware framebuffer. The back buffer remains cursor-free so a cursor move can
restore both the old and new cursor rectangles without reading GOP memory.
