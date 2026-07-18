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
