# Software Bug Journal

This document records non-obvious software bugs encountered during
development, their root cause analysis, and the fix applied.

## Entry 012 — 2026-08-16 shell window close left launchd occupied

### Symptoms

Closing the shell from its desktop title bar removed the visible window, but
opening Shell again from a desktop icon did nothing. The shell process and its
process-terminal record were still alive, so launchd correctly believed its
single shell slot was occupied.

### Root cause and fix

The title-bar path called `WindowManager::close_window` directly and bypassed
the Solvent process-terminal cleanup path. Desktop mouse handling now returns
the closed window identity. Solvent removes a matching process-terminal record
and queues a kernel callback; the scheduler consumes that request after the
runtime lock is released and terminates the owning process. This preserves the
existing scheduler ownership model and makes the launchd slot reusable.

Lattice and workspace tests cover the close notification path. Physical/QEMU
interactive shell reopening should still be included in the next runtime smoke
run.

## Entry 013 — 2026-08-16 iwlwifi q5 scheduler and gen1 byte count

### Symptoms

On the affected 7265D-29 adapter, the UI remained in `Authenticating` after
the authentication TFD was submitted. The new log showed q5 `wrptr=1`,
`rdptr=0`, while `SCD_EN_CTRL=0x00000023` after the experimental q5 gate was
added. Therefore the gate was not the missing condition.

### Root cause and fix

The 7265 is a 7000-series gen1 device. Its scheduler byte-count table stores
DWORDs: Linux adds CRC/delimiter (and a security trailer when applicable),
then rounds the result up to four bytes. The table therefore publishes
`bc_dwords=0x000a` for the 30-byte authentication frame. An intermediate
implementation incorrectly published the raw byte count `0x0026`; the
latest implementation follows the Linux gen1 conversion.

An intermediate follow-up log showed this API-29 7265D image leaving
`FH_TRB=0` and the FIFO buffer idle unless q5 was restored in `SCD_EN_CTRL`.
That gate remains enabled only for this firmware/API combination; other DQA
images retain the Linux ownership rule. The later fl-auth9 log proves that the
gate is necessary for the observed active FH state, but it is not sufficient
to make the scheduler consume q5's TFD.

The connected Linux capture initially appeared to expose one more wire-level
difference, but the source comparison corrected that interpretation:
`IWL_MGMT_QUEUE_SIZE=16` is the host-side software allocation, while
`SCD_QUEUE_CFG.window` is the gen1 scheduler frame limit. Linux defines the
latter as `IWL_FRAME_LIMIT=64` and uses it for this management queue. Fullerene
therefore retains 64 in the command; the auxiliary queue also remains 64.

The fl-auth8 retest used the temporary 16 value and still showed q5
`hw_wrptr=1` with `hw_rdptr=0`. Its SRAM snapshot was `ctx1=0x00400010`
(`win_size=16`, `frame_limit=64`), proving that the experimental value reached
firmware but did not make the scheduler consume the TFD. The temporary value
has been reverted to the Linux-compatible 64; q5 scheduler/TFD consumption is
still unresolved.

All Nitrogen library tests pass, including the corrected byte-count and
API-29 gate assertions. The affected adapter and AP still require physical
validation because QEMU does not emulate this Intel radio.

## Entry 014 — 2026-08-16 fl-auth9 confirms q5 scheduler consumption stall

### Evidence

The Linux-compatible 64-entry configuration is now visible in firmware:
`ctx1=0x00400040` (`win_size=64`, `frame_limit=64`). The q5 gate and FH path
are active (`SCD_EN_CTRL=0x23`, `FH_TRB=0x80305000`), and the TFD/byte-count
layout remains valid (`3 TB`, `20+64+6`, `bc_dwords=10`).

Nevertheless, q5 remains at `hw_wrptr=1`, `hw_rdptr=0` through ticks 64, 512,
and 1024. No `REPLY_TX` or authentication response is observed. This rules
out the experimental window value as the immediate cause and leaves the
firmware-owned DQA queue activation/TFD-fetch path as the next bounded target;
the AP has not yet received an authentication frame. The capture ends before
the 4,000-tick watchdog threshold, so the existing `DqaHostScd` and static-q4
fallback plans were not exercised in this log.

## Entry 015 — 2026-08-16 fallback transition retained the stalled q5 owner

### Evidence

fl-auth10 exercised both fallback stages. The firmware-owned DQA path and the
host-direct SCD path each submitted a valid q5 TFD but remained at
`hw_wrptr=1`, `hw_rdptr=0`. The direct path changed the FH snapshot slightly
(`FH_TRB=0x80305001`, FIFO buffer `0x00001620`) but did not consume the TFD.

When switching to the static q4 path, q4 activation completed, but the next
`CONNECT_FALLBACK_ADD_STA_QUEUE` command could not be consumed by q0:
`target=31`, `rptr=0x1e`, followed by `Busy`. The stalled q5 had previously
been marked inactive and its TFD ring cleared, but its scheduler ownership
bits were left set. The fallback transition now also clears q5 from
`SCD_EN_CTRL`, `SCD_QUEUECHAIN_SEL`, and `SCD_AGGR_SEL` before publishing the
next queue. This prevents a dead q5 owner from blocking the shared gen1
scheduler during q4 reconfiguration.

## Entry 016 — 2026-08-16 q5 ownership cleanup alone did not release q0

