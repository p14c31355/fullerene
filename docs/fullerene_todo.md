# Fullerene TODO

This TODO is aligned with the project architecture (`docs/ARCHITECTURE.md`).

Priority convention:
- **P0** = memory safety / process isolation — do first
- **P1** = structural improvement (ownership, types, tests)
- **P2** = developer experience, performance

### Decision Criteria (future improvements)

Future work should prioritize in this order:

```text
memory safety / process isolation
    → explicit ownership and lifecycle
    → typed contracts
    → deterministic tests
    → module size reduction
    → performance tuning
```

Mere file splitting, moving globals to another file, or wrapping unsafe in a safe wrapper does not count as improvement. An improvement is complete only when the owner, synchronization scope, rollback on failure, and testable boundary are clearly defined.

---

Please refer to the issues for other TODO items.  
You may close them using the "Development" section of the PR, or create new issues based on this Markdown.  

---

## P0: Memory Safety & Process Isolation

### P0-1. fd / handle per-process separation
- [x] `ProcessResources` struct with per-process fd + handle tables (`process.rs`)
- [x] Per-process `alloc_handle`, `with_handle_mut` in `handlers.rs`
- [x] Handle transfer with rollback (transactional move between processes)
- [x] Resource cleanup on process termination (unblock waiters, close handles)
- [x] Handle generation counters (index + generation + cryptographic MAC to detect stale handles)
- [x] Handle permission bits (read/write/signal/duplicate/transfer)
- [x] Syscall return of multiple handles via user buffer (pipe_create)

### P0-2. User memory access unification
- [x] `UserSlice` — validated byte range with `copy_from_user` / `copy_to_user`
- [x] `validate_user_range` — walks current page table (CR3) page-by-page checking PRESENT / USER_ACCESSIBLE / writable flags
- [x] Updated `copy_user_string` to use `UserSlice`
- [x] Updated Linux compat copy functions to use `UserSlice`
- [x] Unify native and Linux ABI copy under one implementation (low priority, both already use `UserSlice`)

