# Fullerene Kernel — Public API (v0.1)

> **Status: DRAFT — Subject to Freeze**
>
> ABI and data structures exposed by the kernel to user space and other crates.

---

## 1. Syscall ABI

The stable contract is defined by `fullerene-abi`; Toluene exposes the
user-space wrappers in `toluene::sys`.

| Calling Convention |
|---|
| `syscall` instruction (x86-64) |
| rax = syscall number, rdi/rsi/rdx/r10/r8/r9 = args |
| Return value: rax; failures are negative `SyscallErrorCode` values |

### Syscall numbers

`fullerene_abi::SyscallNumber` is the authoritative typed list. Compatibility
constants remain available from `fullerene_abi::syscall_numbers`.

| Range | Area |
|---|---|
| 0 | ABI version and capability query |
| 1–22 | process and basic I/O |
| 30–39 | memory |
| 40–49 | events |
| 50–59 | threads |
| 60–69 | windows |
| 70–79 | devices |
| 80–89 | IPC |
| 90–99 | handles/capabilities |
| 100–109 | clocks and timers |

Syscall 0 is backwards compatible: with no arguments it returns
`AbiVersion::CURRENT.pack()`. With a writable `AbiInfo` buffer and its size in
the first two arguments it writes the ABI version, DTO size, native syscall
count, and capability bitset, then returns the number of bytes written.

### Error Handling

Negative return value = error. The positive codes are fixed by
`fullerene_abi::SyscallErrorCode` and align with Linux errno values where
possible.

### Kernel handler boundaries

Native syscall routing is centralized in `syscall/dispatch.rs`. Handler
implementations are grouped by the resource or lifecycle they own:

| Module | Responsibility |
|---|---|
| `abi` | ABI version and capability discovery |
| `process` | process lifecycle, waiting, identity, and per-process resource access |
| `fs` | native file descriptors and terminal I/O |
| `memory` | user address-space mapping and protection |
| `event`, `thread`, `window` | event, thread, and window lifecycles |
| `device`, `ipc`, `cap`, `time` | device access, IPC, handles, clocks, and timers |
| `interface`, `types` | shared errors, user-copy helpers, and kernel object types |

The former mixed-responsibility `handlers.rs` / `basic.rs` path is not part of
the kernel interface. Adding a syscall requires adding its typed ABI number,
placing its implementation in the owning domain module, and wiring only the
central dispatch match.

---

## 2. VDSO (Read-Only Metadata Page)

Fixed mapping at `0x7000_0000_0000`. Provides zero-copy read-only access to kernel metadata.

| Offset | Type | Content |
|---|---|---|
| 0 | `AtomicU64` | time_us — wall clock (µs) |
| 8 | `AtomicU64` | uptime_us — time since boot (µs) |
| 16 | `u64` | pid — current process PID |

Kernel writes with `Ordering::Release`, user space reads with `Ordering::Acquire`.

---

## 3. Process Management

### Process

`crate::process::Process`

```rust
pub struct Process {
    pub pid: u64,
    pub state: ProcessState,
    pub name: String,
    pub registers: [u64; 32],
    pub page_table: PhysAddr,
    // ... (internal implementation)
}
```

### SchedulerContext

`crate::scheduler_context::SchedulerContext`

```rust
pub static SCHEDULER: spin::Mutex<SchedulerContext>;
```

`SCHEDULER` is the only global independent of the `KERNEL` lock.

---

## 4. Klog

| Macro | Description |
|---|---|
| `klog_fmt!(fmt, ...)` | Kernel log output (framebuffer + serial) |
| `boot_stage!(BootStage::X)` | Boot stage marker (panic screen color) |

---

## Changelog

| Date | Change |
|---|---|
| 2026-07-13 | v0.1 initial |