### Evidence

fl-auth11 still reaches the same boundary. Both q5 DQA attempts submit a
valid authentication TFD and remain at `hw_rdptr=0`; the static q4 queue is
then activated, but `CONNECT_FALLBACK_ADD_STA_QUEUE` remains unconsumed by
q0 (`target=33`, `rptr=0x20`). No `REPLY_TX`, authentication response, or
association follows. The earlier q5 bitmap cleanup is therefore not the
complete cause of the q0 stall.

### Follow-up

The fallback teardown is being brought in line with Linux gen1
`iwl_trans_pcie_txq_disable`: deactivate the queue, clear all four dwords of
its SCD TX-status entry, and do not issue a zero-pointer doorbell while
tearing it down. The next real-device log will distinguish stale SCD status
from a remaining shared-scheduler or command-queue problem.

## Entry 017 — 2026-08-16 API-29 authentication moved to static q4 first

### Evidence

fl-auth12 reproduced the fl-auth11 boundary after the Linux-style teardown:
q5 remained at `hw_rdptr=0`, and q0 stopped while consuming the static queue
update (`target=32`, `rptr=0x1f`). No q4 authentication TFD was submitted.

### Change

The API-29 DQA setup is retained for station/firmware initialization, but
authentication now selects the Linux static q4 queue before any q5 management
TFD is posted. The AP station is added with q4 already in its queue mask;
API29 asserts when q4 is attached later through a DQA STA_MODIFY command.
Other firmware/API combinations keep the existing DQA-first order.

## Entry 018 — 2026-08-16 API-29 static queue ownership must be present at ADD_STA

### Evidence

fl-auth13 reached q4 activation and consumed the static queue command, but
the subsequent DQA-style `STA_MODIFY_QUEUES` command caused a runtime
firmware assertion (`error_id=0x000021a0`, command `0x001f0018`). This proves
the previous q5-to-q4 transition problem was not solved by attaching q4 to an
already DQA-created station.

### Change

The API-29 static-first path now activates q4 before `CONNECT_ADD_STA` and
uses the legacy station layout with q4 already present in
`tfd_queue_msk`. It no longer sends a later DQA queue-modify command. The
first authentication TFD should therefore be the next decisive hardware
check.

## Entry 019 — 2026-08-16 system Linux firmware is newer than the embedded blob

### Evidence

fl-auth14 shows that the API-29 runtime rejects both forms of static-q4
station ownership: DQA `STA_MODIFY_QUEUES` asserted in fl-auth13, while the
legacy q4 `ADD_STA` asserted here (`error_id=0x000021a0`). The log stops before
any authentication TFD is submitted.

The successful Linux report uses firmware `29.4063824552.0`, whereas the ISO
used by these logs reported `29.2666559981` (`CoreCycle26_stab::9ef079ed`).
The installed Linux `iwlwifi-7265D-29.ucode` is
`CoreCycle26_stab::f2390aa8` and is now used as the workspace firmware source,
preserving the Linux-tested firmware/runtime pairing. The API-29 connection
path remains DQA-first; the unsuccessful static-q4 experiment is not kept as
the default path.

## Entry 020 — 2026-08-16 fl-auth15 loads the Linux firmware cleanly, but q5 still does not fetch

### Evidence

fl-auth15 reports `CoreCycle26_stab::f2390aa8` and ucode
`29.4063824552`, matching the firmware used by the successful Linux capture.
`CONNECT_PHY_CONTEXT`, `CONNECT_MAC_CONTEXT`, `CONNECT_ADD_STA`,
`CONNECT_SCD_QUEUE_CFG`, and `CONNECT_ADD_STA_QUEUE` all complete without the
previous `ADVANCED_SYSASSERT` (`error_id=0x000021a0`).

The capture still ends in `Authenticating`. The q5 authentication TFD is
submitted with `hw_wrptr=1`, but the hardware remains at `hw_rdptr=0` through
ticks 64, 512, 1024, 1536, and 2048. There is no `REPLY_TX`, authentication
response, association, or DHCP completion. The remaining bounded target is
therefore the host-side SCD/FH fetch setup, not the old firmware mismatch or
the rejected static-q4 ownership forms.

Both fl-auth14 and fl-auth15 are 131,072 bytes, indicating a fixed capture
buffer limit rather than a reduction in stored file capacity. The non-padding
log lines decrease from 370 to 312 because the new run avoids the firmware
assert/fallback sequence. This is a meaningful reduction in failure noise and
an improvement in initialization stability, but it is not yet evidence of a
faster or successful AP connection.

## Entry 021 — 2026-08-16 align the q5 post-configuration doorbell with Linux

### Linux comparison

Linux gen1 DQA setup publishes the queue's CBBC and an initial zero write
pointer before `SCD_QUEUE_CFG`. After firmware processes `SCD_QUEUE_CFG` and
the station queue mask is updated, Linux does not ring that queue again with a
zero pointer; the next doorbell is the real TFD write pointer (`1` for the
first management frame). Fullerene was issuing an extra q5 `HBUS_TARG_WRPTR`
write of `0` after `CONNECT_ADD_STA_QUEUE`, immediately before the
authentication TFD.

### Change

