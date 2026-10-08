# Software Bug Journal

## Entry 037 — 2026-08-23 UTC / 2026-08-24 JST API-29 DQA host gate restored to Linux default

### Finding

The source comments and the Linux-compatible DQA tests described q5
activation as firmware-owned, but `API29_DQA_HOST_SCD_GATE_DIAGNOSTIC` was
still set to `true`. That made the production path write q5 into
`SCD_EN_CTRL` after `ADD_STA_QUEUE`, even though the bounded A/B workaround
had already been rejected as a cause of the `WRPTR=1/RDPTR=0` stall.

### Change

The diagnostic switch is now `false` by default. The explicit branch remains
available for a controlled hardware comparison, while the normal path leaves
dynamic queue activation to the API-29 firmware. A unit test now verifies that
the default path does not alter the peripheral gate value or emit another
doorbell.

The repeated connection-error state transition was also factored into one
typed helper. This reduces control-flow duplication without changing the
error status or message returned to callers. Bonder's management-frame
builders now reserve their fixed/derived capacity, and
`bonder/examples/bench_wifi_frames.rs` records the host-side allocation path
through `Instant` for repeatable optimization measurements.

### Validation

- `cargo check --workspace --all-targets`: passed without compiler warnings.
- `cargo test -p nitrogen --all-targets`: 185 unit tests and 17 Linux
  compatibility tests passed.
- `cargo test --workspace`: passed; the freestanding kernel payload examples
  are opt-in and the ignored throughput test remains intentionally manual.
- `cargo run -p bonder --release --example bench_wifi_frames`: completed;
  the measured host run reported 12.6 ns/auth frame, 24.0 ns/WPA2
  association, and 12.6 ns/deauth frame. Values are machine-dependent.
- Real 7265D AP authentication is still the required hardware gate; source
  and replay tests cannot prove a physical q5 `RDPTR` advance.

This document records non-obvious software bugs encountered during
development, their root cause analysis, and the fix applied.

## Entry 038 — 2026-08-23 UTC / 2026-08-24 JST API-29 TIME_EVENT caused a firmware assert

### Finding

The real-device log `202608240623.txt` reports an Intel 7265D API-29 image
(`iwlwifi-7265D-29`, build `4063824552`) with `dqa=true` but
`session_prot=false`. The connection setup through `CONNECT_ADD_STA_QUEUE`
was accepted. The driver then submitted `POST_DQA_TIME_EVENT` and the
firmware immediately reported `error_id=0x00001986`
(`ADVANCED_SYSASSERT_OR_UNKNOWN`) for opcode `0x29`; authentication TX was
never reached. The same sequence and assertion are present in
`202608231937.txt`.

### Change

The API-29 TIME_EVENT experiment is disabled by default and is additionally
gated on the firmware's explicit session-protection capability. This keeps
the command available for a future compatible image without sending it to
the affected 7265D firmware. Older runs such as `202608231913.txt` show the
correct fallback behavior: the command is skipped and the authentication TFD
is submitted, although that run still has the separate q5 scheduler stall
(`hw_wrptr=1`, `hw_rdptr=0`, `FH_TRB=0`).

### Validation

- `cargo fmt --all -- --check`, `git diff --check`, and
  `cargo check -p nitrogen --all-targets` passed.
- `cargo test -p nitrogen --all-targets` passed: 185 unit tests and 17 Linux
  compatibility tests.
- Strict `cargo clippy -p nitrogen --all-targets -- -D warnings` remains
  blocked by pre-existing warnings outside this change; no new warning was
  introduced by the TIME_EVENT gate.
- A new 7265D hardware run must confirm that the firmware assert is gone and
  expose the remaining q5 consumption issue without the crash masking it.

## Entry 039 — 2026-08-23 UTC / 2026-08-24 JST TIME_EVENT退行の除去を実機で確認

### Evidence

The follow-up log `202608240642.txt` reports
`time_event.skipped api=29 dqa=true experiment=false session_prot=false`.
There is no firmware error record or `ADVANCED_SYSASSERT` after this point,
and the driver reaches authentication-TFD submission with the expected
`bc_dwords=10`, three TFD buffers, and q5 queue ownership.

The original authentication problem remains isolated: q5 stays at
`hw_wrptr=1`, `hw_rdptr=0`, and `FH_TRB=0` at ticks 64, 512, and 1024. The
queue context is still Linux-compatible (`win_size=64`, `frame_limit=64`),
and the configured FIFO remains active (`fifo=3`, `config=0x80000008`). The
Linux-owned default leaves q5 out of `SCD_EN_CTRL` (`scd_en=0x00000003`), but
earlier host-gate runs also failed to advance q5, so enabling that bit alone
is not a sufficient fix.

### Status

The TIME_EVENT firmware-crash regression is resolved and hardware-confirmed.
The remaining work is the q5 scheduler/FH fetch path; no speculative register
change is promoted until the next bounded A/B run identifies a necessary
condition for `RDPTR` progress.

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

## Entry 027 — 2026-08-23 q5 byte-count parity experiment

### Symptoms

From fl-auth9 through fl-auth25, q5 (DQA management queue) remained at
`hw_wrptr=1`, `hw_rdptr=0` after the authentication TFD was submitted. No
`REPLY_TX`, authentication response, or association followed. The SCD context,
queue status, queuechain, and SCD_EN_CTRL all appeared correctly configured.

### Hypothesis

The gen1 SCD byte-count table (`iwlagn_scd_bc_tbl`) was only being written for
queues marked in `SCD_AGGR_SEL` (Scheduler-ACK / aggregate queues). q5 is a
FIFO/non-aggregate management queue, so the byte-count entry at slot 0 was left
as zero in earlier captures.

Linux's gen1 transport (`iwl_pcie_txq_build_tfd` with
`trans->cfg->base_params->bc_table_dword=true` for 7265) writes the byte-count
table **unconditionally for every TFD on every queue** — including FIFO
non-aggregate queues. The SCD scheduler uses the byte-count table to decide
whether a TFD has data to fetch. A zero entry means "no data", so the scheduler
never advances the read pointer.

This made the byte-count entry a valid Linux-parity correction candidate, but
not yet a proven root cause.

This is why q0 (command queue, FIFO 7) worked despite the same code path: the
command queue uses `send_hcmd()` / `send_init_hcmd()` which do not go through
`process_tx_queue()` and thus never hit the conditional byte-count skip. q1
(aux) also worked because its SCD_QUEUE_CFG path configured it as
non-aggregate but the SCD queue status was written with the full
`SCD_QUEUE_STTS_MASK` by the host-direct `enable_aux_tx_queue()` path — and q1
was never actually used for TX data in the connection path (only scan, which is
firmware-initiated).

### Linux-parity correction

`process_tx_queue()` in `tx.rs` now calls `update_scd_byte_count()`
unconditionally for every TFD, matching Linux's `iwlagn_update_bc_tbl()`. The
`scheduler_ack` flag is retained only for the diagnostic log line that
distinguishes "fifo" vs "scheduler-ack" mode.

### Validation and disposition

- All 77 nitrogen iwlwifi unit tests pass, including the updated
  `command_and_data_queues_use_disjoint_dma_buffers` test which now asserts
  the byte-count entry is `0x000a` (10 DWORDs for a 30-byte auth frame) rather
  than 0.
- Physical hardware validation is required to confirm q5 `hw_rdptr` advances
  past 0.

The later `202608240709.txt` capture published `bc_dwords=10` and both
`bc_primary=0x000a` and `bc_duplicate=0x000a`, yet q5 still remained at
`hw_wrptr=1`, `hw_rdptr=0`, with `FH_TRB=0`. Therefore the missing byte-count
entry is not the root cause of the remaining authentication stall. The
unconditional table update remains because it matches Linux, but the cause is
still under investigation.

## Entry 040 — 2026-08-23 UTC / 2026-08-24 JST q5 TX wake-hold experiment rejected

### Evidence

The fixed API-29 run `202608240642.txt` no longer asserts at TIME_EVENT, and
the authentication TFD reaches q5 with a valid CBBC, byte-count entry, TFD,
and write pointer.  It nevertheless remains at `hw_wrptr=1`, `hw_rdptr=0`
while `GP_CNTRL` changes from `0x080403cd` at submission to
`0x080403c5` during the watchdog polls; this suggested a wake-hold experiment.

### Experiment

The experiment temporarily required both the host-command and data-TX rings
to be empty before releasing `MAC_ACCESS_REQ`.  It was not supported by the
upstream Linux transport contract and is therefore reverted.

### Validation

The new run `202608240709.txt` kept `GP_CNTRL=0x080403cd` throughout the q5
polls, but still showed `hw_wrptr=1`, `hw_rdptr=0`, `FH_TRB=0`, and no
`REPLY_TX`.  The wake-hold hypothesis is consequently rejected.  The unit
test is restored to Linux's command-only wake-hold behavior.

Physical validation is still required; the next decisive comparison is the
Linux `iwl_trans_txq_enable_cfg(..., cfg=NULL)` DQA transport initialization
and its queue-used/read-write-pointer state before `SCD_QUEUE_CFG`.

## Entry 042 — 2026-08-23 UTC / 2026-08-24 JST 5 GHz authentication selected an illegal CCK rate

### Evidence

