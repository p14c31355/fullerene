# Fullerene TODO

This TODO is aligned with the project architecture (`docs/ARCHITECTURE.md`)
and the improvement roadmap (`docs/IMPROVEMENT_PROPOSALS_20260704.md`).

Priority convention:
- **P0** = memory safety / process isolation — do first
- **P1** = structural improvement (ownership, types, tests)
- **P2** = developer experience, performance

---

## P0: Memory Safety & Process Isolation

### P0-1. fd / handle per-process separation
- [x] `ProcessResources` struct with per-process fd + handle tables (`process.rs`)
- [x] Per-process `alloc_handle`, `with_handle_mut` in `handlers.rs`
- [x] Handle transfer with rollback (transactional move between processes)
- [x] Resource cleanup on process termination (unblock waiters, close handles)
- [ ] Handle generation counters (index + generation to detect stale handles)
- [ ] Handle permission bits (read/write/signal/duplicate/transfer)
- [ ] Syscall return of multiple handles via `#[repr(C)]` struct (fix pipe packing)

### P0-2. User memory access unification
- [x] `UserPtr<T>` — validated pointer with copy-in/out operations
- [x] `UserSlice` — validated byte range with `copy_from_user` / `copy_to_user`
- [x] Updated `copy_user_string` to use `UserSlice`
- [x] Updated Linux compat copy functions to use `UserSlice`
- [ ] Page-level validation (check present/user/writable per page via page table)
- [ ] Remove all `&'static` return values from user memory API
- [ ] Unify native and Linux ABI copy under one implementation

### P0-3. `&'static mut` and mutable global ownership
- [x] `with_frame_allocator` guard API (replaces raw `get_frame_allocator_mut`)
- [x] Frame allocator access made `unsafe` with doc comment
- [x] **SchedulerContext**: moved all scheduling state (process list, schedule index, tick counter, NMI recovery RSP/RIP) into a single `pub static SCHEDULER` with explicit lock hierarchy. Replaced old `ProcessManager` global and scattered `AtomicU64` statics in `scheduler.rs`.
- [x] **VDSO read-only**: removed async ring-buffer (slot state machine, `VdsoFuture`, `poll_all_vdso_rings`). VDSO page now only contains read-only metadata (`time_us`, `uptime_us`, `pid`), mapped without `WRITABLE` in user page tables.
- [ ] `FramebufferGuard` / `with_framebuffer` closure API
- [ ] Solvent cursor fast path: use `FramebufferGuard` instead of raw address
- [ ] Trace buffer: fix for multi-CPU safety or document single-core assumption
- [ ] Boot-only globals: convert to `Once` or immutable after init

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
- [ ] `FsError`, `DriverError`, `MemoryError` in respective crates
- [ ] `From` impls to convert to `SyscallError` / Linux errno
- [ ] Remove `Result<..., &'static str>` usage (~200+ sites across kernel/nitrogen/genome)

### P1-2. Syscall ABI crate (`fullerene-abi`)
- [ ] Extract syscall numbers, error codes, `#[repr(C)]` DTOs into leaf crate
- [ ] Both kernel and SDK depend on the shared crate
- [ ] `AbiVersion` / capability query syscall

### P1-3. Module splitting by context boundary
- [ ] Split `drivers/fat.rs` → `block_device`, `partition`, `cache`, `fat32`, `exfat`
- [ ] Split `syscall/handlers.rs` → `dispatch`, `process`, `fs`, `memory`, etc.
- [ ] Split `solvent/lib.rs` → `runtime_context`, `input_loop`, `event_loop`, etc.
- [ ] Split `usb/xhci/context.rs` → controller init, command, event, transfer, resources
- [ ] Split `iwlwifi.rs` → device, firmware, registers, tx, rx, connection_state

