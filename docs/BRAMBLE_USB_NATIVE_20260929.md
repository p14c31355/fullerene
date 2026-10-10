# Native USB investigation, 2026-09-29

Native Fullerene enumeration **has not succeeded**. Six RAM-only Fastboot boots
were captured in this session. No Android/configfs identity is counted as native
progress. All six recovered through stock Android and the harness restored the
selected phone to Fastboot. No partition writes were performed.

## Verified boundary

The host detects a high-speed attachment and submits the first address-zero
Device Descriptor request (`wLength=64`). No descriptor payload is returned.
There is **no verified successful native control transaction**, SET_ADDRESS,
SET_CONFIGURATION, or stable configured device. The last positive USB observation
is host-side attachment, not a device-side Connect Done record.

The pulse-free control produces an approximately five-second timeout (kernel
`-110`; cancelled usbmon URB `-2`) followed by three zero-payload `-71` completions.
These errnos do not distinguish missing SETUP reception, DMA/event delivery,
blocked MMIO, or a failed response. The earlier notes' claims that RX is dead,
or that SETUP reached software, are not established by these runs. Native EP0
attribution still requires an independent, same-boot device-side trace.

## Initialization chain checked in the current source

The standalone `usb_probe_entry` installs DT resources, handles watchdogs and
clears the DMA region. The direct `init_usb2_handoff` requests regulator power,
GDSC and performance/interconnect votes, then enters
`init_usb2_gadget_handoff` -> `init_usb2_gadget_reuse_fastboot_ep0`.

The selected path prepares the Qualcomm session/PHY interface and stops the
inherited controller; refreshes HS-PHY rails and reference clock; performs PHY
reset/analog initialization **before** DWC3 device soft reset; enables the UTMI
clock and restores controller interface/refclock settings; establishes the DMA
policy; publishes the event ring; allocates/configures EP0; enables device events;
starts Run/Stop; then arms SETUP and polls control traffic. `--start-after-connect`
defers the first SETUP arm. USB Reset and Connect Done have separate handlers.

The tested USB2 path does not initialize the QMP SuperSpeed PHY. The standalone
gadget profile skips initial SPMI role setup; optional Type-C IRQ routes are
separate experiments. The existing Type-C deferred handler reads the child state,
applies a role change, acknowledges the PMIC and reenables its parent SPI.
No new IRQ masking or Type-C policy was introduced here. These reads establish
source order, **not successful hardware execution of each step**.

The control uses `--no-smmu`, so it assumes a firmware DMA mapping compatible with
physical addresses; that assumption is not verified by an attachment. A separate
run enabled Fullerene's existing SMMU configuration and did not move the host
boundary. Linker DMA objects are aligned; TRB publication includes `dsb st`,
cache helpers include `dsb sy`, and the explicit cache-maintenance run enabled
clean/invalidate operations. The standalone probe does not enable the normal
kernel MMU. No DMA address, cache visibility or PHY register value was read back
independently from this handset in this session.

Graph discovery used Tier 2. Coverage metadata was unavailable for the relevant
Rust files, so material functions were read directly. This is a bounded review,
not a completeness audit of every experimental build flag.

## Changes

Pre-existing changes to `ep0.rs` and `usb_protocol.rs` were preserved and completed;
the incoming patch is saved in `tmp/usb-native-20260929/pre-existing.patch`.

- Removed an unconditional `pullup_mark(2)` in the normal handoff, before DCFG,
  core reset and EP0 construction. It toggled Run/Stop without a diagnostic flag.
- EP0 now distinguishes event LST/IOC flags from TRB result status, handles
  SETUP_PENDING without committing interrupted address/configuration state, waits
  for STATUS XferNotReady, and avoids republishing an active STATUS TRB.
- Polled SETUP requires a retired SETUP TRB; a delayed ring completion is consumed
  once without completing the new data/status phase. Real SETUP events do not
  require the polling-only residual/type predicate.
- Kept the incoming early DCFG SET_ADDRESS write and software commit after status;
  repaired stall/rearm, external trace-response lifecycle and physical endpoint
  number conversion for halt commands.
- Added one-shot RAM witnesses before DCTL (`BDCT`), before event-count (`BCNT`,
  including returned DCTL), and after event-count (`ACNT`, including count).
  `TRBC` records raw endpoint/TRB control/TRB size/event before reuse. These add
  no USB pulses or extra MMIO reads. They have **not been retrieved from hardware**.
- Added `contrib/debug/decode_bramble_usb_trace.py` for an offline debugger RAM
  dump. It validates the header and circular sequence numbers; it cannot prove
  dump freshness or boot ownership.

