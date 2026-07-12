# Fullerene Kernel — Public API (v0.1)

> **Status: DRAFT — Subject to Freeze**
>
> ABI and data structures exposed by the kernel to user space and other crates.

---

## 1. Syscall ABI

`petroleum::common::syscall::*`

| Calling Convention |
|---|
| `syscall` instruction (x86-64) |
| rax = syscall number, rdi/rsi/rdx/r10/r8/r9 = args |
| Return value: rax, errors encoded in rax |

### Syscall numbers

`petroleum/src/common/syscall.rs`:

| # | Name | Description |
|---|------|-------------|
| 0 | `Uptime` | µs since system boot |
| 1 | `GetPid` | Current process PID |
| 2 | `ClockGetTime` | Wall clock time |
| 3 | `Exit` | Terminate process |
| 4 | `Write` | Write to fd |
| 5 | `Read` | Read from fd |
| 6 | `Open` | Open a file |
| 7 | `Close` | Close an fd |
| 8 | `Spawn` | Create a new process |
| 9 | `WaitPid` | Wait for child process completion |
| 10 | `Mmap` | Memory mapping |
| 11 | `Munmap` | Unmap memory |
| 12 | `SchedYield` | Explicit CPU yield |
| 13 | `CreateThread` | Create a thread |
| 14 | `ExitThread` | Terminate a thread |
| 15 | `SendEvent` | Send an event |
| 16 | `RecvEvent` | Receive an event |

### Error Handling

Negative return value = error (EINVAL, ENOENT, EACCES, ENOMEM, EAGAIN, ...).

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