The Linux capture in `linux-wifi-connected-20260815-231412/report.txt` connects
the same 7265D revision and firmware (`29.4063824552`) to `Buffalo-G-2218`,
while `202608240736.txt` selects the 5 GHz `Buffalo-A-2218` AP on channel 48.
Despite that selection, the Fullerene authentication log reports
`rate_n_flags=0x0000420a` and `band=2.4GHz`. That is the 1 Mbps CCK rate. The
5 GHz authentication TFD is accepted by the command path but cannot be
transmitted by the radio, matching the observed q5 `WRPTR=1/RDPTR=0` stall and
absent `REPLY_TX`.

### Root cause and correction

`tx_rate_n_flags()` determines the band from `wifi_conn.current_bssid`, but
`connect()` only populated that BSSID after association. The first
authentication frame consequently used the fallback 2.4 GHz rate. `connect()`
now publishes the selected AP BSSID before any authentication TX, and a replay
test verifies that a channel-36 authentication frame selects `0x0000410d`
(6 Mbps OFDM).

The earlier Linux-compatible wake correction remains in place; this rate bug
was independent of the MAC wake state and is the next hardware-validation
candidate. Success is indicated by q5 `RDPTR` advancing and receipt of the AP
authentication response.

## Entry 041 — 2026-08-23 UTC / 2026-08-24 JST Linux TX doorbell wake condition

### Evidence

The Linux gen1 PCIe TX path checks the firmware-owned `MAC_SLEEP` bit in
`CSR_UCODE_DRV_GP1` before asserting `CSR_GP_CNTRL_MAC_ACCESS_REQ`.  When the
MAC is already awake, it writes the queue write pointer directly.  In
`202608240709.txt`, the pre-authentication snapshot reports GP1 without the
sleep bit and `CSR_GP_CNTRL` without `MAC_ACCESS_REQ`, while the Fullerene q5
submission changes `CSR_GP_CNTRL` to `0x080403cd` and leaves that request held
through the failed polls.

### Correction

`process_tx_queue()` now follows the same split: it reads `CSR_UCODE_GP1` and
calls the existing host-command wake path only when `MAC_SLEEP` is set.
Host-command submission retains its stronger wake behavior because command
processing performs internal scheduler and register operations.  The new unit
test verifies that an awake data-TX doorbell does not set a persistent
`MAC_ACCESS_REQ` hold.

### Disposition

This is a Linux-parity correction motivated by a concrete hardware trace, not
yet a confirmed complete fix for q5.  Re-test on the 7265D is required.  A
positive result is q5 `RDPTR: 0 -> 1` or a `REPLY_TX`; if q5 remains at
`WRPTR=1/RDPTR=0`, the next comparison is the per-queue DQA transport state
(`queue_used`, software read/write pointers, and the initial doorbell) before
`SCD_QUEUE_CFG`.

## Entry 043 — 2026-08-23 UTC / 2026-08-24 JST 7265D firmware stability and stale station reuse

### Evidence

The latest hardware capture, `202608240756.txt`, uses the same Intel 7265D
firmware line as the successful Linux capture: `iwlwifi-7265D-29`, API 29,
build `4063824552`, `CoreCycle26_stab::f2390aa8`. INIT/ALIVE, runtime
initialisation, and repeated scans complete normally, so the capture provides
no evidence of a corrupt or unstable firmware image.

The failure occurs when the first `Buffalo-G-2218` authentication remains
pending on q5 (`WRPTR=1/RDPTR=0`) and a second connection to `Buffalo-A-2218`
reuses station ID 0. The second `CONNECT_ADD_STA` produces firmware
`error_id=0x00002073` (`ADVANCED_SYSASSERT_OR_UNKNOWN`). This is consistent
with a stale host-side station/queue lifecycle, not with a firmware-version
mismatch.

### Correction

Before a validated reconnect, Fullerene now tears down the previous failed
connection: it abandons the stalled data queue, sends `REMOVE_STA` for the
managed AP, and resets authentication, WPA, DHCP, and watchdog state. The
same cleanup runs when the authentication or association watchdog reaches its
terminal timeout. Invalid target SSIDs and incompatible security parameters
are validated first, so a rejected request does not tear down a working
connection.

### Disposition

The firmware is the known Linux-supported final 7265D/Core26 `-29.ucode` line,
and the identical build connects successfully under Linux. The fix still
requires a fresh Nitrogen hardware run. The expected next trace is one
`CONNECT_ADD_STA` per attempt, no `0x2073` assert on reconnect, and q5
`RDPTR` progress or a `REPLY_TX` for the 5 GHz authentication test.


## Entry 044 — 2026-10-07 JST Host-side verification sweep (no hardware attached)

### Evidence

- The `202608231920/202608231913` desktop-attachment logs plus commit
  `af09121f` settle the TIME_EVENT question definitively: the 7265D-29
  firmware (build `4063824552`) advertises `session_prot=false`
  (`firmware.capabilities api_index=0/2 ... dqa=true session_prot=false`;
  `time_event.api_selection ... session_prot_cap=false`). The experiment
  flag stays `false` and any send is gated on
  `fw_session_prot_supported` — which matches Linux's skip path for this
  firmware. The missing-TIME_EVENT root-cause theory is therefore
  rejected; the earlier skill note claiming `API29_TIME_EVENT_EXPERIMENT
  = true` was stale and has been corrected.
- `cargo test -p nitrogen --lib iwlwifi`: **79/79 green** (includes the
  Entry-042 replay test `five_ghz_auth_selects_ofdm_rate_before_association`).
- Source presence of the three active candidates verified: 041 wake gate
  (`tx.rs:139`, `CSR_UCODE_GP1_BIT_MAC_SLEEP`), 042 BSSID-before-auth
  (`connection_state.rs:2225`), 043 `reset_failed_connection`
  (`connection_state.rs:2159`).
- Linux v4.14 `iwl_trans_pcie_txq_enable` (cfg==NULL DQA path) diffed
  against fullerene's `enable_dqa_tx_queue`: for the boot case
  (ssn==write_ptr==0) they match — the initial doorbell is
  `HBUS_TARG_WRPTR = queue << 8` in both; the 7265D `scd_bug` ssn++
  workaround correctly does not trigger; no second zero-pointer doorbell
  after `SCD_QUEUE_CFG` (Linux's first post-config doorbell is the TFD's
  own write pointer).

### Disposition

No further host-side change is warranted from this sweep. The next
decisive datum is a fresh Nitrogen run on the 7265D. Success markers:
q5 `RDPTR` 0 -> 1 or a `REPLY_TX` for the 5 GHz authentication, one
`CONNECT_ADD_STA` per attempt, and no `0x2073` assert on reconnect.


## Entry 045 — 2026-10-07 JST Ventoy log sweep: fl-wifi39/40/41 analysis

### Evidence

- **Firmware swapped since the Aug-23 captures**: `bonder/iwlwifi` now carries
  `iwlwifi-7265D-29.ucode` = 1,036,312 B, build `2666559981` / `CoreCycle26_stab::9ef079ed`
  (file dated Aug 16). The Aug-23 attachment logs used the earlier
  `f2390aa8` / 4063824552 (1,036,772 B). fl-wifi39/40/41 all run the NEW blob
  (2 occurrences per log = INIT + runtime).
- fl-wifi41 (newest) flow: full INIT + runtime ALIVE, scan, then connect to
  `Buffalo-G-2218` (2.4 GHz): `CONNECT_MAC_CONTEXT` acked (rptr=28),
  `CONNECT_ADD_STA` acked, `CONNECT_ADD_STA_QUEUE` acked, `SCD_EN_CTRL=0x23`
  q5 SET, capability gate correctly SKIPS TIME_EVENT ("session protection
  skipped ... using bound PHY channel 11" — bit 22 of the TLV capability
  bitmap is clear for this firmware too).
- **The q5 stall persists unchanged**: auth TX (rate 0x420a, legal for the
  2.4 GHz AP), doorbell `wrptr=0x501`, SCD `hw_wrptr=1/hw_rdptr=0` through
  polls 64/512/1024; FH TRB active `0x80305000`, FIFO-3 credit reduced
  once (0x1ff8 -> 0x1f98) then frozen; `fifo_buf=0x20000700` (channel 3 not
  empty). Identical signature to the Aug-23 captures: the FH fetched the
  TFD once and loaded FIFO 3; nothing consumes it, so the SCD read pointer
  never advances.
- SCD queue status decode (registers.rs): q0=0x9F/q5=0x9B/q1=0x9D differ
  only in the low-3-bit FIFO number (7/3/5) — same ACTIVE|WSL|bit7 layout,
  so the status word is NOT the differentiator.
- The 041 wake fix is live in the log: polls show `gp1=0x00000000`,
  `gp_cntrl=0x080403c5` (no persistent MAC_ACCESS_REQ hold).

### Disposition

The stack of committed fixes (041 wake, 042 BSSID, 043 teardown) changes the
init/preamble state but not the core stall: the MAC never consumes FIFO 3.
The next experiments should target the MAC/radio-level configuration that
grants channel access for management TX (Linux connects with identical
hardware and firmware, so this remains a software-parity gap). Candidate
areas: PHY context / channel-context parity at the moment of auth, and the
`FH_TSSR_TX_STATUS` channel-3 backpressure versus the firmware's expected
TX-path state right after `SCD_QUEUE_CFG`.


## Entry 046 — 2026-10-07 JST ANE: the "Љ" descriptor corruption is a boot-race, root-caused and fixed (v197)

### Symptom and host-side evidence

