# Fullerene Project Rules

## Current implementation snapshot (2026-08-16)

The repository currently implements the context-oriented architecture
described below across the root workspace members. The main runtime path is:

```text
UEFI/BIOS boot
    → Fullerene Kernel (hardware ownership, scheduler, VFS, framebuffer guard)
    → Solvent (event/frame pacing and service orchestration)
    → Resonance / Nozzle / Lattice / ChronoLine
    → Carrier / Genome / Nitrogen mechanisms through explicit callbacks
```

The kernel's `KernelContext` aggregates boot, memory, PCI, framebuffer, input,
window, audio, event, VFS, shell, GUI, and settings state. The scheduler is a
separate `SCHEDULER` lock because interrupts, syscalls, and the scheduler loop
must access it independently. Solvent similarly uses `RUNTIME_CONTEXT` for
runtime state and keeps the callback, event, and dispatcher domains separate.

Rendering is headless-capable. `Lattice::Scene` is an immutable snapshot;
`Compositor` renders either the full target or each clipped dirty region into a
persistent RAM back buffer. Solvent owns the copy to the hardware scanout and
has a cursor-only fast path. This is an intentional implementation of the
dirty-region and deterministic-rendering rules in sections 7 and 8.

The repository also contains the nested `toluene/cargo` third-party source
tree. It is an application port input, not a Fullerene architecture layer and
is excluded from workspace-wide source refactors.

WASM applications cross the kernel boundary through `solvent/wasi`. The WASI
runtime owns execution state, fuel limits, file-descriptor caching, and the
WASI/custom import linker; the kernel supplies one grouped `WasiHost` callback
set. The viewer and Emulsion applications are built as separate nested
workspaces and copied into the kernel build output. Viewer MP4 access is
seek-based and Emulsion screen capture is chunked, so neither path requires a
full media or framebuffer-sized temporary buffer in the host runtime.

The workspace currently contains 21 Cargo members. The latest host validation
passes `cargo check --workspace --all-targets`; the optional BusyBox build
status is intentionally silent when its cache/toolchain is unavailable, so an
optional port does not turn a warning-free Rust check into a warning. The
vendored BusyBox and VSCodium sources remain outside the architecture audit.

Mouse button ownership is explicit at the Solvent boundary: left-button
events enter Desktop/WM activation and drag handling, while right-button
events are routed to the Explorer or desktop context menu without invoking the
left-button drag path. Popup coordinates are clamped to the negotiated
framebuffer rather than a fixed 1024×768 assumption.

The iwlwifi lifecycle is retryable: a failed firmware/PCIe initialization is
cleaned up and returned to `Idle` when the network menu is opened again. A
successful init still starts scan offload only after the device reports the
firmware-ready state, and late RX beacons remain accepted through the scan
completion grace window.

Audio initialization is also retryable until HDA actually becomes ready. PCM
startup playback now requires DMA/LPIB progress to be observed before reporting
success; a completion log alone is not treated as proof that the controller
consumed audio data. Acoustic output still needs a speaker-equipped hardware
run, because headless QEMU cannot validate the analog path.

The release profile keeps aggressive optimization, single-unit LTO, and abort
panics, and strips the symbol table from shipped binaries. Debug builds retain
symbols for diagnosis; this changes artifact metadata, not runtime code paths.

The current audit keeps window and process ownership on their existing side of
the boundary. When a process-terminal window is closed, Lattice reports its
window identity, Solvent removes the terminal endpoint, and the kernel queues
termination of the owning process for scheduler context after the runtime lock
is released. This makes launchd's shell slot reusable without allowing a GUI
callback to re-enter the runtime lock.

For legacy iwlwifi, the authentication exchange now restores the dynamic q5
SCD enable bit at the final DQA doorbell; the affected 7265D firmware otherwise
advances the host write pointer without fetching the management TFD. It also
records firmware TX results for management frames, advances the bounded queue
plan after an explicit failure, and no longer discards association-frame
transmission errors. The DQA management queue follows Linux's gen1 scheduler
frame limit of 64; Linux's separate 16-entry management allocation is a host
software queue capacity, not the SCD command window. Host replay/unit tests
cover the state machine; the AP authentication result on the affected physical
adapter remains the final hardware validation step.

