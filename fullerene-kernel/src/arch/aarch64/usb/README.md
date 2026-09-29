# `usb/` — why the code is shaped this way

This file is the single home for the *reasoning* that used to be duplicated as long
comment blocks at each call site. The call sites now carry one-line pointers here.

**Nothing in this file is a substitute for reading the code.** It records only what the
code cannot: which shapes we already tried, what a measurement actually refuted, and
which constraints are structural rather than stylistic. When a comment at a call site
and this file disagree, this file is the older claim — trust the code and the
measurement.

---

## 1. Structural constraints (do not "fix" these)

### 1.1 `usb/` is compiled by more than one crate root

`usb_probe.rs` is a `[[bin]]` crate root (`Cargo.toml`), and it declares `mod usb;`
itself. `arch/aarch64/main.rs` declares it too. So **every `static mut` in this tree
exists twice**, once per crate.

This is not accidental and cannot be removed by moving files. Five modules reference
each other through `super::`:

```rust
// usb_qemu_sim.rs
use super::{ uart, usb_dwc3_sim::Dwc3DeviceModel,
             usb_protocol::{...}, usb_regs::{...} };
```

For `super::X` to name a sibling in *every* crate that compiles these, they must be
declared at the same module level in each. Making them children of `usb` would require
the host crate to own a `usb` parent, which means compiling `usb/mod.rs` for the host —
and that file needs `super::platform` (218 refs) and `super::timer`, neither of which
exists at the host crate root.

The `#[path = "usb/<name>.rs"] mod <name>;` technique lets the bytes live in `usb/`
while the module name and level stay put. That is why files can sit in this directory
and still be declared as siblings of `usb` rather than as its children.

### 1.2 Consequence: address-selecting statics must not be plain `static mut`

A `static mut` holding a *value* merely reports different numbers to different crates.
A `static mut` holding an *address decision* makes the two crates read **different
memory**, and both will believe they are right.

`DMA_ADOPTED` (was `mod.rs:508`) was exactly this. `ep0_setup_data_ptr()` returns either
the linker's `EP0_SETUP_BUFFER` or `ep0_trb_ptr(0)` inside the adopted SMMU page,
depending on it. The handoff sets it in the probe crate; the kernel crate's copy stays
`false`. Measured: `usb2-live-adopted` reads `DMA_ADOPTED == true` while `dwc3-setupnz`
(kernel crate) and `setupdr` (probe crate) disagree about the same buffer — because they
were reading two different buffers.

**Rule: anything both crates must agree on lives in the crate-shared section.** The
retained trace already did this for years via `#[unsafe(link_section = ".usb_trace")]`;
the linker script places that section once (`build.rs:2489`,
`KEEP(*(.usb_trace .usb_trace.*))`) and the allocator excludes it
(`allocator.rs:193-208`). `SHARED_DMA_ADOPTED` and its two companions now live there
too. No linker-script or allocator change was needed — the section already existed.

### 1.3 A `-> !` function is a one-way door into the diagnostics

`run_ep0_signal_probe` is `-> !`. Anything below a `park_for_seconds` in it is
unreachable for as long as that park lasts. The failed-handoff park alone is
`stage * 15` seconds — up to 180.

### 1.4 The one-way door had a second lock: the pass budget

Section 1.3 says a `-> !` function is a one-way door into the diagnostics. On 2026-09-28 the
*consequence* of that shape was measured, and it is worth stating as its own rule because the fix
that "looked done" was not.

`run_ep0_signal_probe()` drives the controller with two bare `usb::poll()` calls at the top
(`:818`, `:826`), then falls into ~500 lines of diagnostic gates, and only then reaches the real
loop at `:1341`. The bounded polling loop at `:846` is guarded by `if !gadget_ready`. On a
*successful* handoff `gadget_ready` is true - that is what success means - so that loop is skipped.

