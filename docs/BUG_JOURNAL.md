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

---

## Entry 005 — iwlwifi stopped at `step: start pci_probe`

### Symptoms

On the affected real machine, Wi-Fi initialization could stop after entering
the deferred PCI probe phase. The path was especially risky because it ran
against a live firmware-configured PCIe endpoint while the rest of the system
was already servicing the desktop.

### Root cause found by code inspection

`WifiRegistry::probe()` called `PciDevice::get_bar_info(0)`. That API sizes a
BAR by temporarily writing `0xffffffff` to the configuration register. This is
not safe for every firmware-initialized endpoint and contradicted the PCI
allocator's rule to preserve non-zero BARs. The Wi-Fi path also performed a
second complete PCI bus scan to rediscover the upstream bridge, and PCI config
lock acquisition could spin forever if a transaction had been abandoned by
recovery.

### Fix

The Wi-Fi probe now reads the existing BAR only, maps a 4 KiB register window,
reuses the bridge found during the original scan, and bounds PCI config-lock
acquisition. The normal iwlwifi initialization path uses the same non-
destructive BAR policy.

### Validation status

Formatting, `cargo check -p nitrogen`, and dependent kernel/runtime checks
pass after this change; the Nitrogen test suite also passes (74 tests). The
current development host exposes only Intel Ethernet `8086:15b8`, not one of
the supported iwlwifi IDs. The affected target machine subsequently advanced
past PCI probe and reached `step: mmio_poll_mac`, confirming that the original
PCI-probe hang was resolved; the next MMIO power-up stage then exposed a
follow-up issue.

### Follow-up — MAC clock did not become ready

The initial MMIO sequence wrote only `MAC_ACCESS_REQ` after software reset.
Linux's iwlwifi CSR definition specifies that `MAC_CLOCK_READY` remains zero
after reset until the host sets `INIT_DONE`; the timeout fallback also wrote
the wrong bit (`1 << 1`). The sequence now sets the named
`MAC_ACCESS_REQ | INIT_DONE` bits both on entry and during recovery. The
incremental path additionally reports `mmio_mac_clock_wait`, so a subsequent
machine run distinguishes a completed CSR read with an unready clock from a
PCI master-abort/device-gone result. `PciHealth` is copied out of the global
initialization lock before MMIO reads, preventing a stalled transaction from
holding the state lock needed by watchdog recovery.

Formatting, workspace checks, and the Nitrogen test suite (74 tests) pass.
The target machine still needs to boot this follow-up build. Success is
`mmio_read_mac` (and then DMA allocation); a continued
`mmio_mac_clock_wait` indicates that the remaining issue is device power,
link, or an omitted 7265-specific APM sequence rather than PCI discovery.

### Follow-up — firmware alive timeout

The target then reached firmware upload, but the outer Solvent timeout
reported `force_init_failed(timeout)` while waiting for the alive signal. The
outer limit was only 600 scheduler ticks (about 1.35 seconds), shorter than
the driver's 5-second per-candidate alive wait. In addition, the parser was
loading `SEC_INIT`, `SEC_WOWLAN`, and calibration TLVs into the runtime image,
including the `0xffffcccc` section separator. The loader now selects only
`SEC_RT`, skips the separator, clears stale CSR/FH interrupts, uses the GP1
CLR mailbox for RF-kill/CMD_BLOCKED bits, and enables the ALIVE/RX interrupt
causes before polling. The outer timeout is now 12,000 ticks. Because 7265
and 7265D share PCI IDs, firmware selection now waits for the CSR HW_REV read
and chooses only the matching two-candidate family.

The firmware boot path now also updates the direct `FH_UCODE_LOAD_STATUS`
mailbox after every runtime section (`1`, `3`, …) and writes `0xffff` before
releasing CPU reset, matching the 7265 PCIe transport sequence. The MMIO
window is 8 KiB so that the 0x1af0 register is mapped, and a write barrier is
issued after each status update. The physical log now includes `FH_LOAD` in
the alive-timeout diagnostic.

The next physical run should show `fw: alive_ok` or, on failure,
`fw: alive_timeout` with CSR register values rather than being terminated by
the outer timeout first.

The latest physical log exposed an ordering bug: immediately after
`firmware upload complete, starting CPU`, the driver called `PciHealth::recover()`
and retrained upstream bridge `00:1c.2` while the NIC firmware was booting. That
recovery is now limited to before firmware upload; the post-upload retrain was
removed so the alive notification can arrive over the unchanged link.

If the alive poll itself encounters a stalled PCIe completion, the state
machine now checks link health through PCI config space first and does not hold
`WIFI_INIT_CTX` while performing the watchdog-protected MMIO read. This keeps
watchdog recovery and the bounded firmware-candidate transition from being
blocked by a permanently held initialization lock.

The 7265 firmware loader now uses the legacy FH service-DMA channel (channel 9)
for runtime sections and waits for each chunk's FH-TX completion before
advancing. The previous HBUS write loop only proved that the host issued writes;
it did not prove that the firmware image had reached device SRAM. The section
status mailbox is also written from the host-maintained mask, avoiding the
invalid `0xa5a5a5a0` readback value observed on the target.

The first real-hardware retest showed that the FH DMA submission itself timed
out on the first runtime chunk (`0x00800000`, 98,304 bytes), before CPU reset was
released and before an alive wait could begin. This rules out the firmware-alive
mailbox as the immediate failure point. The loader now enables the 7000-series
APMG DMA clock and L1-Active workaround before submitting the service-channel
transfer, and records CSR/FH/TCSR state plus the DMA address on timeout.

### Follow-up — GUI cursor disappears after shell launch

The GUI input path accepted out-of-framebuffer cursor coordinates. A malformed
or sign-wrapped PS/2 relative delta could therefore move the software cursor
outside the visible screen; opening the shell then made it appear to vanish
until a later movement. Mouse events are now clamped to the current
framebuffer bounds before desktop hit-testing and cursor redraw.

### Follow-up — cursor jumps when Wi-Fi is enabled

Wi-Fi firmware/MMIO work runs from the service tick and can delay the next
PS/2 poll. Relative packets that arrive during that interval were then
consumed as one large movement. The scheduler also polled input before calling
`tick_core`, which polled it again. Input polling is now owned by `tick_core`,
and each resumed poll caps stale accumulated motion to a bounded screen step.
The IRQ handlers also verify the PS/2 controller's AUX/keyboard status before
consuming `0x60`, and a poll gap over 50 ms discards the stale relative packet
backlog entirely.