A 2026-07-30 redundancy and correctness audit reduced logic LOC without
changing runtime contracts: repetitive command-serialisation and font/colour
tables became table-driven `const` arrays and a shared `as_bytes` helper
(localised to `nitrogen::iwlwifi`); the kernel memory manager's 37 repeated
initialisation guards collapsed to a `check_init()?` helper. Correctness fixes
from the same pass are recorded in `docs/BUG_JOURNAL.md` (Entry 009), notably
the IPv4 header-checksum byte-order bug, a 2-px framebuffer-scroll stale band,
and unchecked boot-path arithmetic. The `assembly.rs` hand-coded boot
transitions remain `asm!` because no safe Rust equivalent exists for the
CR3/GDT/stack handoff; they are already encapsulated behind safe Rust entry
points per section 6.

The 2026-08-16 pass also uses derived defaults, slice filling, iterator search,
range predicates, and saturating arithmetic where those forms express the same
logic directly. The workspace gate is warning-free under
`cargo check --workspace --all-targets`; the retained benchmark example
`fullerene-kernel/examples/native_ipc_rate.rs` continues to exercise the native
IPC copy path. The boot assembly remains intentionally unchanged: its CR3/GDT
and stack handoff cannot be represented by safe Rust and is already isolated in
Petroleum's low-level entry boundary.

## 1. Overall Philosophy (Highest Priority)

- **Fullerene aims to be a safe, readable, maintainable, loosely-coupled no_std operating system.**
- The project must prioritize long-term architectural clarity over short-term convenience.
- The OS exists in an evolving world of changing hardware, firmware, runtime models, and subsystem requirements.
- Therefore:

```text
Prefer loose coupling over time.
```

- Minimize unsafe and asm usage.
- Maximize use of Rust core/alloc ecosystems.
- Prefer explicit ownership and lifecycle management.
- Code should always remain understandable to future maintainers.

---

# 2. Workspace Architecture Philosophy

The Fullerene workspace is no longer a single monolithic kernel.

Each crate represents:

```text
an architectural subsystem boundary
```

not merely a compilation unit.

Architectural clarity is more important than minimizing LOC.

Similar code is not necessarily shared code.

Duplication is acceptable when:
- ownership differs
- lifecycle differs
- execution phase differs
- synchronization domain differs

---

# 3. Global Dependency Direction Rules

The workspace should roughly follow this dependency direction:

```text
Fullerene Kernel  ──── Genome (VFS / filesystem)
    │   └── scheduler_context (SCHEDULER singleton)
    │       └── process management, VDSO metadata
    ↓
Nitrogen (drivers)
    ↓
Solvent (runtime/orchestration)
    ↓
Resonance / ChronoLine
    ↓
Carrier (I/O abstraction) ──── Lattice / Nozzle
```

Shared across kernel and userspace:

```text
fullerene-abi (no_std leaf crate, no dependencies)
    └── syscall numbers, error codes, repr(C) DTOs, ABI capabilities

petroleum (no_std support library)
    ├── page_table, memory, graphics
    ├── raw syscall instruction (numbers come from fullerene-abi)
    ├── VDSO layout (read-only metadata page)
    └── serial, early boot helpers

sealant (no_std memory capability boundary)
    └── checked RAM / MMIO / user / DMA / physical-address access

DriverKit (`ligand`, published as `DriverKit`)
    └── C ABI user-space IPC client for device handles, channels, and shared-buffer capabilities
```

New in this revision:
- **Fullerene ABI** is the dependency-free contract shared directly by the kernel and Toluene SDK. Petroleum re-exports its typed syscall numbers for compatibility but does not own them.
- **Genome** provides the filesystem framework (`FileSystem` trait, `Vfs` dispatcher, `MemFileSystem`) as a standalone leaf crate. The kernel re-exports Genome types and adds the singleton `VfsContext`.
- **Carrier** provides the I/O abstraction (`Terminal` trait, pipeline, streaming `dispatch()`) as another leaf crate. Nozzle and Solvent depend on Carrier for terminal I/O and command dispatch.
- **Sealant** keeps checked memory capabilities at the raw-memory boundary. A
  range check does not make an arbitrary mapping safe; mapping, initialization,
  provenance, lifetime, and concurrency remain the responsibility of the
  owning subsystem.

Lower layers must never depend on higher-level policy layers.

Examples:

- Nitrogen must not depend on Lattice.
- Nitrogen must not depend on Nozzle.
- Resonance must not depend on GUI concepts.
- ChronoLine must not own scheduler policy.
- Kernel must not directly own desktop logic.

Avoid dependency inversion caused by convenience.

---

# 4. Crate Responsibilities

## Fullerene Kernel

The kernel owns:
- memory management
- interrupts
- scheduler primitives
- low-level runtime initialization
- hardware resource ownership
- architecture bootstrap

The kernel should NOT own:
- GUI logic
- shell logic
- compositor policy
- event routing policy
- desktop state

Kernel code should remain thin.

### Scheduler Context

All scheduling state — process list, schedule index, tick counter, NMI
recovery target — lives in a single `SchedulerContext` struct behind a
`pub static SCHEDULER` singleton (`fullerene-kernel/src/scheduler_context.rs`).

```text
SCHEDULER (Mutex<process list>)
    ↑ independent
solvent runtime (internal state)
    ↑ independent
KERNEL (Mutex<KernelContext>)   — GUI, VFS, shell
```

The three locks are **never held simultaneously**.  The scheduler loop:

1. locks `SCHEDULER` briefly to publish VDSO metadata (atomic stores),
2. calls `solvent::tick_core()` (no `SCHEDULER` or `KERNEL` lock held),
3. locks `KERNEL` only inside `gui::runtime_tick()` for framebuffer render.

Process lifecycle functions (`create_process`, `terminate_process`) access
`SCHEDULER` directly.  The old `ProcessManager` global has been removed;
all existing call-sites now route through `SCHEDULER.with_process()`,
`SCHEDULER.schedule_next()`, etc.  Convenience wrappers (`block_current`,
`context_switch`) in `process.rs` are thin delegates to `SCHEDULER`.

### Process Birth, Supervision, and launchd

The scheduler reserves PID 0 for the kernel idle process. The first ordinary
process is loaded as PID 1 from the bundled static native ELF and is marked
`Init`; this is the launchd boundary. launchd is therefore started through
the same user ELF loader and syscall ABI as every other native program.

The shell is also a static native ELF. It is not a scheduler callback or a
boot-time kernel entry point: launchd creates a terminal endpoint and spawns
the shell. The kernel grants the `run_nozzle` ABI bridge only to a child
spawned by the kernel-marked launchd process; mutable process names and
terminal IDs cannot authorize it. The ELF is a small ABI bridge into the
existing Nozzle runtime, which still owns the VFS/desktop callbacks; Nozzle
consequently retains its #340 welcome text, prompt, Help list, completion, and
built-ins while the process remains launchd-owned.

Process creation records two independent relationships:

- `parent_id` identifies the process that created the child and is used for
  ordinary birth/wait semantics.
- `supervisor_id` identifies the process responsible for administration.

The parent or supervisor can obtain a `ProcessControl` capability. It can
observe state, stop, reap, or reassign supervision without becoming the
child's birth parent. The capability can be transferred through the existing
handle mechanism, so a future service manager can create a process while
launchd (or another admin process) owns its lifecycle. If a parent exits, the
kernel adopts the child under launchd while preserving the supervisor
relationship.

The bundled launchd is itself Rust-only userland. Its service table contains
image, terminal, and restart policy; the interactive shell is an on-demand
job rather than a boot service. The existing desktop/AppGrid terminal action
sets a kernel request flag, and only PID 1 can consume that request through
the native ABI. launchd then creates the terminal, spawns the shell, and
supervises it through its `ProcessControl` capability. It polls, reaps, and
revokes terminated children, and restarts `Always` jobs with bounded
exponential backoff. A service that is not configured for restart is left
stopped. Failure while bootstrapping a required service is fatal to PID 1,
so launchd never continues with an unmanaged child. Terminal creation is
provisional: if the subsequent spawn fails, the kernel closes the endpoint and
removes its temporary owner; on success ownership moves to the child.

This keeps launchd special only at the PID 1 bootstrap boundary. Shells,
services, and applications are otherwise ordinary user processes; adding a
new managed service is a userland service-table change rather than a kernel
special case.

### VDSO (Read-Only Metadata Page)

The VDSO page (`VdsoPage`) at `0x7000_0000_0000` contains **only**
read-only metadata:

```text
Offset │ Contents
───────┼────────────────────────────────────────
   0   │ time_us   (AtomicU64 — wall clock µs)
   8   │ uptime_us (AtomicU64 — monotonic µs)
  16   │ pid       (u64)
```

- Kernel writes via its phys_offset mapping (`Ordering::Release`).
- Userspace reads atomically with no ring transition (zero-copy for
  `Uptime`, `GetPid`, `ClockGetTime`).
- The page is mapped **without `WRITABLE`** in the user's page table.
  The old ring-buffer / slot-machinery (`VdsoFuture`, `poll_all_vdso_rings`)
  has been removed — all non-trivial syscalls go through the `syscall`
  instruction and trap to Ring-0.

#### Kernel File API Fixes (2026-07-04)

- `write_file` now advances the wrapper-side `FileDesc.offset` on success, matching the symmetric behavior already present in the read path.
- `exists` no longer temporarily opens and closes a file; it delegates directly to the VFS existence-check API, avoiding fd consumption and unnecessary state transitions.

#### UEFI Boot Code Cleanup (2026-07-04)

- Removed unnecessary `.clone()` calls on memory-map references.
- `MEMORY_MAP` lock scope narrowed with explicit blocks.
- Function addresses are cast through `*const ()` instead of direct integer-to-pointer conversions.
- Test configuration no longer enables the unused `alloc_error_handler` feature.
- Removed unused imports in syscall test modules.

#### Cargo Manifest Fixes (2026-07-04)

- `toluene/Cargo.toml` — removed stale `tests/unit_tests.rs` reference that pointed to a nonexistent file.
- `bonder/Cargo.toml` — removed `[profile.release]` override (already inherited from workspace root).

Preferred direction:

```text
kernel = primitive foundation
```

not:

```text
kernel = entire operating system state
```

---

## Nitrogen (Drivers)

Nitrogen is the hardware mechanism layer.

Nitrogen owns:
- MMIO
- DMA
- IRQ interaction
- hardware initialization
- device state machines
- framebuffer/device access

Nitrogen does NOT own:
- GUI policy
- shell policy
- compositor logic
- event propagation policy
- desktop logic

Unsafe code should be localized primarily inside Nitrogen.

Preferred philosophy:

```text
drivers expose mechanisms
higher layers decide policy
```

Nitrogen should prefer safe abstractions over leaking raw hardware interfaces upward.

The kernel owns PCI resource and page-table policy. Driver MMIO requests use a
kernel capability that validates and preserves an existing physical direct map
before creating a new mapping; drivers must not split boot huge pages or assume
that changing page-table cache flags is harmless on firmware-defined PCI
apertures. Likewise, firmware-assigned non-zero BARs are immutable inputs to
boot resource allocation and are never size-probed destructively.

The xHCI driver keeps `XhciContext` as its single state owner and API facade,
while lifecycle operations are split under `nitrogen/src/usb/xhci/`:
`controller` owns construction, reset/start, recovery, and root-port polling;
`command` owns command-ring submission and slot/endpoint configuration;
`event` owns event-ring waiting; `transfer` owns control and bulk transfers;
and `resources` owns slot release, deferred DMA cleanup, and teardown. These
modules add inherent methods to the same context and do not introduce
additional controller owners or global state.

The Intel Wi-Fi driver follows the same facade pattern. `IwlWifiDevice` remains
the single owner of MMIO, DMA rings, firmware state, and connection data, while
the implementation under `nitrogen/src/iwlwifi/` is separated by lifecycle:
`device` owns construction and resource lifetime; `firmware` owns image
selection; `registers` owns PCI/MMIO and firmware constants; `tx` owns host
commands and transmit descriptors; `rx` owns interrupt and receive processing;
and `connection_state` owns scanning, association, DHCP/WPA transitions, and
the incremental initialization facade. The split does not introduce another
device owner or change the public `nitrogen::iwlwifi` API.

---

## Solvent (Runtime)

Solvent is the orchestration/runtime layer.

Solvent owns:
- runtime coordination
- subsystem bootstrap
- event loop orchestration
- service ownership
- subsystem wiring
- frame/update pacing
- device-service lifecycle scheduling and projection of driver snapshots into UI state

Solvent should NOT become:
- a GUI framework
- a driver layer
- a scheduler implementation
- a global state dumping ground

Solvent primarily answers:

```text
who runs what
who owns what
who talks to what
```