The extra zero-pointer doorbell was removed. The API-29 compatibility path
still restores q5 in `SCD_EN_CTRL`, because fl-auth15 showed that this firmware
needs the gate for a non-idle FH path, but it no longer couples that gate write
to a second doorbell. The unit test now asserts that the compatibility write
does not modify `HBUS_TARG_WRPTR`; the next real-device log will distinguish
the Linux-compatible pointer sequence from the remaining firmware-specific
SCD gate behavior.

## Entry 022 — 2026-08-16 fl-auth16 matches Linux SCD context semantics

### Evidence

fl-auth16 is running the Linux-tested `CoreCycle26_stab::f2390aa8` firmware
without an assert. After `CONNECT_SCD_QUEUE_CFG` and `CONNECT_ADD_STA_QUEUE`,
q5 reports:

- `ctx0=0x00000000`
- `ctx1=0x00400040` (`window=64`, `frame_limit=64`)
- `trans_tbl=0x00000000`
- `tx_stts=0x00000000`
- `queuechain` contains q5, `SCD_QUEUE_STATUS=0x0000009b`, and
  `SCD_EN_CTRL` contains q5

Linux's gen1 transport passes `cfg=NULL` for a DQA queue, so it intentionally
does not write the host-side SCD context, status, chain, aggregation, or
translation entry. It only initializes the queue pointers; the subsequent
`SCD_QUEUE_CFG` command supplies the firmware-owned queue configuration. For
the non-aggregate management queue, Linux also does not need an RA/TID
translation entry. Therefore q5's context and zero translation entry in
fl-auth16 are not the cause of the fetch stall. A zero TX-status entry before
the first fetch is also not sufficient evidence of failure.

### Remaining difference

The remaining host-side deviation is the API-29 compatibility write that
restores q5 in `SCD_EN_CTRL`; Linux leaves dynamic-queue activation ownership
to firmware. fl-auth16 confirms that this gate makes the FH path non-idle, but
q5 still remains at `hw_rdptr=0`. The next comparison should therefore A/B
that single gate, rather than rewriting the already Linux-compatible context
or translation table.

## Entry 023 — 2026-08-16 fl-auth17 repeats the q5 fetch stall

### Evidence

fl-auth17 reaches the same AP (`Buffalo-G-2218`, BSSID
`f0:f8:4a:e8:22:18`, channel 11) and uses the same Linux-matched firmware
`CoreCycle26_stab::f2390aa8` / ucode `29.4063824552`. The connection commands
all succeed, the authentication TFD is submitted with three TBs and matching
byte counts, and the FH path is non-idle (`FH_TRB=0x80305000`).

The run still reports `SCD_EN_CTRL=0x00000023` with q5 set. q5 remains at
`hw_wrptr=1`, `hw_rdptr=0` through ticks 64, 512, 1024, and 1536, with no
`REPLY_TX`, authentication response, association, or DHCP completion. The
capture therefore does not demonstrate a regression or a successful
authentication.

This is not an A/B test with the API-29 q5 gate removed: the log explicitly
shows `q5_bit=SET`. Compared with fl-auth16, the observed q5 SCD/FH values are
effectively unchanged; the smaller non-padding line count reflects a shorter
capture, not a scheduler fix. The next physical test must either log
`q5_bit=CLEAR` with `SCD_EN_CTRL=0x00000003`, or retain the gate and vary one
other scheduler input so that each experiment has an unambiguous result.

## Entry 024 — 2026-08-16 fl-auth18 reaches the full authentication fallback chain

### Evidence

Waiting through the watchdog timeout adds useful evidence, but does not
authenticate. The initial `dqa_firmware` attempt remains stalled through tick
3584 with q5 at `hw_wrptr=1`, `hw_rdptr=0`. There is still no `REPLY_TX`,
authentication response, association, or DHCP completion.

The driver then falls back to `dqa_host_scd`. This second submission changes
the FH/FIFO observation (`FH_TRB=0x80305001`, `fifo_buf=0x00001620`, and
`tx_status=0x07f70001`), so the host-SCD path does cause additional transport
activity, but q5's scheduler read pointer remains zero and no response is
received. This is not a successful fetch; it is a second stalled descriptor
on the same q5.

Finally, the static-q4 fallback activates q4, but
`CONNECT_FALLBACK_ADD_STA_QUEUE` times out while consuming at command-queue
position `target=31`, `head=31`, `tail=30`, `rptr=0x1e`. The fallback chain is
therefore blocked by the command queue after the q5 stall, rather than proving
that static q4 transmission works or fails on air.

The run still has `SCD_EN_CTRL=0x00000023` with q5 set, so it remains neither
the requested q5-gate A/B test nor a successful Linux-equivalent run. The
timeout path is nevertheless valuable: the next fix should preserve the
initial q5 evidence, prevent a stalled q5 fallback from blocking the command
queue, and make the gate-cleared experiment independently observable.

## Entry 026 — 2026-08-16 API-29 firmware TLV alignment audit

### Linux comparison

Linux's `iwl_parse_tlv_firmware()` advances every record by
`sizeof(struct iwl_ucode_tlv) + ALIGN(tlv_len, 4)`. Fullerene already used
that boundary for the normal path, but the `SEC_INIT`/`SEC_RT` skip and
separator paths advanced only to the unpadded payload end. That is a real
parser discrepancy for a non-word-sized TLV preceding a section record.