**⇒ So the entire pass budget for a successful handoff was two calls, and the host's descriptor
timeout is ~6 s.** Measured: attach at `09:48:09.786`, `device descriptor read/64, error -110` at
`09:48:15.398`. Two `poll()` calls complete in microseconds. The device then spent the rest of the
host's window in diagnostic bookkeeping that never drives the core.

The measured signature, with a positive control, is unambiguous:

```
probe_reach   pulses=1  TRUE    positive control (retained trace, crate-independent)
dwc3-setupnz  pulses=1  TRUE    the SETUP payload IS in the adopted SMMU page
setupdr       pulses=0  FALSE   handle_setup() never ran
```

Data present, reader never called. `dwc3-setupnz` TRUE is only meaningful after the
`SHARED_DMA_ADOPTED` fix (section 1.2) - before it, that word read the linker buffer that DWC3 does
not write to on this path, so the same TRUE would have been noise.

**⇒ The rule:** when a driver loop sits behind diagnostics, count the *passes* the driver gets
before the deadline that matters, not the lines of code. Two passes look like "the driver runs
first", which is what the earlier fix intended and what its comment at `:823` claims. What the
comment said was right; what the code did was not. The fix is a bounded *drive window* sized against
the measured host timeout (`FULLERENE_USB_PROBE_DRIVE_SECS`, default 10 s), placed before the
diagnostics.

**⇒ And do not "fix" this by moving the `:1341` loop.** That loop's late placement is what the
downstream gates depend on. Moving it is the refactor attempted and reverted earlier the same day
(see the file-splitting notes): a bounded window in front is the change that leaves everything else
alone.

### 1.6 Cache the controller identity at handoff

`GSNPSID` is read while the controller aperture is known to be powered. The
handoff refreshes the retained `SHARED_SNPSID` value on every attempt so later
readouts use the current controller identity without touching a possibly gated
register window.

---

## 2. The defect class this directory kept producing

Six separate defects, all the same shape: **a diagnostic condition controlling whether
the driver ran.**

| Where | Shape |
| --- | --- |
| `usb_probe.rs:2816` | `if !gadget_ready \|\| gate_active { run_ep0_signal_probe(..) }` — a gate decided whether the driver was started at all |
| `usb_probe.rs:801`, `:2862` | `#[cfg(all(ep0_signal_probe, handoff_probe))]` — a *diagnostic* cfg compiled the driver loop out of the profile that actually runs |
| `usb_probe.rs:2835` | `if !gadget_ready && DIRECT_ONLY { reset_after_probe_failure() }` — the attribution control reset the handset before the driver was called |
| `usb_probe.rs:891` | `park_for_seconds(stage * 15)` — waited 15-180 s without polling, so the host gave up with `-110` |
| `usb_probe.rs:809` | driver loop at the far end of ~475 lines of gates |
| `usb/mod.rs:5455` | `poll_setup_buffer()` was a no-op unless `--utmi-postrun-readout` named one of seven selectors |

The reason all six survived review is that they *look* like care. `if !gadget_ready`
reads as "only diagnose failures". `#[cfg(diagnostic)]` reads as "don't ship this".
`ref == None` reads as "off by default". Each one is a gate that quietly owns the
driver.

**Rule: drive first, unconditionally; let diagnostics observe.** `run_ep0_signal_probe`
now calls `usb::poll()` before consulting a single gate, and its diagnostics run inside
the loop rather than in front of it. The host is answered regardless of what any
diagnostic does or how long it takes.

**Rule: `--signal-probe` is not a cfg.** `bramble-usb.rs:2034` turns it into the runtime
variable `signal-probe=true`. A cfg needs `--usb-ep0-signal-probe`, which pushes
`FULLERENE_AARCH64_USB_EP0_SIGNAL_PROBE` (`main.rs:4650`), and the reproduced profile
deliberately does not pass it (pinned by the test at `bramble-usb.rs:6601`). A
command-line flag being present is not the same claim as a cfg being on in the
compilation that runs.

---

## 3. Instrumentation: what is trustworthy