EP0 behavior was checked against Linux's
[actual DWC3 EP0 implementation](https://raw.githubusercontent.com/torvalds/linux/master/drivers/usb/dwc3/ep0.c)
and the locally retained qpr1 `drivers/usb/dwc3/ep0.c`. This supports the STATUS,
SETUP_PENDING, SET_ADDRESS and endpoint-direction handling; it does not establish
the cause of the pre-descriptor hardware failure.

## Controlled experiments

Artifacts are under `tmp/fullerene-bramble-loop.<run>.0/`, including image hashes,
build command/environment, source diff, boot acceptance, kernel logs and usbmon.
The compact machine-readable ledger is `tmp/usb-native-20260929/run-summary.json`.

| Run | Variable / hypothesis | Result and conclusion |
| --- | --- | --- |
| 91463 | Incoming code baseline | HS attach, three zero-byte `-71` descriptor completions; no native identity |
| 96074 | Repair EP0 completion lifecycle | Same host boundary; EP0 changes alone did not restore first descriptor |
| 98841 | Remove premature Run/Stop pulses | HS attach retained; first completion changed to timeout/cancellation, then three `-71`; diagnostic pulse affected the observed failure |
| 102547 | Explicit DMA clean/invalidate | Same timeout/zero-byte boundary; this cache policy alone is not a fix |
| 107185 | Add non-pulsing RAM witnesses | Same boundary; trace instrumentation builds/boots but requires another transport to read |
| 109929 | Enable Fullerene SMMU setup | Same timeout/zero-byte boundary; no evidence that enabling this path alone restores DMA/EP0 |

Each run's QEMU preflight, Android boot-image audit and `fastboot boot` acceptance
passed. Android fallback reported `watchdog` where captured; that does not identify
which Fullerene instruction or controller state preceded recovery.

## Verification

- 27 host USB tests passed: protocol, modeled DWC3 registers and modeled Linux
  host enumeration. Includes interrupted control requests and TRB polling guards.
- Explicit AArch64 QEMU USB simulation passed through SET_CONFIGURATION.
- `cargo check -p fullerene-kernel --target x86_64-unknown-uefi` passed.
- Changed Rust files pass rustfmt; `git diff --check` passes.
- Offline trace decoder checks passed for ordinary, circular-buffer and u32
  sequence wrap, and invalid input.

These tests exercise shared protocol/models; they do not emulate the Qualcomm PHY
or execute the real MMIO EP0 wrapper on host hardware. Existing build warnings
remain. No end-to-end x86 runtime test was performed.

## Ready image and next external measurement

`tmp/usb-native-20260929/native-usb-ready.img` is built from the final source,
with direct-only, pulse-free USB2 handoff, HS-PHY-before-core-reset and no-SMMU.
Its build log/environment/hash are beside it. SHA-256:
`f56bf4878b848c154ba174e8ffeadd800a634b3e610e6ba254002713d84cb9b9`.

The highest-value next experiment is a **same-boot RAM/register capture during the
first descriptor timeout**, through a working external debug connection. No such
connection is accessible in this session; host usbmon alone cannot supply it.
Connect that debugger, retain the USB cable, and start the host captures in
separate terminals from the repository root:

```sh
journalctl -kf -o short-monotonic > tmp/usb-native-20260929/host-kernel.log
```

```sh
tshark -i usbmon0 -a duration:90 \
  -w tmp/usb-native-20260929/host-usbmon.pcapng
```

Then boot only this RAM image:

```sh
fastboot -s 26191JECB00076 boot tmp/usb-native-20260929/native-usb-ready.img
```

For this exact ELF, `.usb_trace` is `0x90007000..0x9000a000` (12 KiB), as verified
with `aarch64-linux-gnu-readelf -SW`. Capture it **during this Fullerene boot,
before watchdog recovery/Android**. Also capture DWC3 GCTL, DCFG, DCTL, DSTS,
GEVNTADRLO/HI0, GEVNTSIZ0, GEVNTCOUNT0, DEPCMD0/1 and the event/TRB buffers at the
addresses recorded in the trace. A debug read of a stalled MMIO window can itself
fail; collect RAM first. Decode the RAM dump with:

```sh
python3 contrib/debug/decode_bramble_usb_trace.py usb-trace.bin
```

Correlate with `journalctl -kf` and passive usbmon. This distinguishes a DCTL/count
access that never returns, nonzero count with missing DMA, USB Reset/Connect Done
without SETUP, and a retired SETUP followed by a failed response. A wire analyzer
can establish bus packets/electrical behavior, but does not replace the RAM trace
for proving what Fullerene serviced. Until this evidence exists, root cause is
unproven and native enumeration remains incomplete.

## Follow-up measurements, same evening (HEAD 83f2595e)

Four more RAM-only boots, same attach-reaching profile
(`--direct-handoff --start-after-connect --no-smmu --hsphy-source-exact --refresh-hsphy-power
--hsphy-before-reset --enum-timeout 60 --hold 30`). Entries appended to
`tmp/usb-native-20260929/run-summary.json`.

| Run | Variable / hypothesis | Result and conclusion |
| --- | --- | --- |
| 229027 | `usb2-live-halted-count` (3 pulses = `DSTS.DEVCTRLHLT` set, 1 = clear) | `rows=4 ccs=1010 pulses=1` plus host `-110`. **The device controller is not halted after Run/Stop.** This removes the halted-core explanation for the empty event ring and for `try_arm_setup()`'s halt gate, and shows the "RUN/STOP readback timed out" line is not a statement about the resting state. |
| 239624 | Baseline **minus** `--start-after-connect` (arm the SETUP TRB before Run/Stop, which is the qpr1 order) | **No USB activity in the entire probe window.** The pre-connect `start_transfer` failed and the handoff returned `fail(12)` before Run/Stop, so no pull-up was published. |
| 247921 | Same, with the pre-connect arm made non-fatal (source change below) | **Still no attach (2/2).** Only `start_after_connect` differs from the baseline in `loop-args-debug.txt`, and the artifact hash differs, so the change was in the image. The non-deferred configuration is therefore not an attach-reaching base, and the fatal abort is not why. |
| 255742 | Baseline + `--usb2-source-devten-before-runstop` | Attach and `-110`, identical to baseline. The flag is in the build (`build-command.txt` arg 22, `build.rs:1152/1830`). Enabling device events before Run/Stop does not move the host boundary. |

Source comparisons made in this pass (qpr1 `7152`/commit 7b0944645172, as retained under `/tmp/exact`):

- `dwc3_gadget_run_stop()` arms the first SETUP TRB before `DCTL.RUN_STOP`, with STARTTRANSFER
  command parameter 0. The port's command word, TRB flag set (`HWO|LST|IOC|ISP_IMI`) and event
  buffer programming already match.
- `dwc3_gadget_start_config()` orders `DEPSTARTCFG` then `SETTRANSFRESOURCE` (one resource per
  endpoint) before `SETEPCONFIG`; the port follows the same order.
- `dwc3_gadget_enable_irq()`'s mask (DISCONNECT, USB_RESET, CONNECT_DONE, WAKEUP, ERRATIC,
  CMD_COMPLETE, OVERFLOW, VENDOR, plus ULSTCNG below 2.30a) is what `direct_gadget_devten()`
  returns, so the mask is not the difference either.