### P1-4. Context ownership for callbacks & hooks
- [ ] Move `solvent::SOLVENT_CALLBACKS`, `RUNTIME`, `EVENT_QUEUE`, `DISPATCHER` into `RuntimeContext`
- [ ] Move `nozzle::FS_HOOKS`, `SYS_HOOKS` into constructor-injected services
- [ ] Move `carrier::SHARED_HISTORY` into terminal session
- [ ] Device registry: `DeviceManagerContext` with take/return lease API

### P1-5. FS capability contract
- [ ] `FileSystem` trait: typed `Result` instead of `Option` + string errors
- [ ] `FileSystemCapabilities`: read-only, mkdir, unlink, symlink, large-file
- [ ] Stub operations return `NotSupported` (not silent success)
- [ ] Offset/size/LBA unified to `u64` with checked conversion

### P1-6. Timer & trace semantics
- [ ] Repeating timer: `FixedRate` vs `FixedDelay` distinction
- [ ] Interval 0 rejected at registration
- [ ] Catch-up limit and missed-tick policy
- [ ] Binary heap for timer queue (replace sorted Vec)
- [ ] Trace buffer: sequence-numbered snapshots

### P1-7. Stub syscall audit
- [ ] Linux compat: `mount`, `umount2`, `truncate`, `fsync` return `ENOSYS`
- [ ] Native syscall stubs: return `NotSupported` instead of success
- [ ] Syscall support matrix as test data

### P1-8. Headless / fake device tests
- [ ] `carrier`: pipeline parse, unknown command, stdin/stdout, command stop
- [ ] `solvent`: input event → state transition, dirty rect, clock, terminal session
- [ ] FAT/block cache: `FakeBlockDevice` tests (done as part of P0-4)
- [ ] Syscall: fake process address space + 2-process resource isolation
- [ ] `nitrogen`: register backend trait → state-machine test
- [ ] `lattice`: deterministic scene snapshot / PPM hash

---

## P2: Developer Experience & Performance

### P2-1. Reproducible toolchain & CI
- [x] `rust-toolchain.toml`: nightly date pinned to `2026-06-01`
- [x] Components include `rustfmt` + `clippy`
- [ ] CI: add `cargo fmt --check`, host test, Clippy jobs
- [ ] CI: add headless QEMU smoke test with `isa-debug-exit`
- [ ] CI: separate UEFI build and driver/hardware test matrix

### P2-2. Workspace dependency unification
- [ ] Root `[workspace.dependencies]` for shared crates
- [ ] Eliminate version duplication (`spin` 0.10/0.12, `x86_64` 0.14/0.15, `volatile` 0.4/0.6)
- [ ] `cargo tree -d` in CI to detect new duplicates
- [ ] Package metadata via `[workspace.package]`

### P2-3. Memory usage
- [ ] Measure: boot time, frame time, heap high-water mark, DMA usage
- [ ] Solvent back buffer: use real resolution instead of fixed 3840×2160
- [ ] Reduce double `format!` in shell, clock string clone, temp `Vec` in render
- [ ] Slot map for fd/handle/process after per-ownership model

### P2-4. Capability documentation
- [ ] Support matrix: FS, Linux syscall, native syscall, driver, QEMU/real HW
- [ ] Matrix data in Rust/TOML → generate docs and tests
- [ ] README feature list links to support matrix

---

## Boot Experience (Post-P1)

- [ ] Fullerene Logo Display / Boot Splash
- [ ] Boot progress indicator
- [ ] Panic fallback screen
- [ ] Fade transition to desktop
- [ ] Full color palette, wallpaper, system font

---

## Userspace / Applications (Post-P2)

- [ ] ELF loader
- [ ] Process model & userspace memory isolation
- [ ] Settings app
- [ ] Task monitor
- [ ] File browser
- [ ] Log viewer

---

## Stretch Goals

- [ ] Network stack
- [ ] Audio output
- [ ] SMP
- [ ] Rust userspace SDK
- [ ] Package manager
- [ ] Self-hosted build