Roughly half of all boots enumerated with corrupted descriptors: the host
read "Љ" (U+0409 = the langid descriptor's 0x0409 value) where the
manufacturer/product strings belong, the product string surfaced in the
SerialNumber slot, and "config index 0 descriptor too short (expected 32,
got 9)" appeared while reading the configuration descriptor — the classic
"every answer one request behind" signature. The worse variant never
enumerated at all: `device descriptor read/64, error -110` (5 s timeout) on
the first read, then `-71` and `-75` storms across repeated port resets.

The host's journal (`journalctl -k`, the `usb 1-1:` lines) separates the two
boot classes cleanly, boot by boot:
  good: `new high-speed USB device number N` → `New USB device found` →
        `Product: ANE fastboot` in ~1 s, no error lines before it.
  bad:  errors (`-110`, `-71`, `-75`, `-90`) then a garbled or no product.

### Root cause

`usb_bringup()` step 10 cleared DCTL.SftDiscon and attached the device to
the host immediately — while `ane_usb_dev::init` (the EP0 arm, called later
at main.rs:1075), the v162 timer-IRQ responder pump, and the storage
bring-up were still starting. The host begins its enumeration the moment it
sees the attach. Whether its descriptor reads land before or after the
responder comes alive is a race — and that race is the ~50/50.

When a request is answered *late* (host already timed out, `-110`) and the
port reset then lands mid-answer, the EP0 TxFIFO keeps stale words. Every
later answer is served from the FIFO head, so it trails one request behind
— forever, for that boot. This is the same "one request behind" the
v49/v52 era fought (see the service()-before-storage comments in main.rs);
those fixes added more service() calls, but the attach itself still
preceded the responder.

### Fix (v197)

Keep the device soft-disconnected through the whole bring-up:

- `platform/ane/mod.rs` `usb_bringup()` step 10: **sets** DCTL.SftDiscon
  (bit 1) instead of clearing it (the USB core stays off the bus).
- `platform/ane/mod.rs`: new `pub fn usb_attach()` clears SftDiscon
  (with a ~50 ms settle).
- `arch/aarch64/main.rs`: `usb_attach()` is called after
  `ane_emmc::probe`, so the host's fresh enumeration meets an armed
  responder and a live timer-IRQ pump. First measurements pending; the
  prediction is: every boot shows a clean `New USB device found` →
  `Product: ANE fastboot` with no `-110/-71/-75/-90` lines before it.

### Engine v11 (host side, same session)

Two fixes to `night_engine.sh` (deployed as `night_engine_v11.sh`):
- `flash_now()` now resets `STALL=0`: the v193 stall rescue had misfired at
  15:27 by counting stall time *across* a live replacement boot and firing
  `oem crash` seconds after a fresh boot enumerated.
- The 180 s settle is now a watch loop: the moment the loader's
  "Fastboot2.0" product takes over (kernel died), it re-flashes instead of
  waiting out the full settle.


### Entry 046 addendum — v197 first boots: attach race was not the whole story; v198 targets the real mechanism

First two v197 boots: **boot 1 garbled** (dev 62: "config index 0
descriptor too short (expected 32, got 13)", Product/Manufacturer "Љ" =
the langid descriptor payload, SerialNumber = the product string), **boot
2 clean** (dev 65: "Product: ANE fastboot", no error lines before it). The
deferred attach removes the pre-responder -110 window but does NOT stop
the one-behind shift: it still fires mid-enumeration on a late-attached
fresh session.

Mechanism pinned in code (the "stale belt"): service_ep0's belt answered
when `SETUP_BYTES_READY` was set, but that flag can still be true from the
PREVIOUS setup while `SETUP_BUF` holds its bytes - this core can surface
the DOEPINT0.SETUP latch before the RX entry is readable, so the belt
occasionally served the OLD request's data as the answer to the NEW
request ("got 9" = the 9-byte config header re-served to the full-config
read; the string shift = langid/product payloads one slot late).

v198 fix: answering moves to ONE site at the end of `service()`, gated by
a new `SETUP_BYTES_FRESH` flag (set only by the RX pop, for THIS setup)
and `!TRANSACTION_BUSY` (never mid-transfer; a pending setup is picked up
on the next pass). The inline RX answer and the belt are removed. Plus a
bounded forensic trace (`ud: q=BBWWWWWW LLLL` ring lines per answered
request) so the EP0 request stream survives in EP1 pulls and eMMC slices.


### Entry 046 addendum 2 — the /sys descriptor dump instrument; the v198 regression; the v200 shape

**New observability channel (works even for garbled/failed boots):**
`/sys/bus/usb/devices/1-1/descriptors` holds the raw descriptor bytes the
HOST kernel read, cached per device - readable with no USB session at all
(plus the parsed fields: bMaxPacketSize0, bConfigurationValue,
bNumInterfaces, manufacturer/product/serial). Captured per boot by
`~/dev/p20-root/forensics_snap.sh` into `forensics/snap_<time>{.txt,.bin}`.

First byte-level contrast, same handset, same evening:
  CLEAN (v200 boot, dev 80): 50 B = 18 B device desc (perfect) + 32 B
  config desc (perfect: `09 02 20 00 01 01 00 80 32 ...`).
  GARBLED (v199 boot, dev 78): 27 B = 18 B device desc (perfect!) + a
  9-byte config read = `09 02 20 00 | 09 02 20 00 | 01` vs the expected
  header `09 02 20 00 | 01 01 00 80 | 32`: word0 duplicated, remainder
  truncated. The device-descriptor phase is CLEAN even on garbled boots;
  the corruption starts exactly at the first CONFIG read. The duplicated
  word0 = a FIFO head anomaly (an extra partial push or a duplicated
  answer word), matching the "one request behind" host symptoms.

**The v198 answering experiment (failed on hardware, reverted in v200):**
v198 moved EP0 answering to a single site at the end of service() gated
by SETUP_BYTES_FRESH + !TRANSACTION_BUSY. Result: 2/2 hard enumeration
failures (`-90` device-descriptor short reads, host power-cycles the
port and gives up) - WORSE than the ~50% garble it was meant to cure.
v200 restores the proved v197 answering shape (inline answer at the RX
pop + belt) and keeps: the v197 deferred attach, the v199 enumeration
sentinel (panic if attached but never SET_CONFIGURATION within ~2048
beats), and the v198 forensic `ud: q=` ring trace. First v200 boot
enumerated cleanly (dev 80, 16:10:57).

**Boot-death cadence discovered:** non-enumerating boots die at ~4 m 45 s
(~beat 8192 at the measured rate) - the same constant across five boots
(15:22, 15:34, 15:39, 15:55, 16:00). Clean boots pass 8192 and end via
the 16384-beat cap (~9-10 min) or the engine's magic. The 8192 constant
coincides with the IRQ-time eMMC persist lane's first write; treat it as
the prime suspect for the bad-boot deaths (a stalled boot thread + the
IRQ SDHCI write). Not yet conclusive.


### Entry 046 addendum 3 — the clean trace baseline, the persist-lane kill, and the v202 self-heal

**The clean enumeration script (from `ud: q=` traces of collected rings
adb_118/adb_13, extracted to forensics/trace_c*.txt):**
```
GET_DESC wv=0x0100 len=0x40    (device desc, 64)
SET_ADDR wv=0x00NN len=0       (addr varies per boot: 7, 9)
GET_DESC wv=0x0100 len=0x12
GET_DESC wv=0x0200 len=0x9     (config header)
GET_DESC wv=0x0200 len=0x20    (config full)
GET_DESC wv=0x0300 len=0xff    (string0)
GET_DESC wv=0x0302 len=0xff    (product)
GET_DESC wv=0x0301 len=0xff    (manufacturer)
GET_DESC wv=0x0303 len=0xff    (serial)
SET_CONFIG wv=0x0001           (value 1!)
GET_CONFIG len=0x1
GET_DESC wv=0x0300 len=0x4
GET_DESC wv=0x0303 len=0xff
GET_DESC wv=0x0600 len=0xa     (device qualifier)
GET_STATUS len=0x2
GET_DESC wv=0x0600 len=0xa
GET_DESC wv=0x0a00 len=0x4     (debug descriptor)
GET_STATUS len=0x2
```
Identical across clean boots (the address + a debug-descriptor retry vary).

**v201: the IRQ-time eMMC persist lane is DISABLED.** All non-completing
boots died at ~4 m 45 s ~ beat 8192 = that lane's first write (the same
deadlock noted at v124). The on-demand `oem ringsave` remains. Result:
clean boots now survive to the collection window reliably.

**v202: the self-heal reconnect.** When a boot's SET_CONFIGURATION value
never becomes 1 (host failed the reads, OR accepted a garbled
bConfigurationValue of 2+), the timer IRQ drops the D+ pull-up for ~10
beats, reconnects, and re-stamps the attach beat - up to 3 draws, then
the old panic. Each reconnect makes the host enumerate afresh: the ~50%
garble lottery becomes several independent draws, and a healed boot
shows up as a NEW event the engine then collects. (`usb_config_value()`
added; CONFIG_VALUE latched on every SET_CONFIGURATION.)

**Engine v12:** the v11 settle loop ran a full `journalctl -k` per
iteration (~1 s each), stretching the nominal 180 s to ~280 s real and
missing the window; v12 uses bounded `journalctl -k -n 40` checks,
40 x 3 s ~ 130 s real. v12 also captures the host-side descriptor
snapshot (`forensics_snap.sh`) automatically for every boot event.