### P0-3. `&'static mut` and mutable global ownership
- [x] `with_frame_allocator` guard API (replaces raw `get_frame_allocator_mut`)
- [x] Frame allocator access made `unsafe` with doc comment
- [x] **SchedulerContext**: moved all scheduling state (process list, schedule index, tick counter, NMI recovery RSP/RIP) into a single `pub static SCHEDULER` with explicit lock hierarchy. Replaced old `ProcessManager` global and scattered `AtomicU64` statics in `scheduler.rs`.
- [x] **VDSO read-only**: removed async ring-buffer (slot state machine, `VdsoFuture`, `poll_all_vdso_rings`). VDSO page now only contains read-only metadata (`time_us`, `uptime_us`, `pid`), mapped without `WRITABLE` in user page tables.
- [x] `FramebufferGuard` / `with_framebuffer` closure API ([#267](https://github.com/p14c31355/fullerene/issues/267))
- [x] Solvent cursor fast path: use `FramebufferGuard` instead of raw address ([#269](https://github.com/p14c31355/fullerene/issues/269))
- [x] Trace buffer: fix for multi-CPU safety or document single-core assumption ([#271](https://github.com/p14c31355/fullerene/issues/271))
- [x] Boot-only globals: convert to `Once` or immutable after init ([#273](https://github.com/p14c31355/fullerene/issues/273))

### P0-4. Block cache boundary checks & eviction
- [x] Pre-I/O buffer length validation (`BufferTooSmall` error)
- [x] LBA range check (`LbaOverflow` error)
- [x] True round-robin eviction via `next_victim` field
- [x] `Option<u32>` valid bit instead of `0xFFFF_FFFF` sentinel
- [x] `BlockError` typed error enum
- [x] `FakeBlockDevice` for host testing
- [x] Tests: cache hit, miss, buffer-too-small, LBA overflow, write-invalidate, round-robin

---

## P1: Structural Improvements

### P1-1. Typed errors (replace `&'static str`)
- [x] `BlockError` (done as part of P0-4)
- [x] `FsError` in `genome` crate (FileNotFound, FileExists, PermissionDenied, InvalidFileDescriptor, InvalidSeek, DiskFull, NotADirectory, DirectoryNotEmpty, IsADirectory, InvalidPath, NotSupported, InvalidInput, Io)
- [x] `DriverError`, `MemoryError` in respective crates ([#275](https://github.com/p14c31355/fullerene/issues/275))
- [x] `From` impls to convert to `SyscallError` / Linux errno ([#275](https://github.com/p14c31355/fullerene/issues/275))
- [x] Remove `Result<..., &'static str>` usage (~200+ sites across kernel/nitrogen/genome) ([#277](https://github.com/p14c31355/fullerene/issues/277))

### P1-2. Syscall ABI crate (`fullerene-abi`)
- [x] Extract syscall numbers, error codes, `#[repr(C)]` DTOs into leaf crate ([#279](https://github.com/p14c31355/fullerene/issues/279))
- [x] Both kernel and SDK depend on the shared crate ([#279](https://github.com/p14c31355/fullerene/issues/279))
- [x] `AbiVersion` / capability query syscall ([#279](https://github.com/p14c31355/fullerene/issues/279))

### P1-3. Module splitting by context boundary
- [x] Split `drivers/fat.rs` → `block_device`, `partition`, `cache`, `fat32`, `exfat` ([#281](https://github.com/p14c31355/fullerene/issues/281))
- [x] Split `syscall/handlers.rs` → `dispatch`, `process`, `fs`, `memory`, etc. ([#283](https://github.com/p14c31355/fullerene/issues/283))
- [x] Split `solvent/lib.rs` → `runtime_context`, `input_loop`, `event_loop`, etc. ([#285](https://github.com/p14c31355/fullerene/issues/285))
- [x] Split `usb/xhci/context.rs` → controller init, command, event, transfer, resources ([#288](https://github.com/p14c31355/fullerene/issues/288))
- [x] Split `iwlwifi.rs` → device, firmware, registers, tx, rx, connection_state ([#290](https://github.com/p14c31355/fullerene/issues/290))

### P1-4. Context ownership for callbacks & hooks
- [x] Move `solvent::SOLVENT_CALLBACKS`, `RUNTIME`, `EVENT_QUEUE`, `DISPATCHER` into `RuntimeContext` ([#292](https://github.com/p14c31355/fullerene/issues/292))
- [x] Move `nozzle::FS_HOOKS`, `SYS_HOOKS` into constructor-injected services ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] Move `carrier::SHARED_HISTORY` into terminal session ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] Device registry: persistent `/dev` identity with exclusive take lease
- [x] Return a block-device lease when its filesystem is unmounted ([#294](https://github.com/p14c31355/fullerene/issues/294))

### P1-5. FS capability contract
- [x] `FileSystem` trait: uses typed `Result<..., FsError>` instead of `Option` + string errors
- [x] `FileSystemCapabilities`: read-only, mkdir, unlink, symlink, large-file ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] Stub operations return `NotSupported` (not silent success) — verified in dispatch
- [x] Offset/size/LBA unified to `u64` with checked conversion ([#294](https://github.com/p14c31355/fullerene/issues/294))

### P1-6. Timer & trace semantics
- [x] Repeating timer: `FixedRate` vs `FixedDelay` distinction
- [x] Interval 0 rejected at registration
- [x] Catch-up limit and missed-tick policy
- [x] Binary heap for timer queue (replace sorted Vec)
- [x] Trace buffer: sequence-numbered snapshots

### P1-7. Stub syscall audit
- [x] Linux compat: `mount`, `umount2`, `truncate`, `ftruncate`, `fsync`, `fdatasync` return `ENOSYS` (correct error, not silent success)
- [x] Linux compat: `fchmod`, `fchmodat` changed from silent success to `ENOSYS`
- [x] Native syscall stubs: `protect_memory` and `subscribe_event` implemented with real logic; only `device_ioctl` remains `NotSupported` (needs device dispatch infrastructure)
- [x] Syscall support matrix as test data

### P1-8. Headless / fake device tests
- [x] `carrier`: pipeline parse, unknown command, stdin/stdout, command stop ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] `solvent`: input event → state transition, dirty rect, clock, terminal session ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] FAT/block cache: `FakeBlockDevice` tests (done as part of P0-4)
- [x] Syscall: fake process address space + 2-process resource isolation ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] `nitrogen`: register backend trait → state-machine test ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] `lattice`: deterministic scene snapshot / PPM hash ([#294](https://github.com/p14c31355/fullerene/issues/294))

### P1-9. REAL HARDWARE ISSUE
- [x] filesystem: Issue where opening mounted external storage containing two or more files causes a hang.

### P1-10. Extension support using external crates
- [x] mp4: use `shiguredo_mp4` ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] jpg ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] png
- [x] mp3 ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] wav
- [x] tar
- [x] tgz ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] zip ([#294](https://github.com/p14c31355/fullerene/issues/294))
- [x] md
- [x] gz ([#294](https://github.com/p14c31355/fullerene/issues/294))
---

## P2: Developer Experience & Performance

### P2-1. Reproducible toolchain & CI
- [x] `rust-toolchain.toml`: nightly date pinned to `2026-06-01`
- [x] Components include `rustfmt` + `clippy`
- [x] Host-target `#[panic_handler]` added — `cargo check -p fullerene-kernel` now works without `--target`
- [x] CI: add `cargo fmt --check`, host check, Clippy jobs
- [x] CI: separate UEFI build and driver/hardware test matrix

### P2-2. Workspace dependency unification
- [x] Root `[workspace.dependencies]` for shared crates
- [x] Eliminate direct version duplication (`spin`, `x86_64`, `volatile`); unavoidable third-party versions are reviewed in `dependency-duplicates.toml`
- [x] Dependency duplicate policy in CI to detect new versions
- [x] Package metadata via `[workspace.package]`

### P2-3. Memory usage
- [x] Measure: boot time, frame time, heap high-water mark, DMA usage
- [x] Solvent back buffer: use real resolution instead of fixed maximum resolution
- [x] Reduce double `format!` in shell, clock string clone, temp `Vec` in render
- [x] Slot allocation for fd/handle/process after per-ownership model (fd/handle reuse slots; process registry is bounded with monotonic PIDs)

### P2-4. Capability documentation
- [x] Support matrix: FS, Linux syscall, native syscall, driver, QEMU/real HW (`docs/SUPPORT_MATRIX.md`)
- [x] Matrix data in TOML → generate docs and validate in CI
- [x] README feature list links to support matrix

---

## Boot Experience (Post-P1)

- [x] Fullerene Logo Display / Boot Splash
- [x] Boot progress indicator
- [x] Panic fallback screen (coloured screen encoding boot stage)
- [x] Fade transition to desktop
- [x] Full color palette, wallpaper, system font

---

## Userspace / Applications (Post-P2)

- [x] ELF loader + userspace `spawn` syscall/SDK wrapper
- [x] Process model & userspace memory isolation
- [x] Settings app
- [x] Task monitor
- [x] File browser
- [x] Log viewer

---

## Stretch Goals

- [x] Network stack (iwlwifi)
- [x] Audio output
- [x] SMP topology and AP startup mechanism (MADT + INIT-SIPI-SIPI + online CPU registry)
- [x] Rust userspace SDK
- [x] Package manager with verified native/Linux ELF port manifests
- [x] Self-hosted build port contract (`cargo` + `rustc`)
- [x] Execute FREEDOOM through the reviewed Linux-ELF port contract
- [x] Self-hosted presentation through the native `fullerene-present` port
- [x] Browse the internet through the NetSurf Linux-ELF port
- [x] Select multiple installed desktop session packages (KDE Plasma, Xfce, etc.)
- [x] Self-hosted coding through the VSCodium Linux-ELF port