Wi-Fi follows this boundary explicitly: Nitrogen owns the Intel device and
incremental initialization state machine, while Solvent owns `WifiService`, its
timeout, scan cadence, action consumption, and immutable desktop snapshot. The
kernel installs the `DriverContext` capability, starts Solvent via `solvent::init()`,
and explicitly registers the Wi-Fi service via `solvent::register_wifi_service()`.

The Wi-Fi PCI probe reads firmware-assigned BAR0 state without writing the
destructive all-ones size-probe pattern. It maps the first two register pages,
reuses the upstream bridge found during the original scan, and treats a stale
PCI configuration lock as an unavailable probe rather than spinning forever.

Solvent's crate root is an API facade rather than an orchestration
implementation. Runtime responsibilities are divided under `solvent/src/`:

- `runtime_context` owns callback, runtime-state, event-queue, and dispatcher
  synchronization domains together with runtime configuration and
  initialization.
- `input_loop` translates PS/2 mouse and keyboard state into desktop actions or
  Resonance events.
- `event_loop` owns timer processing, service ticks, event dispatch, and frame
  pacing.
- `window_api` owns window lifecycle, redraw control, and file-launch
  integration.
- `callbacks` defines the kernel-provided service contract and transfer types.
- `services` owns runtime-managed services and their shared UI snapshots.

`RUNTIME_CONTEXT` is the single owner of callbacks, mutable desktop runtime
state, the Resonance event queue, and the dispatcher. Each remains behind a
separate lock because dispatch handlers may re-enter runtime operations.
Callers acquire these domains through `RuntimeContext` guard methods; the
former standalone `SOLVENT_CALLBACKS`, `RUNTIME`, `EVENT_QUEUE`, and
`DISPATCHER` globals no longer exist.

---

## Resonance (Events)

Resonance is the immutable event propagation layer.

Resonance owns:
- event definitions
- event queues
- dispatch/routing
- propagation flow

Resonance should prefer:
- immutable events
- replayable event streams
- deterministic behavior
- explicit ownership

Resonance must NOT become:
- a GUI framework
- a scheduler
- a rendering system
- a global mutable state container

Prefer replayable deterministic event flows.

---

## ChronoLine (Timers)

ChronoLine is the time management subsystem.

ChronoLine owns:
- clocks
- timer queues
- deadlines
- timeout tracking
- repeating timer primitives

ChronoLine should NOT own:
- task scheduling policy
- async runtimes
- rendering policy
- GUI logic

Preferred philosophy:

```text
ChronoLine manages time primitives.
Other systems decide what time means.
```

---

## Lattice (Window Manager / Compositor)

Lattice owns:
- desktop state
- scene management
- compositor logic
- focus management
- redraw invalidation
- window management
- cursor composition

Lattice should NOT own:
- raw hardware access
- timer hardware
- shell parsing
- filesystem logic

Preferred rendering style:
- explicit rendering passes
- immutable scene snapshots
- headless renderability
- deterministic composition

Prefer:
- dirty rect rendering
- replayable GUI tests
- snapshot testing

---

## Carrier (I/O Abstraction)

Carrier is the I/O abstraction layer that decouples data transport from data processing.

Carrier owns:
- `Terminal` trait — abstract I/O interface for shell/console interaction
- pipe mechanism — `arm_pipe_stdout` / `take_stdout` for shell pipeline chaining
- command dispatch — `dispatch()` with streaming support (last pipeline stage writes directly to terminal, avoiding intermediate buffering)
- `Command` / `CommandContext` — trait and context for shell command execution
- pipeline parsing — `Pipeline` / `ParsedCommand` for `|`-separated command chains

Carrier should NOT own:
- filesystem logic
- GUI rendering
- scheduler policy
- kernel memory management

Carrier focuses on one question:

```text
how data flows between producers and consumers
```

The streaming fix: `dispatch()` no longer buffers the last pipeline stage's output into a `String` only to flush it at the end. Instead, the last stage writes directly through to the terminal. This eliminates the O(n) memory spike for commands like `dmesg` that produce large output.

---

## Nozzle (Shell)

Nozzle is the interactive shell subsystem.

Nozzle owns:
- command parsing
- shell state
- prompt rendering
- line editing
- builtin command execution
- terminal interaction flow

Nozzle should NOT own:
- framebuffer rendering
- GUI composition
- device access
- scheduler policy