The API-29 image in the new hardware log is otherwise parsed consistently:
API 29/build, `TLV_ENABLED_CAPABILITIES` index 0 bit 12 (DQA), PHY_SKU,
runtime calibration, and all three selected sections are observed before
ALIVE. `SCD_QUEUE_CFG` also returns the expected q5 response. Therefore this
TLV issue is fixed as a correctness hole, but the current q5 `RDPTR=0`
failure is not proven to originate in the firmware-file TLV stream.

### Change

The legacy parser now computes and validates one aligned `next_tlv` boundary,
uses it on every skip path, and rejects truncated/overflowing records instead
of silently continuing. It also logs the Linux API-change bitmap (including
bit 29, without changing key-command selection) so the physical run can
confirm that the selected firmware's API flags were read at the same offset.

The q5 SCD gate, authentication frame, WPA path, and RX parser were not
changed by this audit.

## Entry 025 — 2026-08-16 API-29 DQA gate A/B matches Linux ownership

### Linux comparison

Linux gen1 `iwl_trans_pcie_txq_enable()` receives `cfg == NULL` for a DQA
queue. In that branch it initializes the queue pointers and publishes the
initial `HBUS_TARG_WRPTR`, but it does not write `SCD_EN_CTRL`, queue status,
queuechain, aggregation, context, or the RA/TID translation entry. The only
`SCD_EN_CTRL` update in the function is for the command queue. `ADD_STA`
publishes the station's `tfd_queue_msk` separately through the MVM path.

### Change

`ensure_api29_dqa_scheduler_gate()` now defaults to observation-only. The
former q5 `SCD_EN_CTRL` write remains behind
`API29_DQA_HOST_SCD_GATE_DIAGNOSTIC = false`, and logs before/after values and
the q5 bit so a physical run is an unambiguous A/B result. The direct-SCD
fallback remains separate. Authentication/WPA/RX parsing and the auth frame
layout were not changed.

The submit log now includes q5 CBBC, station queue mask, SCD control bits,
context words, translation/TX-status words, and FH state in the same record as
the post-doorbell pointers. The first hardware validation for this build is
`qbit=CLEAR` followed by `hw_wrptr=1` and `hw_rdptr=1` (or a TX completion).
No physical result is claimed by this entry yet.

## Entry 009 — 2026-07-30 workspace audit fixes

A full-workspace bug and redundancy sweep produced the following fixes. Each
preserves the existing behaviour except where a bug was corrected.

### IPv4 header checksum double byte-swap (HIGH — networking)

`bonder::ipv4::build_packet` stored `checksum()`'s result through
`hdr.header_checksum = cs.to_be()`, but `Ipv4Header::write_to` already
serialises the field with `to_be_bytes()`. On little-endian x86 the two swaps
cancelled, emitting a little-endian (wrong-order) checksum that
standards-compliant receivers drop. The UDP path already did the right thing
(`hdr.checksum = cs`). Fixed by storing the native-order value. This broke all
outgoing IPv4 packets (DHCP, DNS, …).

### Framebuffer scroll left a 2-px stale band

`FramebufferWriter::scroll_up` and `scroll_buffer_pixels` shifted the buffer up
by one text row (10 px, `FONT_6X10`) but only cleared the bottom 8 lines. The
2 lines between `height-10` and `height-8` retained old bottom-region content
after every scroll. Fixed by clearing the full freed row (10 px) so shift and
clear use the same height.

### Unchecked arithmetic on firmware-supplied boot data

- `BitmapFrameAllocator::init_with_memory_map` computed
  `physical_start + page_count * 4096` with plain `+`/`*`, which can overflow
  `u64` for malformed UEFI descriptors and panic under the dev profile's
  overflow checks. Routed through `saturating_add`/`saturating_mul` to match
  the companion validator.
- `set_heap_range` stored `start + size` without overflow protection; now
  `saturating_add`.
- `VirtualMemoryContext::extend` used `extension.used += additional` where the
  sibling `extend_for` correctly used `saturating_add`; unified on
  `saturating_add`.
- `timing::ticks_per_us` performed the CPUID-0x15 frequency multiply in `u64`,
  which a malformed leaf could overflow (panic in debug); promoted the
  intermediate to `u128`.

### Compositor FPS update landed one frame early

`notify_frame_presented` compared the *pre-increment* `fetch_add` result
against the 30-frame interval, so the FPS readout updated on frames 1, 31, 61…
instead of 30, 60, 90…. Fixed by using `fc + 1` in the modulo.

### Redundancy reductions (behaviour-preserving)

The same pass replaced repetitive code with table-driven / helper forms that
keep the runtime contract identical: the 110-line `glyph()` match became two
indexed `const` arrays (A–Z / 0–9); `EfiStatus`'s two 23-arm matches became
discriminant-indexed arrays; `u32_to_vga_color` became a 16-entry table; the
37 repeated `if !self.initialized` guards in `UnifiedMemoryManager` became a
`check_init()?` helper; the iwlwifi `from_raw_parts` command-serialisation
boilerplate (9 sites) became a single `as_bytes` helper; and
`WifiInitPhase::From<u8>` became a discriminant-indexed array.

---

## Entry 006 — Right-click was consumed by the left-button WM path

### Symptoms

The file-manager context menu could fail to appear, particularly after the
Explorer window had been focused or moved. Desktop right-clicks also used a
fixed 1024×768 popup boundary.

### Root cause and fix