Correction to the earlier "nothing arms EP0 before the pull-up on the production path" reading: that
holds for this profile **because** `--start-after-connect` is part of it. The pre-connect arm exists
in the source (`gadget_handoff.rs:988`); the flag removes it from the running configuration. The
supporting enumeration of `EP0_SETUP_ARMED = true` sites missed one assignment because it is a
boolean expression rather than the literal `true`.

Change in the tree, uncommitted: the pre-connect STARTTRANSFER failure records
`pre_connect_armed = false` and continues to Run/Stop instead of returning `fail(12)`. With
`--start-after-connect` the arm is deferred, so the attach-reaching baseline is unaffected; the
change only makes the non-deferred configuration's failure legible.

Unrelated to these changes: `tools/verify.sh` step 3 (board device-tree audit,
`usb2_phy0:qcom,param-override-seq`) fails on this HEAD with and without the change above.

## Late-evening follow-up (HEAD 83f2595e + local instrumentation)

A host-visible ordered once-only pulse bracket (selector
`usb2-live-pollbracket`, usbmon CCS edge counting, channel calibrated at 2
pulses = the two control arms) was built to answer one question without an
analyzer: on the attach-reaching profile, does the post-handoff owner ever run?

Answer: no. `poll()` is never entered, the handoff call never returns to
`usb_probe.rs`, and the 15-180 s failure park that a `!gadget_ready` return would
start never runs (every run ends with `boot-reason=watchdog` and a 62-63 s
`enumeration-window-ended` capture). So the EP0 SETUP buffer has no consumer, EP0
has no armer after the handoff, and the host's first GET_DESCRIPTOR is never
answered - the measured `-110` boundary.