### 3.1 Write to the retained trace; read it from anywhere

`USB_TRACE` is `#[unsafe(link_section = ".usb_trace")]`, so both crates read the same
bytes. `prev_boot_*_code()` decoders scan it by position, not by symbol.

Words that go through it — `probe_reach` ("SIG"), `probe_s1..s4` ("PSTA"),
`hop_ge1..9` / `hop_any` ("HOP"), `setupdr` (`TRACE_HARVEST_SETUP`) — are
**crate-independent**. Words served by `usb2_live_word` are not, because that function
lives in the kernel crate and reads kernel-crate copies of `usb/` state.

### 3.2 One bit per word

Multi-bit words are read by counting low→high pulses on the CCS line, and popcount is
irreversible: `2` and `1` both arrive as one pulse. Any word whose value range has
colliding popcounts is unreadable. `probe_stage` (0..6) has that flaw; `probe_s1..s4`
exist because of it. `dwc3-trb` (0..3) likewise needed `dwc3-trb0hwo` / `dwc3-trb1hwo`.

### 3.3 A marker that cannot distinguish two situations is not an instrument

The "SIG" marker originally sat at `usb_probe.rs:1262`, deep inside the function. It
reported absent both when the function was never called *and* when it was called and
parked on the way. It had to become the first statement to be worth anything.

Same lesson, other direction: `dwc3-setupnz` read true while `setupdr` read false, and
the resolution was not "the buffer is inconsistent" but "the two readers were reading
different buffers" (§1.2).

### 3.4 Baseline checks

- Print the control first. Five failed measurement rounds in one session came from
  starting with the interesting word instead of the known-good one.
- `log_puts` is empty under `handoff_probe`, so a silent log is not evidence.
- `grep <expected>` is not a measurement.
- Absolute values carry off-by-one contamination from the pulse counter's first line;
  only differences survive.

---

## 4. Hardware facts established by measurement

These are the ones that survived; each was reproduced with a control.

- `--hsphy-before-reset` puts the link at **high speed**, direct-attached, reproducibly.
  It matches Linux ordering: `dwc3_msm_resume()` runs the hs-phy init (`:2869`) before
  `dwc3_core_init` (`:2290`).
- `DSTS.SOFFN` **advances** while attached and is **static** with the pull-up dropped
  (`soffn-count` vs `soffn-control`). SOFFN is frame-driven, not free-running, so the
  host's frames are being decoded.
- `WPortStatus` belongs in every report: `0x0101` direct, `0x0107` hub, `0x0507` broken
  topology.
- `--dcfg-fullspeed` makes the host report full speed and removes `-110`, but never
  enumerates.
- PHY-side values match Linux exactly; SUSPHY was a false lead and cost days.

### 4.1 Refuted

Kept because each one looks reasonable and will be suggested again.

Warm handoff · pulse-count linearity · EP0 arm · DEVTEN · SETUP TRB · DALEPENA · SMMU ·
DEPCFG · SUSPHY (×2) · harness composition · OPMODE · datapath override ·
SW_SESSVLD_SEL · USB2 PIPE mux · DCFG.NUMP · soft reset · param-override · FSEL DT
override · RUN_STOP timing · UTMI_CTRL1 PD path · ordering hypotheses · handoff delay ·
"the handoff succeeds" · "RX is dead" · "no SOF / never reached U0" · "HANDOFF_PROGRESS
== 0 is success" · "SETUP is in the buffer" (wrong buffer — §1.2).

---

## 5. Measurement method

1. **Primary source first.** Read the actual driver at the device's commit
   (`/proc/version` gives it; fetch via
   `curl -s '<repo>/+/<FULL_SHA>/<path>?format=TEXT' | base64 -d`). Branch names 404.
   Never infer behaviour from a comment.
2. **A proxy predicate must be about the same thing as the question.** "The value has
   no bit set" ≠ "we cleared it". "The most recent read" ≠ "the read that write used".