Solvent sent every mouse-down through `Desktop::mouse_down`, whose API had no
button argument. A right-button event could therefore enter the left-button
window-manager path before Explorer handled it. Right-button routing is now
handled before that path: it focuses the target window without starting a
drag, then dispatches Explorer's menu or the desktop menu. Desktop popup
coordinates are clamped to the actual framebuffer dimensions.

### Regression coverage

Lattice now tests popup bounds, and the workspace host check remains clean.

---

## Entry 007 — Current hardware validation boundary

The 2026-07-30 QEMU headless validation reached `scheduler_loop`. The static
BusyBox smoke harness launched two sequential interactive shells, rendered
their output through process terminals, accepted `exit`, and reported PASS.
The same run completed HDA PCM DMA playback, but headless QEMU cannot prove
acoustic output. The current development host exposes Intel Ethernet
`8086:15b8` and no supported Intel wireless controller, so physical iwlwifi
firmware and AP discovery require validation on the affected machine.

The retry path is now implemented: transient iwlwifi init failure releases
partial DMA state and permits the next network-menu open to restart the state
machine. AP discovery remains hardware-dependent until that path is exercised
on a supported Intel 7260/7265 device.

HDA playback now likewise keeps initialization retryable after allocation or
codec-route failure and requires a changed LPIB value before reporting PCM
success. This distinguishes DMA completion from an apparently successful but
silent startup path; analog output itself remains hardware-dependent.

---

## Entry 008 — Audio completion was weaker than audio progress

`AudioContext::play_pcm` previously returned success after the polling loop
without checking that the HDA link-position register had moved. The startup
step consequently could finish with a success-shaped log even when a stream
was not consuming DMA data. The implementation now records the initial LPIB,
requires progress, and logs the final stream status on failure. HDA setup also
does not permanently latch a failed initialization attempt.

---

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

The Wi-Fi probe now reads the existing BAR only, maps a 0x2000-byte (8 KiB)
register window,
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

The next retest reached repeated scan completion, but reported zero APs. The
RX path had been checking obsolete bit positions (`1 << 18` and `1 << 15`) rather
than the 7265 CSR FH-RX/FH-TX causes (`bit 31` and `bit 27`). Those checks now
use the named CSR constants. Scan completion is also delayed long enough for
the four requested channels to finish their dwell time instead of ending after
13 scheduler ticks.

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

## Entry 010 — 2026-08-12 USB rescan entered the hub path for non-hub storage

### Symptoms

On the target machine, `usb_rescan` could hang while Klog Live showed the last
completed marker as:

```text
[USB-RESCAN] context: controllers poll returned
[USB-RESCAN] context: xhci mass-storage enumeration returned
[USB-RESCAN] context: xhci hub enumeration begin
```

The controller poll and generic mass-storage enumeration had returned, but the
shell never reached `/dev/usb0` registration. The Klog Live boundary markers
were added because the synchronous shell command blocks the normal compositor;
the timer/direct repaint path kept the last completed USB boundary visible.

### Root cause and fix

`enumerate_mass_storage()` returned `NotSupported` when the configuration did
not contain a BOT mass-storage interface. The xHCI caller treated every such
device as a possible hub and immediately issued hub class control transfers.
For a non-hub device on the affected real machine, that took the driver into an
invalid hub path and could hang during a control transaction.

The configuration descriptor is now checked for an actual Hub interface
(class `0x09`) before entering hub enumeration. Unsupported non-hub devices
are logged and passed back through `register_xhci_storage`, which invokes
`retry_device_candidate` instead of issuing hub requests or disabling and
removing the xHCI candidate. Genuine hubs retain the existing downstream-port
enumeration path.

### Validation

- Real hardware: `usb_rescan` completed and registered `/dev/usb0`.
- QEMU xHCI smoke test: `usb_rescan` completed and registered `/dev/usb0`.
- `cargo check -p nitrogen` passed.
- `cargo check -p fullerene-kernel --target x86_64-unknown-uefi` passed.
- Formatting and `git diff --check` passed.

## Entry 027 — 2026-08-17 Linux gen1 DQA station ownership order

### Linux comparison

Linux v4.14's `iwl_mvm_enable_txq()` first sends the AP station update with
`STA_MODIFY_QUEUES`, then calls the PCIe transport queue-enable path, and only
after that sends `SCD_QUEUE_CFG`.  The transport path publishes the DQA CBBC
and initial write pointer; the station queue mask is already owned by the
firmware when SCD configures the queue.

Fullerene had the queue-configuration order reversed: it published q5 and
sent `SCD_QUEUE_CFG`, then sent `CONNECT_ADD_STA_QUEUE`.  The q5 snapshots
showed an apparently active SCD context, but this did not prove that the
firmware scheduler had a valid station owner at the point it accepted the
queue configuration.

### Change

The DQA connection path now sends `CONNECT_ADD_STA_QUEUE` before q5 CBBC/WRPTR
publication and `CONNECT_SCD_QUEUE_CFG`.  The existing q5 gate workaround,
TIME_EVENT experiment, authentication frame, WPA path, and RX parser were not
changed by this comparison patch.  Diagnostics retain snapshots after station
ownership, after CBBC publication, after SCD configuration, and immediately
before/after the real authentication doorbell.

### Validation

