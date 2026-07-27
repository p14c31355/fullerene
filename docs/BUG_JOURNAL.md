# Software Bug Journal

This document records non-obvious software bugs encountered during
development, their root cause analysis, and the fix applied.

> Entries are derived from `docs/software.rs` (doc-test format kept for
> reference in the original source).

---

## Entry 001 — File-manager freezes on directory open (two-phase navigation)

### Symptoms

On real hardware (and later confirmed in QEMU), pressing Enter on a
directory in the file manager showed "Loading…" in the status bar,
then the entire system froze — cursor, keyboard, all output stopped.

The freeze occurred even for `/bootlog/`, a purely in-memory
MemFileSystem directory, ruling out block-device I/O hangs.

### Investigation

`git bisect` between `main` and `develop` (PR #298, 118 commits)
identified commit `89051ce3` (`fix(exfat): avoid root directory
read stalls`) as the first bad commit.

That commit changed the file-manager navigation from a **single-step**
to a **two-step** deferred pattern:

```ignore
// BEFORE (ca3b740a) — single step, I/O runs immediately:
fn service_explorer_navigation() {
    let path = take_navigation_request();   // consume pending
    let entries = vfs_readdir(&path);        // I/O now
    finish_navigation(path, entries);
}

// AFTER (89051ce3) — two steps, I/O deferred to next tick:
fn service_explorer_navigation() {
    match take_navigation_step() {
        Checkpoint(path) => { return; }      // ← returns without I/O
        Read(path) => { /* I/O here */ }
    }
}
```

The two-step design was intended to let the compositor render a
"Loading…" message **before** starting synchronous block-device I/O
that could stall. However, the extra tick introduced a window where
the navigation state machine could stall indefinitely:

1. `navigate_to()` sets `pending_navigation = Queued(path)`.
2. Tick N: `Checkpoint` transforms `Queued → Ready` and returns.
3. Render shows "Loading…".
4. Tick N+1: `Read` should consume `Ready` and call `vfs_readdir`.

If between steps 2 and 4 the keyboard repeat of the Enter key
triggered another `navigate_to()`, `pending_navigation` was reset
to `Queued`, and the `Read` phase was permanently starved.
The exact freeze mechanism on real hardware was not fully determined,
but reverting to single-step eliminated the issue.

### Fix

Reverted to single-step navigation while keeping all other
improvements (error-type hardening, `callback_snapshot()`, etc.):

```ignore
fn service_explorer_navigation() {
    let path = take_navigation_request();   // consume pending
    let entries = vfs_readdir(&path);        // I/O now
    finish_navigation(path, entries);
}
```

The "Loading…" status is set by `navigate_to()` and cleared by
`finish_navigation()`. For MemFileSystem directories the I/O is
sub-millisecond, so the message is invisible. For slow block
devices the message may appear briefly — this is acceptable for
now; a true async I/O layer with timeouts remains future work.

### Files changed

- `solvent/src/explorer.rs` — removed `PendingNavigation::Queued` /
  `Ready` enum and `NavigationStep::Checkpoint` / `Read` enum;
  restored `take_navigation_request()`, kept `activate_entry()`.
- `solvent/src/event_loop.rs` — `service_explorer_navigation()`
  uses `take_navigation_request()` instead of `take_navigation_step()`.

### Lessons

- **Defensive tick-boundary design is fragile.** Introducing a
  mandatory one-tick delay between setting a flag and acting on it
  creates a window where intervening events can reset the state.
- **Two-phase patterns need starvation protection.** If the
  `Checkpoint → Read` transition can be indefinitely postponed,
  the system must detect and recover (timeout, priority queue, …).
- **Always test deferred I/O paths on real hardware** even when the
  target filesystem is RAM-backed and "instant". Lock ordering and
  interrupt interactions can differ between QEMU and native hardware.

---

## Entry 002 — Disjoint redraws expanded into a large repaint

### Symptoms

Small updates such as cursor movement or two unrelated UI invalidations could
cause the compositor to repaint every pixel inside the bounding rectangle of
all dirty regions. A cursor moving across a maximized window therefore made
wallpaper generation, window composition, and text overlays operate on the
entire path between the old and new positions.

### Root cause and fix

`lattice::Compositor::render` merged every dirty rectangle before rendering.
The RAM back buffer is persistent, so the merge was unnecessary: each clipped
region can be reconstructed independently from the same immutable `Scene`.
The compositor now renders each region separately and Solvent continues to
copy only the queued regions to scanout. Menu text, network dialogs, and the
debug overlay are also skipped when their bounds do not intersect the active
region. The upper panel is not regenerated for frames that cannot touch it.

### Regression coverage

`lattice::tests::compositor_keeps_pixels_between_disjoint_dirty_regions`
ensures that pixels between two updates remain untouched. The host rendering
example remains available through `cargo run -p lattice --example render_ppm`.

### Lesson

Dirty-region systems should preserve region topology until the last possible
stage. A bounding box is useful for reporting, but not as the composition
worklist when the backing store already contains the unchanged pixels.

---

## Entry 003 — Window shadows and terminal-cell redraw coverage

### Symptoms

Incremental desktop updates could leave a thin shadow or decoration from a
window's previous position. Shell text updates could also retain pixels in
the line gap below a glyph or place the cursor above the terminal cell's
baseline.

### Root cause and fix

The window-manager dirty rectangle used a stale 20px title-bar height while
the compositor rendered a 28px title bar, and it did not cover the shadow
falloff. Dirty bounds now use the compositor's title-bar constant and include
a conservative shadow margin. Terminal cells are treated as 16px high: every
cell redraw clears the complete cell background and the cursor is drawn in
the final two rows.

### Regression coverage

The Lattice tests compare incremental and full composition after moving a
titled window, and verify terminal-cell gap/cursor redraw behavior.

---
## Entry 004 — Release UEFI jump entered KernelArgs

### Symptoms

The default Release Flasks launch stopped immediately after switching CR3
with `#UD (Invalid Opcode)`. The reported RIP was inside the physical
`InitAndJumpArgs`/`KernelArgs` allocation instead of the higher-half kernel
entry point.

### Root cause and fix

The final inline assembly used independently allocated generic registers for
the argument-pointer calculation and the jump target. Release register
allocation allowed the arithmetic scratch register to alias `entry_virt`,
so the jump target was overwritten with `arg1 + arg2`. The transition paths
now use explicit, non-overlapping registers and preserve the entry point
while rearranging arguments.

### Regression coverage

The Release UEFI image was rebuilt and launched with Flasks/QEMU. It reached
`efi_main_real_logic`, memory-management initialization, GUI initialization,
and `scheduler_loop` without the invalid-opcode exception.
