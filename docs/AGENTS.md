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
Fullerene Kernel
    ↓
Nitrogen (drivers)
    ↓
Solvent (runtime/orchestration)
    ↓
Resonance / ChronoLine
    ↓
Lattice / Nozzle
```

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