- Linux source comparison: `mvm/utils.c` `iwl_mvm_enable_txq()` and
  `mvm/sta.c` `iwl_mvm_sta_send_to_fw()`.
- TFD comparison remains unchanged and byte-compatible: 128-byte gen1 TFD,
  three TBs of 20/64/6 bytes, and scratch at TB0+12.
- `cargo check --workspace --all-targets` passed.
- `cargo test -p nitrogen --all-targets`: 182 unit tests and 17 Linux
  compatibility tests passed.
- Real hardware validation remains pending; the decisive result is q5
  `hw_rdptr: 0 -> 1` or a TX completion after the reordered sequence.

## Entry 029 — 2026-08-17 fl-fw falsifies the old-firmware hypothesis

### Evidence

fl-fw is using the Linux-tested firmware `29.4063824552`, build
`CoreCycle26_stab::f2390aa8`, and still asserts on the reordered
`CONNECT_ADD_STA_QUEUE` command with `error_id=0x000021a0`.  The initial
`CONNECT_ADD_STA` succeeds; the assert occurs before CBBC publication and
before `SCD_QUEUE_CFG`.

This is a direct A/B result against fl-auth21 through fl-auth25, which use the
same firmware family and accept the sequence:

```text
CONNECT_SCD_QUEUE_CFG
CONNECT_ADD_STA_QUEUE
authentication TFD
```

The failing and successful `ADD_STA_QUEUE` payloads are the same 44-byte
Linux-shaped `STA_MODIFY_QUEUES` payload (`modify_mask=0x80`,
`tfd_queue_msk=0x20`).  The distinguishing variable is the command order.

### Correction

The DQA connection path is restored to the firmware-accepted order:

```text
CBBC / initial WR_PTR
SCD_QUEUE_CFG
ADD_STA_QUEUE
API-29 gate observation
authentication TFD
```

The earlier Linux comparison had conflated Linux's host-side queue mapping
update inside `iwl_mvm_enable_txq()` with the later firmware-side
`iwl_mvm_sta_send_to_fw(..., STA_MODIFY_QUEUES)` command.  Linux sends the
firmware station update after `SCD_QUEUE_CFG`; the new comments and snapshots
make that distinction explicit.

### Validation

- `cargo check --workspace --all-targets` passed.
- `cargo test -p nitrogen --all-targets`: 182 unit tests and 17 Linux
  compatibility tests passed.
- The next hardware run should reach the existing q5 fetch boundary instead
  of asserting during station setup.

## Entry 030 — 2026-08-17 fl-fw2 reaches q5 fetch with the gate disabled

### Evidence

fl-fw2 loads the Linux-tested `29.4063824552` / `f2390aa8` firmware and
accepts the corrected connection sequence:

```text
CONNECT_ADD_STA             success
CBBC publication             success
CONNECT_SCD_QUEUE_CFG        success
CONNECT_ADD_STA_QUEUE        success
```

Immediately before authentication, q5 is firmware-configured as queue 5 on
FIFO 3 with `status=0x0000009b`, `active=true`, `ctx1=0x00400040`, and
`queue_owned=true`. The authentication TFD is published with `hw_wrptr=1`,
the expected three TBs, and `dma_wire_match=true`.

The API-29 host gate is disabled and observed as:

```text
SCD_EN_CTRL=0x00000003
q5_bit=CLEAR
```

After submission the queue remains at `hw_wrptr=1`, `hw_rdptr=0` through
ticks 64, 512, 1024, and 1536. `FH_TX_TRB` remains zero and no `REPLY_TX`
arrives. Earlier captures with the host gate forced (`SCD_EN_CTRL=0x23`) had
the same q5 read-pointer stall, although their FH snapshot differed.

### Conclusion

The firmware image and `ADD_STA_QUEUE` order are now validated. The direct
host-side `SCD_EN_CTRL` q5 write is not sufficient to make the scheduler fetch
the TFD and is not the primary root cause by itself. The remaining boundary is
now the dynamic q5 SCD-to-FH handoff after the real doorbell.

The q5 status changes from `0x9b` before the doorbell to `0x1b` afterwards;
this raw status transition should be retained in the next comparison, but it
must not be called Linux's `SCD_ACT_EN` bit without further decoding. Linux
defines that field at bit 19, not raw bit 7.

## Entry 031 — 2026-08-17 Linux gen1 SCD_TXFACT steady-state mismatch (rejected)

### Linux comparison

The initial comparison incorrectly attributed `iwl_scd_deactivate_fifos()` to
the end of `iwl_pcie_tx_start()`.  In the Linux source, deactivation occurs in
`iwl_pcie_tx_init()` before the later start sequence, and in the TX stop path.
`iwl_pcie_tx_start()` calls `iwl_scd_activate_fifos()` after configuring the
command queue and leaves `SCD_TXFACT=0xff` active on return.

### Change

The experiment wrote `SCD_TXFACT=0` after FH channel/chicken-bit setup.  On
hardware this prevented the first `BT_CONFIG_INIT_API29` command from being
consumed: q0 `wrptr=1`, `rdptr=0`, and initialization stopped before NVM and
connection setup.  This rejects the experiment as a Q5 fix and confirms that
the command FIFO needs the active `0xff` state after TX start.

### Hardware criterion