Stage-by-stage counts, the refuted alternatives (arm-order A/B; DEVTEN timing;
the halt story) and the measured observer perturbation (marks inside the 400 ms
arm window move the stop) are recorded in the skill's `boundary-state.md`. All
code changes are instrumentation plus one reverted experiment: the in-handoff arm
window was briefly skipped for `--start-after-connect`, which neither made the
handoff return nor moved the host boundary, and is reverted.

Next step is structural, not another probe: the consumer has to start
independently of the handoff's post-Run/Stop tail (drive first), because
`usb_probe.rs` reaches `run_ep0_signal_probe` - and its first `poll()` - only
after the handoff call returns.

## Night follow-up: the consumer is now live

The bracket established that the post-handoff consumer never ran because the
handoff never returned. Acting on that (the direct approach the tail comment of
`init_usb2_gadget_reuse_fastboot_ep0` already describes), the polling owner is now
driven from inside the handoff, immediately after the Run/Stop that raises the
pull-up (`drive_polling_owner()`, `usb/poll.rs`).

Effect, measured: the device answers. The host's descriptor attempts no longer
time out after 5 s; they return promptly with `-71`, the host retries for ~17 s,
and it gets far enough to attempt SET_ADDRESS ("device not accepting address
112"). The failure has moved from "no consumer" to "the host rejects the
response", which is a different and smaller problem.

The drive body is `poll_setup_buffer()` alone: the event-ring read is the
statement that does not return in this context (bracket stage 11 silent, stage 10
fired). Adding `try_arm_setup()` to the same tight loop regressed the run's pulse
pattern, so the next A/B is arming once per consumed SETUP instead of every pass.

Constraints recorded in the skill's `boundary-state.md`: the usable window is
~0.3-17 s after the pull-up (host's first request to the secure watchdog's bite),
not the 50 s the constant requests.

### drive-first variants B/C/D

Arming once per consumed SETUP (B), draining the event ring (C) and performing
the missing data->status transitions by hand (D, `service_control_eventlessly()`)
all leave the host at the same place: prompt `-71` and "device not accepting
address N". The per-SETUP pulse marks saturate at one, so only about one of the
host's requests is consumed in the whole window - which points at the re-arm
after the first SETUP, not at the status handshake alone.

Next instruments are device-side values, not more host-side runs: publish the
queued descriptor length, `LAST_CONTROL_ACTION`, and the arm outcome from the
drive loop, so the response the host rejects can be named.

### drive-first, second stretch

Further variants (ring drain, eventless status synthesis, XBL NORMAL data TRB,
short first descriptor) left the host at the same `-71`. Two loop-body facts came
out of the pulse patterns: `poll_ep0_event_ring()` stops the drive loop, and
`service_control_eventlessly()` perturbs EP0's arm state enough that the host's
SETUP stops landing in the buffer at all. Both are recorded with their run ids in
the skill's `boundary-state.md`.

The configuration now in the tree is the one that measurably carried the host to
the address stage (buffer poll + arm on a consumed SETUP) plus read-only marks,
including a one-word bucket for whether the data-phase STARTTRANSFER was accepted.

### drive-first, third stretch

`EP0_SETUP_ARMED` is false at the drive's start, so the host's SETUP had nowhere
to land and the buffer poll could never consume it consistently. The drive now
arms EP0 first - which, unfortunately, makes the CCS mark channel unreadable
(one edge, reproducible), so from here the readouts are host-side.

Host-side distance, measured: no descriptor byte has ever been delivered. Every
`descriptor_complete` is `-71`/`-2` with `length=0`.

Primary-source comparison (provisional: the local 4.19.110 tree, the runtime
commit fetch was 503): TRB flags and phase structure match Linux; the one
difference found is that Linux refuses a second STARTTRANSFER while
`DWC3_EP_TRANSFER_STARTED` is set, which `try_arm_setup()` does not check - and
the drive re-arms on every consumed SETUP.

### Buffer/slot differentials

`--abl-setup-trb-buffer` and `--xbl-direction-trb` are both refuted: the host still
sees `-71` with `length=0` on every descriptor completion, so neither the ABL
TRB-buffer alias nor the XBL direction-split slots is why the SETUP the software
polls never holds the host's request.

### SMMU/DMA-mapping spellings and the ABL event consumer: refuted

Five host-side-only runs (`--dma-adopt-smmu`, `--smmu-disable`,
`--smmu-install-bypass`, `--smmu-install-bypass --smmu-install-all`,
`--abl-event-consume`) all leave the same host state: `-71`, and every descriptor
completion with `length=0`. No descriptor byte is delivered in any of them.

Two hypotheses therefore do not explain the boundary:

* the project's own unverified `--no-smmu` assumption about physical addresses for
  the DMA objects (none of the four mapping spellings moves the host);
* event delivery (neither the ordinary consumer, which stops the drive loop, nor
  ABL's acknowledgement order changes anything host-visible).

What is left is the link/PHY layer and the precondition for any EP0 event at all.
The harness's own `next-experiment.txt` reaches the same conclusion independently:
inspect USB2 PHY RX/SOF or event ingress rather than further EP0/TRB permutations.

### SOFFN: the link is live

`usb2-live-soffn-after-drive` (poll-only drive window, one bucket, no other marks)
returned 1 pulse: DSTS SOFFN advanced while the host was talking, so SOF frames
reach the controller and the failure is not missing link traffic.

Together with the earlier runs the picture is: armed EP0 -> the core transmits
something the host rejects (`-71`); unarmed EP0 -> the host times out (`-110`);
frames arrive either way. The next readings are (a) whether the "one CCS edge"
runs were unreadable marks rather than a wedged loop, by delaying the arm ~1 s
after the pull-up, and (b) with a readable channel, whether the response
STARTTRANSFER is accepted and whether the core retires the data TRB.

Note: the drive's window and instrument are now chosen by the compile-time readout
word, so existing readout selectors are no longer starved by the drive.

### Overnight sweep (21:40+): first analyst read

Two baseline records and one `soffn`. Machine-decoded CCS timelines:

* baseline x2: a single ~265 ms entry low right after the connect, then a level
  pull-up for the rest of the 62 s capture. Host logs (own kernel.log): -71 at the
  descriptor stage, a 15 s-later -110, then address-stage -71s. Zero descriptor
  bytes ever (max length 0).
* soffn: ONE 2241 ms low 3 s after the connect = "SOFFN moved"; reproduces 482532
  line for line. The link delivers frames while the host is talking.

Boundary unchanged: host advances to the address stage, every response rejected,
no bytes delivered. Sweep list unchanged (baseline-2, bracket, then the PHY
spellings); the next code-level readings (delayed arm, data-phase readout) are
queued for an analyst window with build budget.

### Analyst read #2 (23:13): lost-tick evidence recovered; baselines stable

The two new records are (a) the 23:04 tick loss (harness shape, already
fixed in `tools/overnight_step.py`) and (b) a third stable baseline
(`--110`, 12 completes, max length 0). The lost tick's device run was
recovered from its orphan rundir 627326: drive-off URB shapes are
5.3/5.4/5.1 s unanswered GETs first, then instant `-71`s - the inverse of
the drive-on baseline order - with the same `--110` endpoint and zero
bytes delivered. The in-flight soffn repeat (637432) reproduced
`rows=4 ccs=1010 pulses=1`. Sweep list unchanged; the next tick collects
soffn and launches `baseline-2`.

### Analyst read #3 (23:50): the 600 ms arm does not return on the current tree

Seven records since read #2. Baseline 5/5 stable (266-269 ms entry dip, `--110`,
12 completes, 0 bytes); soffn 2/2; no-drive 2/2 with the portable drop time
+20.861 s. New pair: `setup-reached` and `arm-stage` both published zero marks
and the device went dark at +6.522/+6.523 s = 600 ms + 5.923 s (the watchdog
rule) - the drive's arm call at 600 ms is where the loop stops on tree
67469e0f, and nothing after it can be published. The "600 ms returns" premise
comes from run 537442 on the older tree 00a36ba4. Next: `arm-steps` (existing
word, marks inside the call) at the head of the sweep. Also fixed: `one_sel.sh`
now expands `$EXTRA` (the no-drive-extended-arm record 684647 was silently a
plain no-drive run; `usb2_extended_setup_arm: false` in its own debug dump).

### Analyst read #4 (2026-09-30 00:35): arm-steps returns TRUE at 600 ms on the current tree

Seven new records read. Headline: on kernel-identical trees, the 600 ms arm inside
`try_arm_setup()` publishes all four step marks and a single mark B = the arm returned
TRUE (698491), while the same call with no marks inside stalls (setup-reached/arm-stage,
death at 0.600 + 5.923 s). The marks are not observationally neutral - their keepalive
delays carry the USB domain. 537442's word was actually `usb2-live-arm-late` (it did
return, refused) on tree 00a36ba4; the ledger row label is corrected in
boundary-state.md. Tree 67469e0f -> 12ceb2b3 is a docs-only delta. `no-drive-long-arm`
(707045): first genuine run with the EXTRA fix - wire result unchanged (drop +20.861 s).
Replicates of arm-steps are queued (arm-steps-2/-3); the true reading is n=1.