Prefer terminal abstraction over direct framebuffer coupling.

Preferred direction:

```text
Nozzle produces text interaction.
Terminal systems decide how it is rendered.
```

---

## Genome (File System)

Genome is the file system / VFS abstraction layer.

Genome owns:
- `FileSystem` trait — abstract interface for any filesystem implementation
- `MemFileSystem` — in-memory tmpfs backed by a B-tree of inodes
- `Vfs` dispatcher — mount-table routing (longest-prefix match) for path-based operations
- path normalization (`.`, `..`, symlink resolution)
- `InodeType`, `VNode`, `FileDescriptor` — core filesystem types
- `FsError` — typed error enum for filesystem operations

Genome should NOT own:
- kernel memory management
- device drivers (block devices, USB)
- GUI logic
- shell or runtime state

Genome focuses on one question:

```text
how persistent data is organised, stored, and retrieved
```

USB mass-storage enumeration is likewise two-phase. Nitrogen registers block
device candidates without invoking VFS callbacks. After the controller lock is
released, the kernel integration layer performs FAT probing and mounts through
Genome. This lock boundary must be preserved: recursively borrowing a
`USBContext` from a mount callback is prohibited.

The FAT-family integration is divided along I/O ownership boundaries under
`genome/src/fat/`: `partition` translates media-relative
LBAs, `cache` owns sector caching and eviction, `block_device` adapts the kernel
contract to filesystem I/O, `fat32` and `exfat` implement their VFS backends,
and the module root only probes and dispatches mounts. Existing callers enter
through `genome::fat::mount_device`.

USB controller service registration is boot-safe and does not activate BAR
MMIO. Solvent polling observes only an already-active controller and must never
activate the Nitrogen state machine from rendering or input dispatch. Explicit
`usb_rescan` is the activation request boundary; it queues the work and returns
immediately, while scheduler-owned device processing performs activation,
discovery, and `/dev` registration. Filesystem mount policy remains separate.

PCI storage follows the same lifecycle rule. Boot may discover an RTSX
controller and prepare its service, but card-register MMIO begins only at the
explicit `sd_rescan` boundary. AHCI and NVMe drivers must not be registered in
the boot attach pipeline until they expose real block-device ownership; AHCI
now satisfies that boundary by publishing identified ATA disks as
`/dev/sataNpN`, while NVMe remains initialization-only. A controller reset
performed by a placeholder wrapper is not service registration.

Kernel-to-driver operations use a bounded generic submission/completion ring
pair. The request owns its payload and identifies a typed device target; the
completion returns status, byte count, read data, and driver-specific sequence
state where needed (for example, the USB BOT tag). NVMe/AHCI initialization,
MMIO requests, and block reads/writes use this common SQ/CQ boundary. The
storage adapter does not put borrowed user or VFS buffers into the ring: it
moves data through an owned request buffer and copies read data back only after
the CQ entry is consumed. Hardware-specific queues remain below this layer:
AHCI command lists, xHCI transfer/event rings, EHCI queue heads, and VirtIO
virtqueues are not conflated with the kernel request ring.

This is a transport boundary, not a claim that every device is block I/O.
Audio playback and iwlwifi control work now use typed SQ/CQ pairs as well. The
Solvent Wi-Fi service and WASM audio callback only enqueue owned requests; the
kernel scheduler submits a bounded batch, advances DMA/firmware state without
spinning in the caller, and drains completions independently. Audio CQ entries
are currently reported to the kernel log, while Wi-Fi CQ entries update the
driver-owned state and record rejected requests. PS/2 input and framebuffer
updates remain outside this transport boundary. Wi-Fi data TX crosses an owned
Wi-Fi SQ/CQ, while RX is polled by the driver, placed in its driver-owned receive
queue, and consumed by `NetDevice::poll_frame`. The hardware TX/RX rings remain
internal to the driver; network RX must not be described as a separate `DataRx`
SQ/CQ or forced into a synchronous block-request shape.

The scheduler's device phase runs before the Solvent runtime tick. This gives
service code a non-blocking producer boundary and keeps SQ execution,
hardware progress, and CQ consumption out of GUI/input callbacks. Legacy
BlockDevice and device-ioctl calls retain a synchronous compatibility adapter
until their ABI can return request handles; those adapters use the same owned
request format and are serialized against concurrent callers.