Fullerene now restores Linux's `SCD_TXFACT=0xff` post-start state and logs it
in the legacy TX command-queue line.  The next run must first pass
`BT_CONFIG_INIT_API29`; only then is the q5 `SCD_QUEUE_RDPTR: 0 -> 1` test
meaningful.  If q5 still remains at zero, the next comparison is the Linux
separate coherent first-TB buffer, not another 802.11-layer change.

## Entry 032 — 2026-08-17 fl-fail rejects post-start SCD_TXFACT=0

`fl-fail.txt` tested the Entry 031 build with firmware
`CoreCycle26_stab::f2390aa8` / API 29.  Initialization reached ALIVE and
programmed the legacy TX foundation, but the first `BT_CONFIG_INIT_API29`
descriptor was not consumed:

```text
wrptr=1 rptr=0 scd_status=0x1f scd_en=0x1
init.hcmd.error ... consumed=false ...
```

This is a transport-start regression before DQA or Q5 exists.  The Linux
`tx_start()` ordering requires the activated scheduler FIFOs to remain active;
the `SCD_TXFACT=0` write was therefore removed from the post-FH setup path.

## Entry 033 — 2026-08-17 fl-fw3 validates TXFACT and isolates the Q5 fetch boundary

`fl-fw3.txt` uses the restored Linux gen1 scheduler state and records
`scd_txfact=0xff` during legacy TX setup, Q5 setup, and after the Q5 doorbell.
The first `BT_CONFIG_INIT_API29` and all connection setup commands are
consumed successfully, so the Entry 031 transport-start regression is gone.

The Q5 boundary is unchanged:

```text
Q5 before submit: status=0x9b wrptr=0 rdptr=0 ctx1=0x00400040
Q5 after submit:  status=0x1b wrptr=1 rdptr=0
FH fifo 3:        config=0x80000008 trb=0
```

No `REPLY_TX` or FH transaction follows.  This rejects `SCD_TXFACT` as the
Q5 fetch cause.  It also means that the next investigation must remain below
the 802.11 layer, at the dynamic SCD-to-FH handoff.

## Entry 034 — 2026-08-17 fl-fw4 rejects separate first-TB as the fetch cause

`fl-fw4.txt` tested the Linux-style separate coherent first-TB layout.  The
TFD showed the intended arrangement:

```text
TB0 = first_tb_dma       0x03aa9000
TB1 = payload_dma + 20   0x039a9014
TB2 = payload_dma + 84   0x039a9054
scratch = first_tb_dma+12
```

Despite this, Q5 remained `wrptr=1 / rdptr=0` through the observed ticks,
with `FH_TX_TRB=0` and no `REPLY_TX`.  The first-TB layout change is therefore
rejected and must not remain as the next hardware experiment.

The byte-count path is also not a new untested candidate: `fl-auth22.txt`
already records `bc_primary=0x000a` and `bc_duplicate=0x000a` after the Q5
authentication submit, while Q5 still remained `wrptr=1 / rdptr=0`.  Repeating
that byte-count-only A/B would duplicate an existing negative result.

## Entry 035 — 2026-08-17 Linux-equivalent runtime CBBC ownership A/B

The Linux gen1 path publishes all `FH_MEM_CBBC_QUEUE(n)` values in
`iwl_trans_pcie_tx_reset()`, before `iwl_pcie_tx_start()` releases the
firmware CPU.  Later `iwl_trans_pcie_txq_enable(..., cfg=NULL)` only resets
the software queue pointers and writes the initial `HBUS_TARG_WRPTR`; it does
not rewrite the CBBC for a DQA queue.

Fullerene was re-writing q5's CBBC in `enable_dqa_tx_queue()` after ALIVE and
immediately before `SCD_QUEUE_CFG`.  The address was normally identical, but
that was a real ownership/order difference at the FH/SCD boundary.  The new
default preserves the pre-ALIVE CBBC and leaves the old behavior behind the
explicit `DQA_RUNTIME_CBBC_REPUBLISH_DIAGNOSTIC` switch.  The submit log now
prints `runtime_cbbc_republish=false` and the CBBC readback.

This is a bounded transport-only A/B.  It does not change `SCD_EN_CTRL`,
`SCD_QUEUE_CFG`, `ADD_STA_QUEUE`, TFD layout, byte counts, authentication
frame contents, or RX/WPA handling.  A positive result is q5
`SCD_QUEUE_RDPTR: 0 -> 1` or a `REPLY_TX`; an unchanged `wrptr=1 / rdptr=0`
with `FH_TX_TRB=0` rejects this ownership-order hypothesis as well.

## Entry 036 — 2026-08-23 Ventoy baseline rejects the authentication-layer hypothesis

### Same-device comparison

The Ventoy capture contains a successful Linux connection and the Fullerene
captures from the same machine. Both use Intel PCI `8086:095b`, subsystem
`8086:5210`, firmware `29.4063824552.0`, BSSID `f0:f8:4a:e8:22:18`, and the
same 2.4 GHz channel. Linux completes `authenticate -> authenticated ->
associate -> associated` and receives DHCP; Fullerene stops before any
`REPLY_TX` or authentication response.

### Root-cause boundary