### Analyst read #5 (2026-09-30 01:15): arm-steps 3/3; the no-mark arm stops at every timing; TRB-retire word fixed and re-queued

Eight records since read #4, all kernel-identical to 698491 (a45bbde8 vs 12ceb2b3 is a
docs-only delta). arm-steps-2/-3 replicate 698491 within ~5 ms: the mark-carrying 600 ms
arm returns TRUE, 3/3. The no-mark in-drive arm stops the loop at every timing measured
(entry / 600 ms / 1.8 s; drop = arm + 5.923 s in all five runs). consume-late 2/2
identical but published nothing - the entry arm stops the loop before the consume marks;
the host's 6-completion -71 tail is the board dying at +6.6 s (the last -71s coincide
with the port drop within ~2 ms), not a new answer. no-drive-extended-arm is genuine
(usb2_extended_setup_arm: true) and wire-identical to every no-drive; the drive-off arm
windows (400 ms / 5 s / 10 s) are now all measured - retire those variants. baseline
10/10. setup-trb-state published nothing (its entry arm cannot return); fixed by gating
the ep0 step marks on it (build-verified in run 800002's own kernel compile: +8 lines at
ep0.rs, no errors, booted). Cycle 23+: clock-check, setup-trb-state (fixed),
setup-trb-state-2, baseline-8.