**Client-path findings (all dead ends recorded):** the Android fastboot
tool cannot talk to the kernel responder at all (LK only; silent empty
output) - never poke a kernel window with it (it consumes/breaks the
window: a later pyusb CNXN fails with the SET_CONFIGURATION dance -110).
pyusb fails on garbled boots at descriptor parsing; raw libusb (python
usb1) fails too because the host registered ZERO interfaces for the
garbled config (`claimInterface` -> LIBUSB_ERROR_NOT_FOUND). A garbled
boot is unreachable from the host, period - only /sys dumps + the
device's own self-heal can act on it.


### Entry 046 addendum 4 — the double-answer root cause candidate + v203 self-heal + v204

- **/sys descriptor dumps are THE instrument for suspect boots** (works
  without any USB session; the host kernel caches the raw bytes it read).
  Clean boot = 50 B: 18-B device descriptor + 32-B config, perfect.
  Garbled boot (observed twice: dev 78 and dev 105, byte-identical) = 27 B:
  perfect first 18 B + a config-header read served as
  `09 02 20 00 | 09 02 20 00 | 01` - expected `09 02 20 00 | 01 01 00 80 |
  32`. The FIRST WORD of the response is served TWICE, then the stream
  truncates. The stray 0x02 lands in bConfigurationValue, the host picks
  "config 2" with 0 interfaces - the whole downstream mess.
- **Mechanism (candidate): the double-answer.** The RX-pop path pushes
  the answer; later in the SAME service() pass a stale XFERCOMPL from the
  previous transfer clears SETUP_HANDLED; then the SETUP-branch belt
  (`SETUP_BYTES_READY && !SETUP_HANDLED`) re-answers the SAME setup. Two
  copies of one response staged in the EP0 FIFO; the surplus surfaces on
  later reads (duplicated word0, truncated reads, the "got 9"/"got 13"
  variants). Timing-dependent -> the ~50% boot lottery.