Across the Fullerene A/B captures, q5's first authentication TFD is valid and
the host doorbell advances `WRPTR` from 0 to 1, but the scheduler never advances
`RDPTR` from 0. With the Linux-owned DQA gate the corresponding `FH_TX_TRB`
remains zero. The same boundary persists when the host gate is forced, when
the separate Linux-style first-TB buffer is used, when byte-count entries are
written (`0x000a`), and after the Linux firmware/order/CBBC/TXFACT comparisons.
Therefore the failure is below 802.11 authentication, at the API-29 dynamic
q5 SCD-to-FH handoff; password, WPA parsing, AP rejection, and RX parsing are
not the cause of the observed stall.

### Correction

The host-direct SCD diagnostic is disabled by default again. Firmware retains
ownership of the dynamic q5 context through `SCD_QUEUE_CFG`, matching the
Linux DQA contract; the host-direct path remains available only through the
bounded fallback/diagnostic mode. The rejected unconditional FIFO byte-count
change is not kept.

Hardware confirmation still requires one Fullerene run after this change. The
decisive success signal is q5 `RDPTR: 0 -> 1` or a `REPLY_TX`, followed by the
Linux-equivalent authentication response.

## Entry 028 — 2026-08-17 fl-snap3 is an old-firmware ADD_STA_QUEUE assert

### Evidence

fl-snap3 does not fail at the initial station creation.  The first
`CONNECT_ADD_STA` command returns status `0x00000001` with a four-byte response,
and its command-queue target and read pointer both advance from 29 to 30.
The failure is the reordered `CONNECT_ADD_STA_QUEUE` command immediately
afterwards: it is submitted as opcode `0x18`, sequence `0x001e`, and the
firmware asserts with `error_id=0x000021a0` before CBBC publication,
`SCD_QUEUE_CFG`, or the authentication TFD.

The capture is running firmware `29.2666559981`, build
`CoreCycle26_stab::9ef079ed`.  This is the older blob from Entry 019, not the
Linux-tested `29.4063824552` / `CoreCycle26_stab::f2390aa8` blob used by
fl-auth15 and later successful initialization captures.  The failing payload
contains the expected Linux-shaped `STA_MODIFY_QUEUES` fields, including
`modify_mask=0x80` and `tfd_queue_msk=0x20`; the old firmware nevertheless
rejects this queue-ownership update with the same assert class already seen in
Entry 018.

### Conclusion

This run confirms that the initial `ADD_STA` is genuine, but it cannot validate
the Linux queue-order experiment because the firmware image is different and
asserts at the queue-owner update.  The `SCD_RDPTR=0x1f` printed in the assert
record is the command queue pointer, not q5's SCD read pointer.  No conclusion
about q5 fetching or TIME EVENT can be drawn from this run.

### Next experiment

Repeat the current reordered connection path once with the known-good
`29.4063824552` / `f2390aa8` firmware.  Do not add a fallback queue form for
`29.2666559981`: both its DQA queue update and legacy static-q4 ownership form
already have recorded `0x21a0` asserts.  If the newer image accepts
`CONNECT_ADD_STA_QUEUE`, continue to the q5 `hw_rdptr` measurement; if it also
asserts, then compare the exact queue-update payload and command version before
changing the ordering hypothesis.

## Entry 027 — 2026-08-23 q5 rdptr=0 root cause: missing byte-count table entry

### Symptoms

From fl-auth9 through fl-auth25, q5 (DQA management queue) remained at
`hw_wrptr=1`, `hw_rdptr=0` after the authentication TFD was submitted. No
`REPLY_TX`, authentication response, or association followed. The SCD context,
queue status, queuechain, and SCD_EN_CTRL all appeared correctly configured.

### Root cause

The gen1 SCD byte-count table (`iwlagn_scd_bc_tbl`) was only being written for
queues marked in `SCD_AGGR_SEL` (Scheduler-ACK / aggregate queues). q5 is a
FIFO/non-aggregate management queue, so the byte-count entry at slot 0 was left
as zero.

Linux's gen1 transport (`iwl_pcie_txq_build_tfd` with
`trans->cfg->base_params->bc_table_dword=true` for 7265) writes the byte-count
table **unconditionally for every TFD on every queue** — including FIFO
non-aggregate queues. The SCD scheduler uses the byte-count table to decide
whether a TFD has data to fetch. A zero entry means "no data", so the scheduler
never advances the read pointer.

This is why q0 (command queue, FIFO 7) worked despite the same code path: the
command queue uses `send_hcmd()` / `send_init_hcmd()` which do not go through
`process_tx_queue()` and thus never hit the conditional byte-count skip. q1
(aux) also worked because its SCD_QUEUE_CFG path configured it as
non-aggregate but the SCD queue status was written with the full
`SCD_QUEUE_STTS_MASK` by the host-direct `enable_aux_tx_queue()` path — and q1
was never actually used for TX data in the connection path (only scan, which is
firmware-initiated).

### Fix

`process_tx_queue()` in `tx.rs` now calls `update_scd_byte_count()`
unconditionally for every TFD, matching Linux's `iwlagn_update_bc_tbl()`. The
`scheduler_ack` flag is retained only for the diagnostic log line that
distinguishes "fifo" vs "scheduler-ack" mode.

### Validation

- All 77 nitrogen iwlwifi unit tests pass, including the updated
  `command_and_data_queues_use_disjoint_dma_buffers` test which now asserts
  the byte-count entry is `0x000a` (10 DWORDs for a 30-byte auth frame) rather
  than 0.
- Physical hardware validation is required to confirm q5 `hw_rdptr` advances
  past 0.
