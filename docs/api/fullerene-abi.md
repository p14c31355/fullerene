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
- `device_ioctl::{GET_PCI_INFO, READ_PCI_CONFIG, WRITE_PCI_CONFIG}` and the
  `PciDeviceInfo`/`PciConfigRequest` argument records for native PCI handles.

Every extensible pointer-facing type has a fixed `MIN_BYTE_SIZE`, a current
`BYTE_SIZE`, a native-endian serializer, and compile-time size/alignment
assertions. Kernels accept buffers at least as large as `MIN_BYTE_SIZE` and
copy only the prefix that fits in the caller's buffer. Reserved fields must be
written as zero and retained when extending a structure.

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

## Native device handles

`open_device` accepts a PCI BDF such as `02:03.0`, a `vendor:device` pair such
as `8086:5845`, or a stable NVMe name such as `nvme0`. The initial native ioctl
surface is intentionally limited to PCI identity and configuration-space
access; unsupported commands return `NotSupported`.

`READ_PCI_CONFIG` and `WRITE_PCI_CONFIG` use `PciConfigRequest`. The width is
1, 2, or 4 bytes and the offset must be naturally aligned. Reads update the
request's `value` field in the caller's buffer. Writes require the handle's
write permission.

`INITIALIZE_NVME` accepts no argument. It submits one initialization request to
the kernel-owned SQ and returns the `nvmeN` controller index after the
corresponding completion has been written to and consumed from the CQ. Other
NVMe data-path commands are not accepted yet.
