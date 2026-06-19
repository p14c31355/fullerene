# Development

## Toolchain

Use `rust-toolchain.toml` for pinning nightly.

## Panic Policy

Aborts in dev/release for no_std compatibility.

## Memory Allocation

Uses `linked_list_allocator` for heap management with frame allocation tracking.

## Testing

Run unit tests with `cargo test` for libraries (chronoline, resonance, nozzle, lattice, petroleum have tests). For kernel tests, run in QEMU as above.

## Debugging

Use serial output and QEMU logging. For GDB debugging, enable QEMU GDB stub with `-s -S`.