The kernel device registry preserves `/dev/<name>` identity while transferring
exclusive block-device ownership to a mounted filesystem. An available entry
contains a device lease; a present entry without a lease means mounted or in
use. Controller re-enumeration must not invalidate an outstanding lease.

Native syscall handling is split by context under
`fullerene-kernel/src/syscall/`. `dispatch` is the only syscall-number router;
`abi`, `process`, `fs`, `memory`, `shared_buffer`, `event`, `thread`, `window`, `device`, `ipc`,
`cap`, and `time` own their domain handlers. `interface` owns the shared error
and user-copy contract, while `types` owns handle-backed kernel object types.
Domain modules must not perform secondary syscall-number dispatch.

`/dev` is a `DevFs` mount backed directly by that registry, not a tmpfs with
manually-created placeholder files. Consequently registration and removal are
visible immediately and `/dev/null` is supplied by DevFs itself. Seeing only
`/dev/null` before an explicit media rescan is expected. Media discovery remains
distinct from mounting: `sd_rescan` may retry an inserted SD card, while
`mount /dev/sd0 <path>` only acquires and mounts its existing lease.

The kernel crate re-exports Genome types and adds the singleton `VfsContext` (wrapping `Vfs` with `spin::Mutex` + handle table) through the kernel's `vfs` and `fs` modules, keeping the core logic framework-agnostic.

### VFS Refactoring (2026-07-04)

The following improvements address fd collision across mount points, mount numbering stability, safety, and MemFS validation.

#### File Descriptor Collision Across Mount Points

Each filesystem allocates local file descriptors starting from 0. When multiple filesystems are mounted, the root FS and a mounted FS can return identical local fd values. The previous handle table used only the raw fd as the lookup key, so read/write/seek operations could be misrouted to the wrong filesystem.

Resolution:
- VFS-wide unique public fd allocation.
- A mapping from public fd to (mount number, FS-local fd).
- `read`, `write`, `seek`, `close` translate to local fd before dispatching to the target FS.
- Used fd numbers are never reused, even when allocation wraps.
- Replacing a mount invalidates all handles belonging to the previous FS.

#### Stabilized Mount Table Numbering

Previously the mount vector was sorted by mount-point path length on every addition. This reordering could change mount numbers for already-open handles, silently redirecting I/O to a different filesystem.

Resolution:
- New mounts are appended; existing numbers never change.
- Replacing the same mount point updates the same entry in-place.
- Path dispatch selects the longest-matching mount (stable across mounts).
- Unmount uses exact match rather than parent-path search.

#### Removed Unsafe from Genome VFS

`find_fs` used raw pointer casts (`*const` / `*mut`) to return mutable references into a `Vec`. Refactored to use safe Rust borrowing by separating mount selection from mutable reference acquisition.

#### Strengthened Mount Target and MemFS Read Validation

- Only directories are accepted as mount targets (regular files were previously allowed).
- Reading a directory fd from MemFS now returns `NotAFile` instead of silently returning EOF.
- Duplicate child-inode lookup consolidated into existing `lookup_child`.
- Removed unused inode-number parameter from `Inode::new`.
- Implemented `Default` for `MemFileSystem`.

---

## Isobemak

Isobemak is the boot image engineering and packaging system.

Isobemak owns:
- ISO9660 image generation
- El Torito support
- hybrid GPT layouts
- FAT32 ESP generation
- UEFI boot image construction
- boot metadata layout

Isobemak should prioritize:
- standards correctness
- compatibility
- deterministic image generation
- explicit binary layout handling

Prefer correctness over cleverness.

---

## Flasks

Flasks is the development runtime/runner tool.

Flasks owns:
- build orchestration
- QEMU execution
- debug profiles
- test launch configuration
- development workflows

Flasks should support:
- rapid iteration
- compatibility testing
- multiple machine profiles
- reproducible debugging

---

# 5. Ownership and State Rules

Prefer:

```text
explicit ownership transfer
```

over:

```text
global singleton access
```

Avoid hidden initialization order.

Do not hide lifecycle dependencies behind:
- globals
- macros
- implicit side effects
- hidden static initialization