- **v203 self-heal confirmed live**: a boot that saw >= 1 SETUP but never
  SET_CONFIGURATION(1) power-cycles the USB link (DCTL.SftDiscon, ~10-beat
  hold) up to 3 times, then panics to LK. Observed (boot #9): reconnect at
  ~70 s after the first SETUP (the host logged a fresh connect), the
  redraw was still garbled, further host retries failed, retries
  exhausted -> panic -> LK -> reflash. NOTE: the redraws were NOT
  independent - consistent with EP0 FIFO residue surviving reconnects
  (nothing flushes it). Candidate v205: flush TxFIFO on handle_reset.
- **v204 (double-answer eliminator)**: the RX pop now consumes the
  DOEPINT0.SETUP latch and CNAKs before handle_setup(), so the belt
  cannot re-fire for the same setup in the same pass. First boot (dev
  115) clean; rate check pending across the next boots.
- **Instruments written tonight**: `forensics/boot_ledger.py` +
  `forensics/boot_ledger.tsv` (classifies every usb 1-1 attach:
  CLEAN/GARBLED/LOADER/FAILED), `forensics_snap.sh` (per-boot /sys
  descriptor dump + parsed fields), and the `ud: q=` request traces now
  riding in the ring (readable from EP1 pulls on clean boots).
- **Clean enumeration script** (from ring traces, consistent across
  boots): GET_DESC(dev,64) -> SET_ADDRESS -> GET_DESC(dev,18) ->
  GET_DESC(cfg,9) -> GET_DESC(cfg,32) -> str0,str2,str1,str3 ->
  SET_CONFIG(1) -> GET_CONFIG -> str0(4), str3 -> qualifier(10) ->
  GET_STATUS -> qualifier -> debug(4) -> GET_STATUS -> SET_CONFIG(1)...
  (arch + BOS probes are what follow).


### Entry 046 addendum 5 — THE MECHANISM CONFIRMED: EPENA-stuck + stale TSIZ (v206, 17:04)

Boot #13 (v206, first boot with the guard+flush) produced the direct proof:

- The host's enumeration (dev 29-31) hit the usual failure storm
  (-110 -> -71 -> -75, power cycle, -75 again) - and then, WITHIN THE SAME
  BOOT and without any device-side reconnect, the host's 4th attempt
  (dev 32) enumerated PERFECTLY ("ANE fastboot", full strings). The first
  in-boot recovery from the failure class ever observed.
- The collected ring (adb_101.bin) carries the causal chain:
  - `WD: EP0 EPENA` x 4 - ep0_send found DIEPCTL0.EPENA still set (the
    previous control transfer was never retired - the host aborted it
    mid-flight during the failed attempts). Four aborted transfers / four
    detections. WITHOUT the v206 force-disable, the following
    `DIEPTSIZ0` write is SILENTLY DROPPED by the core (EPENA=1) and the
    OLD xfersize drives the next packet -> stale-size/short/duplicated
    reads. That is the "one request behind" / [09 02 20 00 | 09 02 20 00
    | 01] garble class, at last pinned to a register-level mechanism.
  - `UD: FFLUSH` x 7 - the new per-bus-reset Tx+Rx FIFO flush ran at
    every port reset (including the host's own retry resets), sweeping
    the residue that used to make every later attempt inherit the same
    bad hand.
  - `WD: RECONNECT` x 0 - no self-heal needed this boot: the host's own
    retries + the active dirty state being cleared converged to clean.
- q= trace shows the host's retried reads (cfg9 x2, cfg32 x2 before the
  good attempt) - matching the failure/retry cadence in the journal.
- v206 = ep0_send EPENA guard (bounded wait, then EPENA|EPDIS force-
  disable, counted in DBG_EPENA_STUCK, ring marker "WD: EP0 EPENA") +
  GRSTCTL Tx/Rx flush at handle_reset ("UD: FFLUSH") + GRSTCTL_AHBIDLE
  const. First boot: clean, self-recovered. Scoreboard continues below.

### THE ROOT CAUSE, FINAL FORM (2026-10-07 ~17:10)

1. At enumeration, host attempts have timing margins (cold responder ->
   -110 timeouts sometimes; WHY the first reads sometimes time out is
   still the remaining open sub-question - likely the responder latency
   vs the host's first-read window).
2. Any aborted/timed-out control transfer leaves DIEPCTL0.EPENA set on
   the device (the core never retires it without a host handshake).
3. The NEXT ep0_send writes DIEPTSIZ0 while EPENA=1 -> the core drops
   the size write -> the response is driven by the STALE size and the
   FIFO accumulates data out of order -> the garbled/short/duplicated
   reads ("Љ" strings, "got 9"/"got 13", the duplicated first word).
4. Which boot/attempt lands in this state = timing -> the ~50% lottery.
   The dirty state also persists across D+-only reconnects, which is why
   v203's self-heal redraws kept failing until v206's flush.

Fix surface (now live): v206 EPENA-guard (kills 3) + per-reset FIFO flush
(sweeps 2's residue and gives every retry/reconnect a clean slate).


### Entry 046 addendum 6 — the engine v13-v16 hardening + LK idle auto-boot (17:10-17:30)

- **Boot lifetime is bounded by the us-based ProgressWatchdog** (self-
  clocked, race factor varies per boot): observed deaths ~3 m 50 s to
  ~5 m 50 s from boot start. The engine's collection must land inside
  that. v13 = skip the ring-xfers after a successful CNXN (~3 min/cycle
  saved); v14 = settle 25 x 3 s (~75 s real) so a recovered boot's
  attempt starts ~+180 s; v13's ~130 s settle had lost the race by
  seconds (boot #15).
- **v15/v16 = the event-detection hole.** `last_event()` matched the
  "Product:" line; a product-less kernel attach (kernel strings never
  printed: dev 49 at 17:20, dev 53 at 17:26) returned the PREVIOUS
  attach's line -> CUR == EV -> the whole boot was skipped in silence
  (no snapshot, no log, no recovery). v15 triggers on the attach line
  itself ("new high-speed USB device number N" - unique per attach).
  v16 scopes the product search to THAT attach's device number
  (`-A8 "device number $DEVNO "`) - the unscoped tail-1 had mislabelled
  dev 53 as "loader attach" using the older LK line.
- **THE LK AUTO-BOOTS THE KERNEL AFTER ~80-100 s IDLE.** Observed: LK
  (dev 50) at 17:24:42 sat unflashed (engine swap window); at ~17:26 the
  kernel came up by itself (dev 51-53, shifted strings = its own boot).
  Operational rule: an LK window is SHORT - flash promptly or the loader
  continues into the old kernel on its own.
- **firmware-updater daemon interference**: `usbfs: USBDEVFS_CONTROL
  failed cmd fwupd` lines appear when fwupd probes our 18d1:d00d device
  mid-enumeration (rq 8 = GET_DESCRIPTOR). Harmless so far, but it is a
  third party poking the device during our windows - expect occasional
  extra control traffic in the journal.
- Engine generation at 17:30: **night_engine_v16.sh** (attach-line
  trigger + scoped classifier + v13/v14 behavior). Ledger dedupe fixed
  to a (device, time) key (host Device numbers got reused after the
  host-side USB renumber around 16:55; dev-only keys silently skipped
  fresh boots).


### Entry 046 addendum 7 — ring-window semantics, the v208 experiment, v210 (17:30-18:10)

- **The CNXN piggyback is the LAST min(used,2048) bytes of the ring**
  (`ane_usb_dev.rs`: `let take = min(used, 2048); let start = used - take;`
  then a raw copy from TEXT_BLOCK_BASE+start). Consequences:
  - The collected 2049-byte files are TAILS. The window slides forward as
    a boot writes more; a line's absence from a collection is NOT proof
    it was never written (it may have slid out). The launchd-line
    presence in the v206-era rings vs absence in later rings may be pure
    window slide - the full 32 KiB `oem xfer r 348d0000 8000` read is the
    only definitive source for "how far did the launchd get".
  - The ring content is stored/rendered UPPERCASE ("ud: q=" appears as
    "UD: Q="). Grep the collections case-insensitively or in uppercase.
- **v207 prepare_attach (EP0 cleanup + FIFO flush right before the
  attach) WEDGED the USB core**: the v208 boot never attached at all
  (no host device for 4+ min). Same signature as the v26-era "GRSTCTL
  write wedged the core" note. REVERTED; do not touch GRSTCTL before the
  host's first bus reset. The v206 flushes inside handle_reset (after
  resets) remain and are proven.
- **v208 (attach moved after the timer/GIC bring-up) is REVERTED in
  v210**: it did NOT stop the first-read -110s, and it lengthened the
  IRQ-less pre-attach window. The attach returns to the post-probe site.
- **The -110 hypothesis (v210 fix, test pending)**: the v206 EPENA
  guard's bounded wait was 0x2_0000 iterations of MMIO reads - on this
  slow bus that can be SECONDS, i.e., the guard itself could eat the
  host's 5 s timeout when the endpoint was genuinely stuck at the first
  read. v210 shortens both guard waits to 0x400 (a healthy completion
  clears EPENA in microseconds; a stuck one is force-disabled
  immediately). Watch the first-attempt quality in the ledger.
- **Kept from the v208 batch**: the launchd milestone MARKS (L0..L6) now
  re-stamp into the ring every 256 beats ("mark: ..." - uppercase in
  ring) with the deepest mark in LAST_MARK - the launchd-depth
  instrument. Reading it needs a collection window wide enough, or the
  full-ring xfer.


### Entry 046 addendum 8 - engine v17/v18: the missed-boot bug (18:00-18:15)

- The v16 classifier decided ~2 s after an attach event. When the
  enumeration was still in flight (retries, the -110 dance), it saw no
  product line and skipped the attach - **boot #22 (a CLEAN boot) was
  missed entirely**.
- v17: on a product-less attach, wait (bounded, 42 s) for THIS attach's
  product before deciding.
- v18 (current): the wait is 90 s (30 x 3 s) and the product search is
  `journalctl -k --since "@<epoch-8s>" | grep "usb 1-1: Product: "` -
  the old `grep -A8` window missed product lines pushed more than 8
  lines below the attach line by the -110/config error bursts (boot #26
  was skipped this way).
- Boot #26 (v210) also showed the self-heal reconnect did NOT fire
  (no re-attach between the garbled attach and the watchdog death) -
  worth a look next session (the SETUP-conditional sentinel may need
  the same start-conditioning as the enumeration sentinel).


### Entry 046 addendum 9 - engine v19/v20 + v210 samples (18:10-18:22)

- v19: empty event baseline at start (the in-flight boot at a restart
  gets processed). v20: the product-wait `--since` anchor = the ATTACH's
  own epoch (journalctl -o short-unix), not now-8s - boot #27 was
  falsely declared product-less when a 33 s restart gap hid its (attach
  + 6 s) product line. Both verified live (c15: product arrived late,
  boot #28 collected as rings_night/adb_1.bin).
- v210 samples so far: #26 garble (self-heal did NOT fire - investigate),
  #27 -110 -> CLEAN recovery on the SAME device (first time), #28 -110 ->
  CLEAN same-device + collected, #29 product-less failure (correct skip).
  The first-read -110 persists on every boot; the v210 guard shorten is
  not its cause. Same-device recovery now common.
- Full-ring read attempt on boot #28: `OKAY len: 32768` (responder
  accepted) then host-side EIO + cfg0 -110 (the boot died mid-read at
  ~18:17). A full-ring read must start within the first ~90 s of a boot
  to fit the lifetime. The 2 KB CNXN piggyback cannot reach the launchd
  era: the enumeration-retry Q= lines fill the window (adb_1.bin's tail
  = the repeated 060A/0000/0606 trio + SET_CONFIG(1)).
- The launchd-depth instrument (mark: re-stamps every 256 beats) is in
  the running kernel but invisible in 2 KB windows - the full-ring read
  is the only reader.


### Entry 046 addendum 10 - the launchd-entry stall's first wall fell (18:30-18:45)

THE FINDING: the "LAUNCHD: PRE -> WD STALLED" stall was TWO stacked walls,
and the first fell tonight:

1. **uart::putc on the ANE path = [ring tee] + [watchdog_tend()]**. The
   SP805 tend does a volatile READ at 0xe8a06000+4. The historical stall
   ELR sat "inside putc" (0x1BFD1A4/0x1BFD1C0), and the ring's last line
   was "launchd: pre t=" WITHOUT a newline: the main thread froze on the
   very next console byte - while the timer IRQ kept writing (its own
   SP805 feed is a write-only kick; a read is the one access class that
   can stall on this bus). An unbounded bus stall cannot be fixed by
   spin counts (the v39 dwmmc lesson) - the fix is to not issue the
   access: **v211 removes watchdog_tend() from putc on the ANE build**
   (env FULLERENE_ANE_NO_PUTC_WD_TEND=1 -> cfg fullerene_ane_no_putc_wd_tend).
   The SP805 stays fed by the timer IRQ (beats % 8), the USB path and the
   storage loops' own tend calls.
2. EVIDENCE: the first v211 collection (rings_night/adb_144.bin) contains
   "..LAUNCHD: PRE T=00000000" **followed by "LAUNCHD: CALLING RUN"** -
   the line that never before existed. The main thread now passes the
   putc and walks into the launchd block's next statement:
   `platform::ane_fb::event(b"LAUNCH RUN")` - the panel paint
   (paint_at + flush_row), the same class as the v145 freeze
   (Line::draw clear loop, ELR 0x1BE6C54).

3. **v212**: ane_fb::event() now (a) mirrors every ledger tag to the ring
   ("EVT: <tag>"), and (b) under FULLERENE_ANE_MARKS_UART_ONLY SKIPS the
   panel paint entirely. The next clean boot's ring will show whether the
   event call completes ("EVT: LAUNCH RUN") and whether launchd::run()'s
   first mark ("MARK: L0 RUN ENTERED") follows.

Build recipe delta: FULLERENE_ANE_NO_PUTC_WD_TEND=1 is now part of the
standard boot-image recipe alongside FULLERENE_ANE_MARKS_UART_ONLY=1.


### Entry 046 addendum 11 - THE BREAKTHROUGH: launchd::run() reached (18:44)

rings_night/adb_14.bin (boot #34, v212) - the first ring in this project's
history to contain:

- **EVT: LAUNCH RUN** - the ane_fb::event() call completed (the second
  wall after v211's putc fix).
- **MARK: L0 RUN ENTERED x21** - launchd::run() was ENTERED; the v208
  re-stamp instrument works (one-shot marks would have been invisible).
- **EVT: VFS OPEN** - the bootstrap's VFS is opening the image.
- **EVT: UD RST x4, EP1 DN, SENT1, NG 19 67657476, DEC OK, ADB C...** -
  the responder + ADB decode working while the launchd runs.

The old signature ("LAUNCHD: PRE -> WD STALLED", the ELR frozen inside
putc) is GONE. The boot now dies ~3 min in (the ProgressWatchdog) with
the L1 mark ("img read") NEVER printed: **the next wall is inside
launchd::run() between the L0 mark and the L1 mark - the launchd image
read (the VFS read path).** Next investigation: the fs read between L0
and L1 in launchd.rs and the storage path it drives.

v211 + v212 recipe: FULLERENE_ANE_MARKS_UART_ONLY=1
FULLERENE_ANE_NO_PUTC_WD_TEND=1 (+ the standard pic/pie/notext/lld).

Status ledger: the boot is now RECYCLABLE through this new wall (the
watchdog kills it, the LK reflashes, the loop continues) - no manual
presses anywhere in the chain.


### Entry 046 addendum 12 - the L0->L1 wall: frozen at the VFS resolve entry (18:50)

With v211/v212 the boot reaches launchd::run() and dies (watchdog) INSIDE
the first fs call: `fs::read_kernel_path("/bin/launchd", ...)` ->
`read_vfs_path` (arch/aarch64/fs.rs:7633). The ring (adb_14.bin) shows:

    MARK: L0 RUN ENTERED      <- run() entered
    VFS: OPEN BEGIN           <- read_vfs_path entry (line 7640)
    EVT: VFS OPEN             <- the ledger event (line 7644)
    VF<frozen>                <- 2 bytes of "vfs: resolve begin" (line 7652)

The main thread's byte stream stops there (verified by fragment search:
no "RESOLVE"/"VE BEGIN"/"PRE-SPLIT"/"IT MADE"/"COMP=" anywhere - not a
contiguity artifact). The IRQ continues: further marks (MARK: L0 re-stamps,
WD: BOOT PROGRESS STALLED) keep landing in the same ring, so the ring
write path demonstrably works from IRQ context.

Dead ends checked tonight: putc's ANE path = ring tee + atomics only (the
v211 SP805 read removed); text_byte = watchdog_note() (atomics), upper-
case, then plain stores at fixed addresses - no loops, no locks. The
freeze is main-thread-specific and happens BETWEEN two text_byte calls.

Open hypotheses for the next session (in order):
1. A sync exception (data abort) taken on one of the ring stores while
   IRQ writes still succeed - would need an MMU mapping gap for
   TEXT_BLOCK_BASE+12+size (block = 0x348d0000..0x348de00c, cap 0xe000;
   FIXED copy at +0x4000..+0x5c00; step-marks at 0x348df000). Check the
   MMU tables for this window (both copies, incl. past 0x348d8000 - the
   collection's 32 KiB read window ends there and the block does not).
2. The exception handler itself wedging when it tries to print (the
   exception prints go through the same funnel).
Next instruments if needed:
- v213: non-text step markers at StepMarks (0x348df000), read back with a
  tiny xfer (4-64 B - likely EIO-safe) instead of the ring.
- Check whether the FIXED copy (0x348d4000) contains content PAST "VF"
  (the two copies diverge if the main ring's header-size read misbehaves).


### Entry 046 addendum 13 - SMK result, the xfer root cause, the 84 s cycle (19:00-19:15)

**The VFS freeze point is now pinned by a non-text instrument.**
v213 put step markers in read_vfs_path; v215 moved them to a PRIVATE trail
at 0x348de800 (the shared step-mark block 0x348df000 fills with the boot's
own stage letters - '1','9','q','r','s','e','a','h','f','k','l','m','n',
'o','p' - before the VFS ever runs; the first SMK read proved it). v214/
v215 dump the private trail into the ring as "SMK:<hex>" every 256 beats,
so the normal CNXN collection carries it (a separate fetch would be
conversation #2 and race the magic reset - measured: the v22 40 s marks
fetch died with no OKAY).

Result (rings_night/adb_24.bin): **SMK trail = "12"** - '1' (fn entry),
'2' (before event("VFS OPEN")), and NOTHING more. The freeze sits between
vfs_step('2') and vfs_step('3'): the event's ring mirror completes
("EVT: VFS OPEN" is present), then the very next construct is
`with_vfs(|vfs| ...)` -> `vfs.lock()`. The lock-holder hypothesis is now
the prime suspect (a spin lock held elsewhere = a silent main-thread
freeze with the IRQ alive, exactly the signature).

**The `oem xfer` root cause (why streams never worked).**
`ep1_send` arms DIEPTSIZ with the FULL packet count, then the fill loop
writes ALL bytes into the EP FIFO with no free-space gating and no
completion wait. For a reply larger than the TX FIFO's depth the overrun
writes wedge EP1 (the v172 note: "asking for more than is present once
wedged the endpoint"). The CNXN piggyback (2049 B) fits; the xfer streams
do not. FIX (v216): chunked fill gated on DTXFSTS free words + a
completion-aware loop, then the fb/screenshot channel and the fast bulk
ring read both become possible.

**The fast cycle works (v23).** Measured from the engine log: attach
19:00:57 -> CNXN collection 19:01:34 (37 s!) -> magic reset -> LK ->
flashed+rebooted 19:02:21. **84 seconds attach-to-next-flash** (the old
cycles ran 4-6 min). The v23 clean-state poll + 9 s grace replaced the
fixed 45 s settle; the v17-21 classification chain + the descriptor
fallback keep the collection first-try on clean boots.

**User directives (19:05-19:10): make enumeration/ADB/fastboot as reliable
as possible and speed the dev cycle; make fastboot + screen screenshots
usable; the whole screen area is available for captures.** Plan:
A. ep1_send chunked fill (v216) -> working xfer -> fb dumps + full ring reads.
B. Screenshot channel: read the canvas (0x31000000, 1080x2340x4) in
   regions over the fixed xfer, render host-side to PNG (no camera).
C. The VFS-lock holder hunt (the freeze's prime suspect).
D. Keep the engine chain fast (Settle ~9 s grace after clean).


### Entry 046 addendum 14 - the xfer works (v216, 19:25)

The v216 ep1_send fix (DTXFSTS-gated chunked fill + an arming wait for a
pending transfer) made the first successful `oem xfer` of the current era:

    sent: oem xfer r 348d0000 800
    OKAY len: 2048
    wrote 2048 bytes    (real 18.6 s ~ 110 B/s)

Content is byte-exact: "FTLX" magic, size 0xE000, then the ring's base -
the boot's very first log lines ("FULLERENE: ENTERED ... BOOT: X0=...").

Historical root cause (now fixed): the old blind fill wrote all `n` bytes
regardless of the TX FIFO's free space; any reply larger than the depth
overran the FIFO and wedged EP1 forever (the v172 lesson). The CNXN
piggyback (2049 B) fit, so collections lived while every big read died.

Test method (repeatable): SIGSTOP the engine on a fresh clean attach
(kill -STOP <pid>), run the xfer as the sole conversation, SIGCONT.
The paused engine keeps its state; its product-wait loop is a safe pause
point (no conversation in flight).

KNOWN LIMIT: 110 B/s (2 KB in 18.6 s) - the xfer sender still paces on
the CNXN-style per-chunk FIFO-full-drain waits. v217 plan: rework the
xfer sender to back-to-back 4 KB ep1_send chunks (the v216 path is
multi-packet safe now: arm once with the full packet count, gated fill,
no per-chunk re-arm) -> unlocks the framebuffer screenshot (full screen
1080x2340x4) and fast full-ring reads.


### Entry 046 addendum 15 - the DWC2 IN multi-transfer wall (v217-v219, 19:30-19:45)

The v216 fix (gated fill + arming wait) made the FIRST successful `oem
xfer` of the era: `r 348d0000 800` -> OKAY, 2048 bytes byte-exact, 18.6 s
(~110 B/s). The 512 B chunked path works; the speed is the limit.

The speed attempts and what they measured:
- v217 (one 128 KiB arm; fill straight from RAM): "OKAY len: 131072"
  then a 4-minute stall with NO "WD: EP1 FILL" marker. The fill's
  re-reading loop (free-words per round) is unsafe: DTXFSTS is not
  trustworthy on this bus (v176), a stale "free" reading sends the fill
  writing into a full FIFO, and the AHB write then stalls forever.
- v218 (12 KiB windowed arms, ONE free-read per arm, fill capped):
  "OKAY len: 32768" then a 150 s timeout - the second arm never
  completed. The DWC2 IN-transfer completion/re-arm semantics across
  sequential multi-transfer arms are NOT what the current code assumes
  (the arming wait burns its bound, later arms drop).
- v219: REVERTED to the proven 512 B chunked path (with the v216 gated
  fill and the arming wait kept). The next bulk attempt must begin by
  re-reading the vendor gadget.c (hi6250) IN-endpoint re-arm/complete
  pattern - source first, per the working discipline.

Screenshot pipeline status (user directive: whole screen allowed):
- The path is proven end-to-end EXCEPT the speed: `oem xfer r <fb> <len>`
  reads any RAM (the canvas = 0x31000000, 1080x2340x4) and render_fb.py
  turns raw bytes into PNG without deps.
- At ~110 B/s a full screen (10.1 MB) needs ~26 hours; even a 100 KB
  strip needs ~15 min. The screenshot becomes practical once the bulk
  IN protocol is understood - that is the next big rock, together with
  the tight-refill pattern.
- Operating recipe for xfer tests: SIGSTOP the engine on a fresh clean
  attach (kill -STOP <pid>), run the xfer as the sole conversation, then
  kill -CONT. The product-wait loop is a safe pause point.


### Entry 046 addendum 16 - the vendor-pattern xfer pump + the EOVERFLOW wall

Source work: re-read the vendor gadget.c (hi6250-src drivers/usb/dwc2/
gadget.c, s3c_hsotg_write_fifo, lines 315-471). Confirmed:
- NON-periodic writes are capped at 512 B per call, with the explicit
  warning that >512 B requests "cause the endpoint to get stuck with a
  fragment of the end of the transfer in it" (401-407). This is the
  documented root of the v217/v218 stalls.
- For dedicated FIFOs (our case) the vendor reads DTXFSTS for can_write,
  arms the transfer once, and feeds the remaining bytes from the
  IN-complete/FIFO-space interrupt path.

Implementation (v220/v221, in ane_usb_dev.rs):
- Stream statics XFER_ON/XFER_FEED/XFER_END/XFER_WIN_END + `xfer_pump()`:
  arm bounded 64 KiB windows (PKTCNT=ceil(win/512), only when EPENA is
  clear), feed <=512 B chunks per DTXFSTS read, max 16 chunks/pass;
  re-arm when FEED reaches WIN_END; "XF: DONE" when fully fed.
- `oem xfer r` handler starts the pump instead of streaming inline; the
  pump is hooked into service() every pass. v221 adds a double-read hedge
  on DTXFSTS (stale-low reading would starve the wire) + "XF: STALL".

Test results (32 KiB fb read, SIGSTOP recipe, fresh clean boots):
- The reply header arrives promptly ("OKAY len: 32768"), so the command
  path works; the payload then fails with the HOST-side
  "[Errno 75] Overflow" (EOVERFLOW) and never completes (120-200 s).
- Same with 128 KiB. The device-side markers are not readable in these
  sole-conversation boots (ring uncollected).

Next-session suspects, in order:
1. The IN-complete path already exists in service_ep1 (EPINT_XFERCOMPL ->
   EP1_IN_IDLE=true, comment: "the stream may send the next one") but the
   pump is NOT hooked there, and the vendor's per-packet refill trigger
   (the non-periodic TX FIFO empty interrupt, GINTSTS_NPTXFEMP, enabled
   in write_fifo) is not used at all. Wire xfer_pump() into service_ep1
   and/or enable+handle the FIFO-space interrupt.
2. The host EOVERFLOW itself: pyusb read size / usbfs behavior; check the
   interface state on a boot where the descriptors are known clean, and
   try smaller reads (e.g. 4096) with a tighter loop.
3. fb_xfer's error path sleeps 50 ms per error and can thrash; make the
   error handling rate-limited and the read size adaptive.

The pump is dormant unless an `oem xfer` is commanded: all normal flows
(CNXN collections, engine cycles) verified working with v220/v221 images.


### Entry 046 addendum 17 - the pump test: stream stops after ~56 bytes (20:14)

Test (v222 image + the improved host tool: 4 KiB reads, EOVERFLOW-tolerant,
progress prints): `oem xfer r 31000000 8000` on a fresh clean boot,
engine SIGSTOPped.

Observed:
    OKAY len: 32768
    overflow x 1  got 56        <- then silence: reads all time out
    (150 s timeout, no completion)

So: the command path and the reply header work; exactly ~56 bytes of
payload arrive; the host's subsequent reads time out (the one EOVERFLOW is
followed by silence, not a stream). The device-side feed stops after the
first packet or two - the pump is not sustaining the stream.

Open questions for the next session, with the test design that answers
them:
1. Is the pump running at all during a sole-conversation boot? The pump
   counters live in the ring, but sole-conversation boots are never
   collected. **Change the test design**: run a normal engine cycle first
   (CNXN collection = conversation #1), but with a NON-magic arg0 so the
   deliberate reset is NOT scheduled (the responder resets only on
   arg0 0xC0DE0001) - the boot then stays alive, the ring (with XF:
   markers) is in hand, and the xfer can run as conversation #2. Trigger
   the reset afterwards.
2. The device-side feed cadence: verify that the timer-IRQ service passes
   actually call xfer_pump during the epoch (add a counter if needed).
3. The FIFO-space interrupt (GINTSTS_NPTXFEMP, enabled by the vendor in
   write_fifo) remains unhooked - the vendor's per-packet refill trigger.
4. The 56-byte stop is consistent with: the arm happened, a packet or two
   left, then the feeding gated forever (DTXFSTS or the pass cadence).

No regression: the pump is dormant without an `oem xfer`; the engine's
collections keep working on v222.


### Entry 046 addendum 18 - the PC instrument: 12 boots, stationary PCs (21:20)

v224 added LAST_IRQ_PC (the interrupted PC of the most recent IRQ, stored at
the IRQ's top, printed by the 256-beat re-stamp as "PC:<16 hex>"). v226 added
the VFS static's identity probes (VA at init, VB at the read site) and the 'N'
probe on the with_vfs None branch.

The 12 most recent v226 collections (20:53-21:0x) all show the same shape:
within one boot the PC line is printed 2-6 times and is EXACTLY CONSTANT;
across boots every value is different (0x1BED1D4, 0x1C080E8, 0x1BEACB0,
0x1BEAE88, 0x1BEADE4 x3, 0x1BEAE48, 0x1BED41C, 0x1BEAE04, 0x1C08574, ...).
Mapped with base 0x1660000 (the image load; NOT 0x480000 - the old pc_lookup
base was wrong), the values land in elf 0x58ACB0..0x5A8574. That is the
blackbox/print module and its neighbours: 10 of 12 values sit within ~10KB
of text_byte itself (0x58ADE4..0x58AE94).

Disassembly of the two repeated stops:
- 0x58AE04 (text_byte+0x20) = the cbnz of the LL/SC retry loop of the
  inlined watchdog fetch_add (ldxr/add/stxr/cbnz at 58adf8..58ae04).
- 0x58AE88 (text_byte+0xa4) = the final ring append store (strb to
  0x348d000c+size, then size+1, then ret at 58ae94).
Both are "about to execute" points: the thread never advances past them.

Reading: a stationary thread whose stop point varies per boot, always in or
beside the console/print path. The VA==VB result (same static address
0x36F7CA0 both sides) rules out a duplicate-static bug, and no 'N' in the
trail means the with_vfs None branch was never taken after the read wipe, and
no 'l' means the blocking spin::Mutex acquire was never reached from the read
- consistent with the freeze sitting inside the console writes around the
event() call, not in the VFS lock.

Open candidates (next session):
1. The D-cache-off LL/SC pathology: the ANE runs with SCTLR.C=0; if the
   exclusive monitor does not complete for normal memory with the cache
   off, stxr fails forever. But then every early spinlock would hang too,
   and the boot works up to launchd - so if real, it must be timing-window
   specific (e.g. only after the timer IRQ starts clearing the monitor
   every tick, with the IRQ itself storing to the same cacheline granule).
2. A stalled store to the ring/SRAM on the final strb (the +0xa4 sample).
3. Distinguish "stationary" from "slow": print a second PC line right at
   the IRQ exit if the same value repeats (cheap, no new state).

Next experiments (cheap, no new asm):
- Wider PC histogram: keep the 256-beat print in more boots; chart the
  per-boot constant values against the boot's other features (warm/cold,
  USB state) to find the timing variable.
- v227 candidate: replace ProgressWatchdog::note()'s fetch_add with a plain
  load/store (single-core; the counter is only compared for inequality) to
  remove the LL/SC loop from the console path entirely. If the freeze then
  moves elsewhere, the atomic theory dies with one build; if it vanishes,
  the console hot path is healed.

Also tonight: v223 added the 'e' probe (after the event, before with_vfs);
v225 added the 'l'/'L' probes around the blocking spin::Mutex acquire and
blackbox::step_probe as a shared helper; v226 added 'B' after the VFS
assignment, VA/VB address probes and the 'N' None-branch probe.


### Entry 046 addendum 19 - the desktop port lands in the aarch64 kernel (v229, 22:03)

The desktop stack (petroleum/nitrogen/solvent) already built for
aarch64; v229 wires it into the ANE boot. New pieces:

- `arch/aarch64/desktop.rs` (`#[cfg(fullerene_aarch64_ane)] mod desktop;`
  in the aarch64 runtime root): the ANE counterpart of `gui::init` +
  `gui::render`. It
  1. installs the frame-allocator adapter (`AneFrameAllocator` over
     `PhysicalFrameAllocator::allocate_contiguous_frames`, a new method -
     the single-frame walk of `next_frame` cannot express the desktop's
     10 MiB back buffer run),
  2. `solvent::set_tsc_per_ms(1000)` + `set_render_progress_fn` (render
     progress lines go to the durable ring), `solvent::init()`,
     `apply_settings(1.0, 100, true)`, `tick_core(now_tsc())`,
  3. builds a `FramebufferGuard::try_new()` over the canvas
     (0x31000000, 1080x2340, stride 1080) and calls `solvent::render`,
  4. `ane_fb::flush_panel()` to blit the canvas to the display windows.
  It runs inside the launchd block (`if allocator_ready`) BEFORE
  `launchd::run()`, so the desktop paints even though the launchd's VFS
  work parks.

- `ane_fb::DESKTOP_OWNED` (v229): the boot instruments
  (`report`/`paint`/`clear`/`paint_at`/`draw_fb_diag`) return early once
  the desktop owns the canvas, so their text rows cannot scribble over
  the desktop pixels. Ring/durable logs and the flush helpers are
  unaffected (the flushes copy whatever the canvas holds - they now
  propagate the desktop).

- `exceptions.rs` timer IRQ: `solvent::ANE_TICKS.store(uptime_us/1000)`
  next to `TICK_NOW.store` - the desktop's monotonic clock (nominal ms).

Two build traps hit while linking v229:

1. **R_AARCH64_ADR_PREL_LO21 out of range for `__rela_dyn_start`.**
   `entry.rs`'s relocation applier used `adr` (PC-relative, +-1 MiB).
   In v228 the distance from `.text` (0x582000) to `.rela.dyn`
   (0x63d960) was ~780 KiB; the desktop's code/data pushed it past
   1 MiB and the link failed. Fixed with `adrp` + `add :lo12:` in both
   `entry.rs` (+-4 GiB range; still PC-relative and relocate-safe) and
   `usb_probe.rs`'s copy of the same sequence.

2. **The image no longer fit the stock entry window.**
   Linking solvent pulled all five wallpaper presets into `.data`
   (FULLERENE_PIXELS 6 MiB + SHARP_PIXELS 6 MiB + three ~0.5 MiB
   presets). Payload 15,309,349 B vs the stock window
   (34,625,536 - 0x1660000 = 11,229,184 B). Trim: on aarch64
   `lattice::wallpaper` keeps the default preset (index 3 "fullerene")
   and swaps "fullerene-sharp" for a 1x1 stub, so preset indices stay
   identical across platforms and persisted wallpaper modes keep
   resolving. Payload 9,017,893 B; artifact 11,593,728 B (fits).

Build/env unchanged (pic+pie+notext, rust-lld, manual-ane-launchd target
dir, MARKS_UART_ONLY + NO_PUTC_WD_TEND). x86 targets untouched: the
lattice change is cfg(target_arch="aarch64") only and the desktop module
is gated to the ANE cfg.


### Entry 046 addendum 20 - the desktop lands, crawls, and the v234 regression (night 10-07 -> 10-08)

The desktop wiring (v229) reached the device and the whole stack runs on the
ANE: `arch/aarch64/desktop.rs` takes the canvas at 0x31000000, installs the
PageBuf frame-allocator adapter, calls solvent::init/apply_settings/tick_core,
builds a FramebufferGuard over the canvas and renders one frame, then
`ane_fb::flush_panel()` blits it to the display windows. `ANE_TICKS` is fed
from the timer IRQ; `ane_fb::DESKTOP_OWNED` stops the boot instruments.

What the rings then showed (v229..v233, adb_14/79/118/142/37):

* `DESKTOP: BRING-UP` and `PR: FILL START` print, then the boot thread parks
  with the PC pinned inside the first big heap fill (bring_up+~0x21x-0x45x).
  The PC trail (v230 instrument, 8 deep) repeats one address - the thread
  advances ~one instruction per IRQ. It is a crawl, not a fault: ESR/FAR/SPSR
  stay 0, IRQs keep running (restamps continue, collections work).
* The first substantial write to freshly-allocated heap memory is what crawls.
  Small allocations work; the 24 KiB fill needs minutes. Suspects investigated:
  - cache staleness after the loader handover (v232 added a set/way
    `dc cisw` invalidate modeled on the Linux !invalidate_dcache_all`);
  - IRQ budget (the timer handler copies canvas bands and can call a full
    9.6 MiB flush_panel; first-round IRQ_QUIET gates (v233) cover the paint
    entry points only, not the direct flush_row/flush_panel call sites).

The v234 regression (0/8 draws, pre-USB deaths):

* v234 (getter + flush gates + FA probe) and v235/v236/v237 (= v233 plus the
  getter and the two `flush_row`/`flush_panel` gates `irq_quiet() &&
  in_irq_context()`) never reached USB: flash + reboot, then the loader came
  back in ~30 s, eight draws straight. The two gates are inert by construction
  (irq_quiet is false until bring_up), so the empirical correlation is
  unexplained - recorded honestly as such.
* v233 and the restored v238 (= v233 floor plus the tick-budget raise) boot
  fine, so the night's working floor is v238.
* During the hunt the loader also wedged in a fastboot-only loop after a bad
  v234 boot; flashing the stock kernel (firmware/ane_bootimg/repacked_stock.img)
  once and letting Android come up (a "good boot") restored kernel boots, and
  v228 (no desktop) always booted - which is what isolated the regression to
  the v234+ builds.

Also fixed tonight:

* `dc cisw` invalidate (v232) parked under audit in v237+ - kept out of v238.
* Tick budget raised 16384 -> 0x200000 beats (v236): under attach churn the
  beat count races and the old cap landed in ~25-35 s real, cutting boots
  down mid-dance.
* PageBuf std/test path wrote 4x its length (`write_bytes` with an element
  count) - fixed in petroleum; the host desktop-render test (1080x2340)
  caught it and now passes 29/29 in solvent.
* The link no longer fits ADR range: entry.rs/usb_probe.rs relocation
  prologues use adrp+add (R_AARCH64_ADR_PREL_LO21 went out of range when the
  desktop code/data grew).

Open at the time of writing: why fresh-line heap fills crawl (next lever: the
IRQ canvases, then the cache state), and whether re-adding the flush gates on
a fresh layout is safe. The engine runs v238 while this is investigated.


### Entry 046 addendum 20 — v255-v259 boot UI/USB-first A/B (2026-10-08 JST)

The phone was kept on the literal loader descriptor before each flash:
`Fastboot2.0`, VID:PID `18d1:d00d`, serial `SCV7N18927000473`. The stock-embed
artifacts passed the mechanical ELF/header/package checks and were flashed with
the standard `ane_cycle.sh` path. No screen-based conclusion was used.

#### v255: full boot panel before the USB responder

After `Writing 'kernel' OKAY`, the loader disconnected at 12:11:44. The host
then saw repeated descriptor failures (`-71`, `-110`, and `-62`) and never saw
`Product: ANE fastboot` within the 120-second cycle. The device did not return
to a usable loader window automatically during the bounded recovery monitor.

Source inspection showed that the new full-screen `ane_fb::report()` ran before
`ane_usb_dev::init()` and `usb_attach()`. This made the panel paint a credible
candidate for stealing the first descriptor window, so the next A/B moved the
report after attach.

#### v256/v257: attach-first and enumeration foreground pump

v256 moved the report after `usb_attach()`. The host still failed at the first
device descriptor (`-71`/`-110`/`-62`) and the phone returned to `Fastboot2.0`
at 12:30:05. v257 added a bounded two-second `service()` pump until
`USB_ENUMERATED`; the same descriptor failure remained (first retry at
12:31:44). Therefore the panel alone is not the complete cause: the failure
occurs before `ENUMDONE`, and the v256/v257 changes did not restore the wire.

#### v258/v259: gated panel and pre-GIC ordering rollback

v258 was built with the panel skipped unless `ENUMDONE` was already latched.
v259 additionally removed the pre-GIC foreground polling loop and retained only
the proven single service pass. Both artifacts passed the mechanical package
checks. They were not flashed after the v257 failure; no claim is made about
their device behavior.

The working next hypothesis is now the v253+ USB/boot ordering or controller
state, not merely the diagnostic UI. The v253-v254 `desktop`/re-enumeration
changes must be compared against the v238/v252 floor on the exact EP0 attach,
reset, and timer-service path. Do not continue UI A/Bs until that comparison
produces a source-grounded change.

#### Recovery state

The known v252 stock artifact was flashed at 12:38:19 and returned `OKAY`, but
its subsequent reboot was not observed as `ANE fastboot`; the loader then had
to be entered manually. The final verified state for this session is the
loader, product `Fastboot2.0`, serial `SCV7N18927000473`, held without another
reboot or flash.

#### v260: autonomous USB recovery and no-button cycle

v260 adds an explicit `ATTACHED` latch after the deferred `usb_attach()` and
lets the timer-side USB recovery state machine run without waiting for
`SETUP_SEEN`. This covers the first-descriptor failure where the host is
connected but no setup packet reaches the responder. While
`SET_CONFIGURATION(1)` has never completed, the timer drops and raises the
D+ pull-up every 1024 beats, up to six draws. If none reaches configuration 1,
the existing panic path writes the BOOTLOADER PMIC reason and resets through
LPM3 NMI/SP805 into `Fastboot2.0`; a healthy configuration stops the active
re-enumeration and leaves only the normal USB service pump. No framebuffer
paint is added to this path, so Desktop ownership remains intact.

The v260 AArch64 build used PIC/PIE, user-launchd, UART-only marks, and
`FULLERENE_ANE_NO_PUTC_WD_TEND=1`. Host tests passed 29 + 7 + 10 + 2. The
packaged artifact passed the corrected stock-embed validator and has SHA-256
`857c9df195737d46767ca34c29c7445f37afca736b9892217bb1c339088443f6`:
`/home/placeless/dev/p20-root/artifacts/fullerene-ane-v260-autoreboot-usb-reenum-stock.img`.

At 12:46:49 the unattended `night_engine_v24.sh` flashed this artifact and
reported `Sending`, `Writing`, and `Rebooting` all `OKAY`. At 12:47:16 the
host observed a new high-speed USB attach; the engine remained running and
entered its product-wait path without asking for a button or COMBO. Further
device success remains subject to the live unattended cycle.

Linux mainline `drivers/usb/dwc2/gadget.c` (`dwc2_hsotg_pullup()`) handles
gadget disconnect through `dwc2_hsotg_core_disconnect()` plus gadget
disconnect, and reconnect through `dwc2_hsotg_core_init_disconnected()` then
`dwc2_hsotg_core_connect()`. The ANE path deliberately does not copy that
full sequence: prior ANE experiments showed that GRSTCTL/full USB
reinitialization can make the device disappear permanently. v260 therefore
retains the empirically safe DCTL.SftDiscon-only pulse; this is a known
source-level difference, not an assumed equivalence.

The first live timing confirms the timer-side operation: host device 44 at
12:47:16, device 48 at 12:49:44, and device 52 at 12:52:12, approximately
148 seconds apart, matching the 1024-beat reconnect interval. During that initial
sample the descriptor failures were `-71/-110/-62` and no product string had
appeared yet; a later retry did reach `Product: ANE fastboot` at 13:06:58, but
its first three ADB attempts returned `no device` and no loader handoff followed.
The kernel remains within the bounded six-draw recovery window; the expected
no-button escape is `WD: NO ENUM` panic followed by the running engine's
Fastboot2.0 catch.

v261 resets `CONFIGURED`, `CONFIG_VALUE`, and sticky `CONFIG_WAS_1` explicitly
from `ane_usb_dev::init()` instead of relying on warm-handoff BSS contents.
The v261 AArch64 build, host tests, stock embedding, and validator all passed;
artifact SHA-256 is
`03af6fd6c083aeddf55e91c7adda7c1631c66a4c4172a552f3e8a260538abc3d`.

The unattended engine was then extended so a live `ANE fastboot` product window
uses `fastboot reboot-bootloader` and immediately flashes the queued artifact
after `Fastboot2.0` appears. This closes the previously observed gap where an
ANE fastboot window was visible but ADB failed and the engine only waited for a
loader. The v261 engine is currently running as process `proc_be7d6f0dad8a`;
v261 has not yet been flashed because the current v260 boot has not produced a
loader window after the 13:06:58 clean enumeration.
