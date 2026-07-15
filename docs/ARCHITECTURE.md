# Fullerene Project Rules

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
petroleum (no_std library)
    ├── page_table, memory, graphics
    ├── syscall numbers + raw syscall instruction
    ├── VDSO layout (read-only metadata page)
    └── serial, early boot helpers
```

New in this revision:
- **Genome** provides the filesystem framework (`FileSystem` trait, `Vfs` dispatcher, `MemFileSystem`) as a standalone leaf crate. The kernel re-exports Genome types and adds the singleton `VfsContext`.
- **Carrier** provides the I/O abstraction (`Terminal` trait, pipeline, streaming `dispatch()`) as another leaf crate. Nozzle and Solvent depend on Carrier for terminal I/O and command dispatch.

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

USB controller service registration is boot-safe and does not activate BAR
MMIO. Solvent polling observes only an already-active controller and must never
activate the Nitrogen state machine from rendering or input dispatch. Explicit
`usb_rescan` is the activation boundary; device discovery and `/dev`
registration remain separate from filesystem mount policy.

The kernel device registry preserves `/dev/<name>` identity while transferring
exclusive block-device ownership to a mounted filesystem. An available entry
contains a device lease; a present entry without a lease means mounted or in
use. Controller re-enumeration must not invalidate an outstanding lease.

The kernel crate re-exports Genome types and adds the singleton `VfsContext` (wrapping `Vfs` with `spin::Mutex` + handle table) through the kernel's `vfs` and `fs` modules, keeping the core logic framework-agnostic.

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


