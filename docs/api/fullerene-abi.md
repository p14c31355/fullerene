# Fullerene ABI

`fullerene-abi` is the dependency-free, no_std contract between the kernel and
user-space SDK. Kernel-only policy and raw x86 instructions do not belong in
this crate.

## Stable definitions

- `SyscallNumber`: typed native syscall numbers, plus the raw compatibility
  constants in `syscall_numbers`.
- `SyscallErrorCode`: positive error numbers; the syscall return convention
  negates them.
- `AbiVersion`, `Capability`, `CapabilitySet`, and `AbiInfo`: version and
  feature discovery.
- `MemoryInfo`, `TimeSpec`, `DeviceInfo`, and `WindowEvent`: fixed-layout
  `#[repr(C)]` records for pointer-based syscall arguments.

Every pointer-facing type has a public `BYTE_SIZE`, native-endian serializer,
and a compile-time size/alignment assertion. Reserved fields must be written as
zero and retained when extending a structure.

## ABI query

Native syscall 0 supports two forms:

```text
syscall(AbiQuery, 0, 0, ...)                    -> packed AbiVersion
syscall(AbiQuery, info_ptr, AbiInfo::BYTE_SIZE) -> bytes written
```

The first form preserves compatibility with the original version-only query.
The second fills `AbiInfo`, including a capability bitset that lets newer SDKs
detect optional kernel facilities at runtime.

## Compatibility rules

- Existing syscall numbers and error codes are never renumbered.
- A DTO may grow only by consuming reserved space or appending fields.
- Callers pass their buffer size; kernels reject buffers smaller than the
  versioned structure they write.
- Toluene depends on `fullerene-abi` directly. Petroleum only re-exports the
  syscall-number type for older callers.