## 2026-09-30 01:55 - analyst read #6: setup-trb-state fixed word 4/4 (five dips) - but its single report pulse is NOT arrival evidence

Machine alignment (`tools/ccs_align.py`, new) shows the host made NO attempt between the word's
TRB publish (t0+1.16) and its HWO check (t0+2.00) in all four runs; the clear HWO at the check
therefore cannot quote a SETUP reception, and ep0.rs's own fallback comment ("this revision
failed to clear HWO while delivering the packet") contradicts `setup_trb_retired`'s doc claim.
Withdrawn before propagation: "1 pulse = request reached EP0". clock-check 2/2 (control paid);
baseline 12/12. ep0.rs: `late_arm_mark` re-gated for `usb2-live-consume-late` (its entry arm
now carries the step marks so it can return); cargo build/check rc=0; first rebuilt tree =
clock-check 858139. Next: consume-late (fixed), setup-trb-state, clock-check, baseline-8.


## 2026-09-30 02:40 - analyst read #7: consume-late (fixed) publishes a seven-dip lattice, 2/2 within 7 ms

The fixed `usb2-live-consume-late` now returns and publishes: seven dips, replicated exactly
(866529 / 893144, falls within 7 ms: +0.062/0.536/1.160/1.577/1.992/2.586/3.064). Dips 1-4 =
the same mark lattice as setup-trb-state (m1..m4); dip 5 = the first pass after the arm's cost
ends (both words); dips 6-7 = the consume marks - one full pair (c1+c2, "buffer fresh, handler
returned") plus a second c1, with the run ending there (no c2', no exit pair). The exit pair is
excluded by the pet arithmetic: the loop was already dead at +3.48 (last pet; window end is
~+4.06). Keep read #6's HWO warning: a c1 without a host submit in its window is not a
"SETUP landed" quote. Instrument note: hub-status CCS samples come in 208 ms bursts with ~5 s
dark gaps - "no dip observed" is unreadable in a dark window; liveness comes from the pet
arithmetic. clock-check 4/4; setup-trb-state 6/6 (901714 added); baseline 13/13. Correction:
read #6's cross-word "same sites as arm-steps' first four" clause holds for dips 1-2 only.
Next: baseline-8, then the third consume-late replicate.


## 2026-09-30 03:15 - analyst read #8: consume-late 4/4 (three tight replicates + one host-correlated outlier)

The fixed consume-late word now has four replicates. Dips 1-5 replicate 4/4 within
7 ms; dips 6-7 (the consume marks) replicate in 3/4 (+2.586..2.591 / +3.063..3.070)
and are off-band in run 922663 only (-116 ms / +88 ms, widths swapped 474/209). That
run is exactly the one with the changed host face (no second 3-submit group; --110;
7 completes), and its d6/d7 fired with NO control URB to the device between +0.000
and +3.620 - the dips cannot be quoted as "a SETUP was consumed" (read #6's HWO
caution, materialised end to end). Controls: clock-check 6/6, setup-trb-state 7/7,
baseline 15/15; descriptor_max_len still 0 in every readable run. boot-reason.txt
missing 1:1 with baseline runs (15/15 vs 0/33) - baseline-path property. New tool:
tools/usb_event_window.py (URBs + raw CCS samples on one axis). Next: consume-late-
weighted sweep (4/8) to test whether the off-band mode always accompanies the host
face change.


## 2026-09-30 03:55 - analyst read #9: consume-late's outlier does not recur; setup-inpage wired into the drive, arm-land/setup-inpage entry arms gated

Three more consume-late runs (974590/981781/994587) all land in the tight band with the
grp2 host face (d6 = grp2 + 62-63 ms); the 922663 off-band mode is now 1/7 and its
host-face correlation stands. setup-trb-state 9/9, clock-check 7/7, baseline 16/16;
descriptor_max_len still 0 in every readable run. The pointer word
`usb2-live-setup-inpage` was never readably run - its drive wiring was incomplete (it
fell into `Some(_) => return`) and its entry arm carried no marks; both fixed
additively (poll.rs window arm, ep0.rs gate) and build-verified (rc=0). Queue
rewritten: arm-land, setup-inpage x2, clock-check, consume-late x2, baseline-8,
setup-trb-state - arm-land vs setup-inpage is a slot-0/slot-1 buffer contrast with
identical report sites.

Addendum 03:58: row 56 (the in-flight consume-late) is tight band again - off-band
rate 1/8. And the first arm-land run on the edited tree is readable: 5 dips = m1..m4
plus a single result pulse (the arm returned TRUE); the landing pulse never fires
because the drive stops at ~+2.48 (replicate will say if that stop is stable).
Queue now: setup-inpage, arm-land, clock-check, consume-late, baseline-8,
setup-inpage, consume-late, arm-land.

Second addendum 04:10: arm-land 2/2 + setup-inpage 1/1 identical (5 dips, stop +2.475 +-4 ms,
result pulse = arm TRUE, landing never reached). New fact: the root hub completes a port reset
at +2.475 in every run examined (PED=1/PRC=1), and band consume-late survives it (emits d6/d7
after), while the arm-land class stops there - open question for the next read. Also: d6/d7
lock ~62 ms after a HUB-class control transfer, not the EP0 submit group (922663 fits; band
runs' hub burst and EP0 group share a timestamp).


Addendum 04:52: first `usb2-live-data-phase` run reads `rows=2 ccs=10 pulses=0` (--110,
watchdog, 4 completes, 5.1 s stall first) - the no-mark-arm stop, not a device fact: its
entry arm goes through `try_arm_setup()` and the word was missing from the `late_arm_mark`
gate (the 742348 / 771804 lesson). Added it to that gate in ep0.rs; aarch64 build
re-verified (rc=0). The queued `data-phase-repeat` should now publish its two digits
(trb_state+1, reject?2:1). precheck's row: 9 pulses, --110, 3 prompt -71s, no stall - the
never-arm profile answers without any stall, as designed.


Addendum 04:50: precheck-2 (separator build) reads `rows=22 pulses=10`, `--110`, 3 completes
(no stall). A parallel session has since moved `data-phase` to a 600 ms arm and added
`usb2-live-triage` / `usb2-live-arm-recover`; the combined tree (their edits + this read's
ep0.rs gate addition) builds rc=0. Gate addition: `usb2-live-data-phase` joins `late_arm_mark`.


## 2026-09-30 05:25 - analyst read #10: fresh-gates verdict true; the fault is now at the EP1 data start; EP1 latch bug fixed

Rows 67-77 decoded. Corrections first: jsonl row 74 (`fresh-gates`, rd 1131834.0) is a
second `setup-buffer` replicate (its manifest says `usb2-live-setup-buffer`); the real
fresh-gates launch became 1133458.0 (`device-absent`, transport wait timed out) and the
one valid fresh-gates run is 1139667.0. `setup-buffer` is 2/2 at `+1.66/+3.06/+4.46` ->
f1-3 = 1,1,1 (GET_DESCRIPTOR shape; fields 4-5 never fire).

`fresh-gates` 1139667.0: `+1.56/+2.56/+3.56` -> f1 `setup_packet_pending()` TRUE (which
*implies* the four TRB conditions held at +1.5 s - an implication, not four readings),
f2 Setup, f3 armed; f4-7 unread (silence after +3.62). `data-report` 1145750.0: digits
1,1,1,1 - `DATA_PHASE_PENDING_LEN != 0` (a DataIn response was prepared; that static is
written only in the DataIn arm) and `DATA_PHASE_PENDING_START` (the EP1
`retry_start_transfer(1,_,2500)` returned false). This moves the fault past the consumer:
the response is built; the data-phase start is where it stops. `response-bytes` f1 =
b[0]==18 is certain; the later digits are provisional (those pulses are 207/208 ms wide
against 62 ms) pending the queued re-runs.

Every passive word now loses its tail fields (last publish 3.6-5.0 s), so "no pulse" is
"nothing publishing by then", never a false; schedule fields inside ~4 s. Independent
audit found the new EP1 timeout recording unreachable (nested `endpoint == 1` inside
`endpoint == 0`); fixed additively, build rc=0. Next: data-report / data-cmd x3 (the latch
those runs read was the buggy one, now fixed).

Addendum 05:25: row 77 (`data-report-2`, rd 1152089.0): digits 1,1,1,1 at
+1.56/+2.56/+3.56/+4.55 - data-report is now 2/2. The step queue is between runs; its next
launch is the new `data-cmd` word (which reads the EP1 latch this read just fixed).


2026-09-30 06:10 (analyst read): data-cmd new-order runs 1192489.0 / 1199156.0 /
1206807.0 all read bit 31 set + status nibble 0 in `DATA_PHASE_LAST_COMMAND`:
the EP1 data-phase STARTTRANSFER never retires (3/3). data-report is 2/2 (a
response was prepared; start still flagged pending). ep0-physical x2 carried no
CCS readout (classification-only). Next: `usb2-live-depcmd` reads DWC3_DEPCMD(1)
and (0) raw (CMDACT/CMDSTATUS) - see boundary-state.md.

### 2026-09-30 06:40 - depcmd: EP1 command held active; dirtrb no-op; phys-0 fix now bound to data-cmd

- `usb2-live-depcmd` (06:13): DEPCMD(1) CMDACT set at +1.4/2.3/3.2 s, status 0,
  parameter nonzero -> the EP1 STARTTRANSFER is accepted and still active in the
  controller. The never-retired latch (3/3) is now controller-side fact.
- `--xbl-direction-trb` + data-cmd, 2 runs: decode identical to the 5 plain runs
  (f0=1 never-retired, f1=1 status 0, f3=1 pending start; arm result 415 ms at
  +2.84 s). The TRB-slot split alone does not unstick the command.
- `--xbl-direction-trb` + fresh-gates: gates still 1/1/1; first publish past +4 s
  ever (one 62 ms pulse at +6.59, slot-matching field 6 of 7 = TRUE).
- Code (additive, build rc=0): phys-0 routing now also applies to
  `usb2-live-data-cmd` runs (labels `fix-data-cmd*` / `fix-dirtrb-data-cmd*`);
  depcmd field schedule tightened to 1.2 + k*0.7 s.
- Next queued: fix-data-cmd, fix-dirtrb-data-cmd x2-3, depcmd-2/3, baselines,
  response-bytes-2/3. Goal signal remains descriptor_max_len > 0.


### 2026-09-30 07:20 - routing reads like the control; the sweep was losing half its runs to a bookkeeping race (fixed); response-bytes splits

- `fix-data-cmd` (1269691.0, first sample with cmd->phys0): decodes byte-identical to
  the control family (1/2/1 at +1.46/2.36/4.16, arm result 415 ms at +2.84). The
  phys-0 routing does not unstick the never-retired command. `fix-dirtrb-data-cmd-2`
  x2: the dirtrb family is now 4/4 identical.
- `response-bytes` 2/2 vs 2/2 split, correlated with the host family: the 4-pulse
  samples (extra 62 ms + 208/207 ms marks at +2.8/+3.1/+3.5 s) ran the -71 family,
  the 2-pulse samples the -110 family. Wide marks flagged for width-vs-emit check
  before any digit is assigned; raw edge tables in boundary-state.md.
- 06:45-07:17: 14 of 30 records junk (`no run directory matched`, duration 20 s) and
  9 orphan dirs, all device-absent. Root causes (all fixed): `harness_running()`
  missed one_sel.sh/no_readout.sh during their device waits so live runs were
  collected early and doubled; the lock's age-based staleness let ticks unlink a live
  supervisor's lock; one_sel.sh lacked no_readout.sh's 4ee0 -> `fastboot reboot`
  recovery. Supervisor restarted (PID 1334487, unbuffered) to load the fixes.
- Next: the wrap retries the eaten words; depcmd-2/3 still 0 clean samples. Goal
  signal unchanged: descriptor_max_len > 0.