3. **Site identification needs an explicit tag.** `Location::caller()` returns 0
   through `#[inline]`.
4. **Counters are only as good as the predicate they encode.**
5. **A FALSE flag means nothing until you have read its clear condition.**
6. **Do not close the search in one file.** The `GUSB2PHYCFG0` writes live in three
   files across 29 sites.
7. **Instrument the earliest point, not the most interesting one.** A wall of
   independent-looking negatives is usually one upstream fact.
8. **Cost discipline.** A run is 74-88 s and that is irreducible: ~11 s build+boot,
   ~16-25 s observation, ~46 s transport recovery which is a safety mechanism and must
   not be shortened (shortening `--enum-timeout`/`--hold` made the CCS carrier stop
   producing pulses — the measurement broke, not the device). The waste was the agent
   polling at 285 s and firing four-word batteries. One outstanding question → one run,
   polled at 15-20 s.

---

## 9.1 The failed-handoff park must still poll

The park is where a failed handoff waits, and it is the reason a
failed handoff can never recover: this loop never drained the
event ring, so Connect Done, USB Reset and SETUP were all
discarded while the PHY kept the pull-up advertised and the host
kept seeing an attached high-speed device. Consuming the ring here
is what lets the *next* event be seen during the wait rather than
only after it. Opt-in so every existing park-timing readout (the
stage*15 s Android-return ladder) stays bit-identical.

---

## 10.1 Milestone bisect words

Same bisect shape as `milestone-ge-*`, but reading
HANDOFF_PROGRESS - what `handoff_progress()` actually writes,
and which had NO readout despite the doc comment at :820
claiming `usb2-live-handoff-progress` published it. The direct
handoff's five silent `return false` sites in
init_usb2_handoff() now report 11-16 through this, so the exact
failing exit is identifiable without UART (probe builds compile
`log_puts` out entirely).

---

## 3.5 Reading probe-crate progress from either crate

Did the PROBE crate get as far as its poll loop? Crate-independent: these
decode the probe's own retained-trace markers ("SIG" at usb_probe.rs:1262,
"GATE"+1 at :1284), so unlike `poll_called`/`act_known` they are not fooled by
usb_probe.rs being a second crate root with its own copy of every static.

  probe_reach true -> run_ep0_signal_probe was entered, so the `loop` at
                      :1310 (which drives usb::poll()) was reachable.
  probe_reach false-> it was never called; the usb_probe.rs:2816 fix did not take.

---

## 3.6 A command-line flag is not a cfg

Same trap as `blipenv`, one level down. The gate readout block
(usb_probe.rs:1819) is entered only if
`option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")` is `Some(...)` in the
compilation that runs the probe. `--signal-cmd-gate dwc3-gate-probe`
demonstrably reaches the build command (build-command.txt arg[23-24])
and `dwc3-gate-probe` returns a fixed 6, yet that run still printed a
single host attach line. So publish the constant itself rather than
inferring it from the child environment.
  gateenv   : is any gate constant compiled in at all?
  gateprobe : is it exactly the discriminator's name?

---

## 3.7 Which ControlAction did on_setup return

WHICH ControlAction did `GadgetDriver::on_setup` return? The four
response-phase words were all initial, so the callback's return value
is the one remaining unknown. One bit each so the pulse count is
unambiguous:
  act_known    the callback returned at all (any action)
  act_datain   ControlAction::DataIn  (a reply was built)
  act_statusin ControlAction::StatusIn
  act_stall    ControlAction::Stall   (would show as EPIPE on the host)
  act_other    StatusOut / SetHalt / ClearHalt / Setup

---

## 3.8 The response phase

The RESPONSE phase. `handle_setup()` ends in `retry_start_transfer`
(mod.rs:3985) and records whether that failed (`DATA_PHASE_PENDING_START`,
:478) and how many bytes it queued (`DATA_PHASE_PENDING_LEN`, :479).
Both are mod.rs statics, so they are readable on the CCS carrier - the
one instrument that is not crate-local by construction.
  ph_pending : the DATA-phase STARTTRANSFER did NOT complete
  ph_len     : a non-zero response length was queued
  ep0_data   : EP0 is in the Data phase
  ep0_status : EP0 is in the Status phase