**Exception — SCHEDULER singleton**: The `pub static SCHEDULER` in
`scheduler_context.rs` is an intentional global because the scheduler
loop, interrupt handlers, and syscall dispatch all need lock-free
access to scheduling state from arbitrary context.  The critical
distinction is that `SCHEDULER` owns its own lock (independent of
`KERNEL`) and exposes a controlled method surface (`with_process`,
`schedule_next`, `block_current`, …).  No new globals should be added
without the same level of justification and encapsulation.

Prefer capability passing.

Subsystem state should be owned locally whenever possible.

---

# 6. Unsafe and Low-Level Code Policy

- Minimize unsafe usage.
- Minimize asm! usage.
- Prefer safe Rust whenever possible.
- Unsafe blocks must explain:
  - why unsafe is necessary
  - what guarantees make it safe

Unsafe code should be localized near hardware boundaries.

Preferred philosophy:

```text
unsafe should be isolated
safe APIs should propagate upward
```

---

# 7. Testing Philosophy

Always verify runtime behavior with:

```bash
cargo run -q -p flasks -- --vga std
```

QEMU testing remains important.

However, the project should increasingly prefer:
- headless subsystem tests
- replayable event tests
- deterministic rendering tests
- snapshot testing
- non-interactive GUI validation

Prefer architectures that allow:

```text
same input
→ same state
→ same frame output
```

The system should become progressively more simulation-friendly over time.

---

# 8. Rendering and Event Design Philosophy

Prefer immutable/event-driven architectures.

Recommended flow:

```text
hardware input
    ↓
Nitrogen
    ↓
Resonance events
    ↓
Lattice / Nozzle
    ↓
render output
```

Avoid tightly coupling:
- drivers and GUI
- rendering and input acquisition
- timers and scheduler policy

Prefer deterministic replayability.

---

# 9. Documentation Rules

- Important structures/functions require doc comments.
- Update docs/ whenever architecture changes.
- TODOs must be concrete and actionable.
- Architectural changes should document ownership implications.

---

# 10. Coding Style Rules

- Refactor repetitive operations into helpers/constants.
- Avoid repeating identical operations more than 3 times.
- Split files appropriately.
- Merge redundant files.
- Avoid giant god-modules.
- Avoid phase-boundary abstractions unless ownership/lifecycle are identical.
- Prefer readability over clever abstractions.

Long-term maintainability is more important than temporary elegance.

---

# 11. External Crates

- External crates are encouraged when they reduce complexity.
- Prefer crates that preserve:
  - explicit ownership
  - no_std compatibility
  - initialization clarity
  - architectural transparency

Do not add unnecessary bootloader/UEFI framework dependencies.

Use Isobemak for ISO generation.

---

# 12. Prohibited Actions

- Do not tightly couple subsystem layers.
- Do not leak GUI logic into low-level drivers.
- Do not introduce unnecessary global state.
- Do not hide ownership.
- Avoid unexplained unsafe.
- Avoid large magic constants.
- Avoid architecture-obscuring abstractions.
- Avoid dependency shortcuts that violate subsystem direction.
- Do not use grep due to task termination risk.

---

# 13. Long-Term Architectural Goal

Fullerene should evolve toward:

```text
small core primitives
+
loosely coupled subsystem crates
+
deterministic event-driven orchestration
+
safe hardware abstraction
```

The project should remain:
- understandable
- debuggable
- replayable
- testable
- evolvable over time

Architectural clarity is the highest long-term priority.

---

# 14. Context First Principle

When introducing a new subsystem, first design its Context structure.

Implementation details should be organized around the Context, not the other way around.

The Context is the source of truth.
Functions, drivers, and hardware interactions are merely operations performed on that Context.

---

# 15. Context-Driven Design

Fullerene adopts a Context-Driven Design philosophy.

Any complex subsystem, hardware state, protocol state, or execution environment should be represented as a dedicated Context structure.

Avoid exposing raw hardware details, scattered state variables, or low-level implementation details across the codebase.

Prefer:

* AssemblyContext
* GraphicsContext
* AudioContext
* VirtualMemoryContext
* ProcessContext

instead of:

* Global state
* Scattered register values
* Raw page table manipulation
* Direct hardware access from unrelated modules

The goal is to reduce cognitive load, improve maintainability, and provide a stable abstraction layer between hardware-specific implementation and higher-level system logic.

Rule of thumb:

> If multiple functions share the same conceptual state, create a Context structure and move the state into it.