---

## 5.1 Event-ring register programming

---- Event-ring registers: did the programming stick? ----
`evtadr_set`/`evtsiz_set` read the registers back at the post-run
readout; `evtprog_set` says the kernel did program them. A run
where `evtprog_set` is TRUE but `evtadr_set` is FALSE means the
writes did not survive, which would explain GEVNTCOUNT0 = 0
without any link problem at all.
---- The one register never read: DCFG.DEVSPD ----
`config.rs:7-37` chooses the device speed for DCFG. When
`snpsid & 0xffff0000 == 0x5533_0000` and the revision is below
2.20a, it selects SUPERSPEED *even for a USB2-only run*, citing
Linux's metastability workaround. Bramble is a DWC_usb31 part, so
whether that branch is taken decides what the controller thinks it
is running at - and a controller in SuperSpeed mode leaves the
USB2 PHY data path unconfigured, which is a direct explanation for
a link that carries nothing. One bit per candidate speed.
---- Link-speed timeline: sampled every 0.5 s across the window ----
  spd_first_hs : device reported HS on the FIRST sample
  spd_last_hs  : device reported HS on the LAST sample
  spd_hs_seen  : device reported HS at ANY point
  spd_changed  : the device's reported speed changed during the window
  fsel_first_hs / fsel_hs_seen : same for the PHY's own FSEL
  spd_sampled  : at least one sample was taken

---

## 3.9 The proven carrier for DWC3 words

Route the DWC3 debug queues onto the PROVEN carrier.

The gate-readout channel (`--signal-cmd-gate` -> usb_probe.rs:1819 ->
`stop_run_cycle`) is not host-visible: the project's own positive control,
`usb2-live-gate-probe6` (mod.rs:9182), publishes two Run/Stop cycles
unconditionally inside the documented 6-7 s attach window and still printed a
single attach line. So every `dwc3-debug-*` zero read this session was a
channel result, not a device fact.

This carrier is different: `usb2-live-ccs-<word>` (mod.rs:9512) keeps the
pull-up alive past Run/Stop, so the root hub keeps reporting CCS and the host
sees the pulses. Same selector names as the gate path, so nothing is renamed.

The sample call is deliberately IN this function, not in the probe crate:
`trace_dwc3_debug_sample` and `dwc3_debug_readout_code` then share one
compilation of `usb/`, which is what the crate-duplication trap (usb_probe.rs
is a second crate root) requires. Sampling is read-only and consumes no events.
All `dwc3-*` names, not only the debug family. `dwc3-trb` is
`(trb0 & 1) | ((trb1 & 1) << 1)` (trace.rs:1134) - the hardware-own bit of the
two EP0 TRBs - which is exactly "is the EP0 OUT transfer live, or does the core
still own it?". Those words come from the retained snapshot
`trace_dwc3_boundary()` (mod.rs:2579), which was only ever called from
usb_probe.rs:1874/1888, i.e. inside the gate path and therefore unreachable here.
Call it in this scope so the word describes THIS boot.

The debug-queue sampler is separate and needs six samples: `debug_queue_metric`
compares a min against a max, and the `*-change` words compare two samples.

---

## 3.10 Handoff milestones go into the shared trace

ALSO publish it into the retained DRAM trace, so it can be read back
crate-independently (note 59). `HANDOFF_PROGRESS` is a `static mut` inside `usb/`, and
`usb_probe.rs` compiles `usb/` a second time, so a reader there sees a different copy.

The marker is "HOP" + the milestone, so the DECODER can report the HIGHEST milestone
reached rather than just the last one written - a stage that is entered and then
abandoned still proves it was reached.

---

## 3.11 Readout-side entry instant

Set the instant `usb2_live_word` is entered, i.e. on the *readout* side.

`poll_called` is set on the poll side and read on the readout side. If those
two sides are different crates - `usb_probe.rs` is a crate root that compiles
`usb/` as its own submodule - each has its own copy of every static here, and
the readout sees a never-written value. Publishing a word that is written and
read entirely on the readout side separates the two cases:

  readout_ran = TRUE  and  poll_called = FALSE  => different crates
  readout_ran = TRUE  and  poll_called = TRUE   => same crate, so the poll
                                                   genuinely never ran

---


## 3.12 Emit the polling marker once

The retained `POL1` marker proves that the USB polling loop ran. Write it once
per boot so repeated polling does not consume the trace ring or evict useful
diagnostic records.

## 3.13 A ladder word is not a predicate word


`prev_boot_probe_reach_code()` returns a *ladder*: it scans the retained trace and folds several
independent markers into one number, including `HOP` as `100 + milestone`. Words built on it
(`probe_reach`, `probe_s1..s4`, `probe_stage`) therefore answer "how far did the boot get overall",
not "did this particular function run".

That distinction cost a session. `probe_reach` was added to answer "was `run_ep0_signal_probe`
entered", and it reads TRUE whenever the handoff progressed at all — HOP is written by
`handoff_progress()` *inside* `init_usb2_gadget_reuse_fastboot_ep0`, strictly upstream of the probe.
Measured 2026-09-28: `hop_ge1` TRUE in one run and `poll_ran` FALSE in another, which the ladder
cannot explain except by being satisfied by HOP alone.

**⇒ Rule: if the question is one predicate, the instrument must be one predicate.** `sig_only`
exists for this: it scans for the "SIG" marker with no fold and no participation from any other
marker. Do not extend the ladder to cover a new question — add a decoder that answers only it. The
ladder also reports different values in different runs (`probe_stage` read 3 while `hop_ge1` read
TRUE), so cross-run ladder comparison is invalid for the same reason.

## 3.14 Bracket the handoff call

`INB4` and `INAF` are retained markers immediately before and after the handoff
call. Together they distinguish a call that was never entered from one that did
not return.

## 3.15 Record synchronous exceptions

The synchronous exception handler writes a retained marker before diagnostics
or UART output. The readout can therefore distinguish a controller hang from
an exception followed by an unavailable console.

## 3.16 Bracket the readout publication

`WBFR`, `WAFT`, and `SELX` mark entry to the USB live-word lookup, return from
that lookup, and completion of the POSTRUN publication block. The readers
validate and scan the same retained trace sequence.

## 3.17 Keep setup detection layout-aware

When SETUP data aliases TRB0, its idle marker is the TRB DMA address. A fresh
packet is detected by a changed address pair; a separate SETUP buffer instead
uses its nonzero payload as the marker.

## 3.20 Read link state from the DWC3 field

For this DWC3 core, `DSTS.USBLNKST` occupies bits 21:18. Link-state gates and
readouts must use that four-bit field consistently.

## 3.22 Pulse breadcrumbs

`--pulse-breadcrumb` selects a one-shot host-visible Run/Stop pulse at a chosen
diagnostic site. The selector is passed to the build script and must trigger a
rebuild whenever it changes.


## 4.2 Speed sampling across the readout window

Speed sampling across the readout window.

The host and the device disagree about the link speed: `lsusb -t` reports
480M while `DSTS.USBCONNECTSPD` reports 1 (full-speed). `lsusb -t` is the
host's own view, so the disagreement is real, but a single post-run reading
cannot say *when* they diverged - and a timing problem and a permanently
broken UTMI speed path need different fixes.

These sample both ends every 0.5 s for the whole readout window (`--hold`),
so the run answers: was the device ever at high speed, and did either end
change during the window?

  DSTS.CONNECTSPD: 0 = HS, 1 = FS, 2 = LS
  COMMON0.FSEL   : 0 = HS, 1 = FS (the PHY's own view)
