# Bramble context entry point

This is the default LLM-facing entry point for the Pixel 4a 5G (Bramble)
investigation. It contains the current state, fixed safety boundary, and the
next useful discriminator. Do not load the full ledgers unless a run or source
detail is needed.

Full evidence is preserved in the compressed [status history](../evidence/bramble/CONTEXT_STATUS_FULL.md.gz)
and the compressed [AArch64 hardware ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz).
Use the [small Run index](../evidence/bramble/RUN_INDEX.md) to select a
targeted section before decompressing either archive.

## Current goals

| Goal | Success criterion | Current state |
| --- | --- | --- |
| USB handoff | Fullerene-owned `idVendor=1234`, `idProduct=0001` | Not reached |
| FullereneOS AArch64 port | Boot the real FullereneOS runtime on Bramble | Early bring-up; generic runtime not yet entered |
| Recovery safety | Failed handoff returns to Android without persistent writes | Confirmed for the recorded RAM-only runs |

## Current state (last evidence update: 2026-09-14)

- The only `1234:0001` observation was produced by a prohibited Android
  configfs rebind. No Fullerene-owned descriptor success has been observed.
- The attach-reaching Fullerene USB2 path crosses HS attach, then fails at the
  address-0 Device Descriptor boundary with zero-payload `-110`/`-71`
  completions. This is a pre-descriptor / pre-USB2-RX-data failure.
- The earlier DSB-corrected hardware record is the decisive internal
  discriminator: the event-DMA probe passed and `armstat=0` showed that
  STARTTRANSFER retired, while the SOF gate reported no SOF frames. The later
  source-exact PHY, clock, Type-C, event-queue, and timing runs preserved that
  same boundary. The remaining fault domain is USB2 HS receive/clock recovery
  (or an external/secure owner of it), not another EP0/TRB formatting choice.
- The current source-exact control remains the reference artifact. Run
  `1700055.0` is byte-identical to the current-source standalone control and
  reaches HS attach before the same descriptor timeout.
- Run `2179479.0` tested only `--skip-typec-spmi` on the normal
  Android-init/direct-handoff profile. QEMU and image audit passed and
  RAM-only `fastboot boot` was accepted, but no Fullerene attach, descriptor,
  Android fallback, or Fastboot return was observed during the bounded window;
  the final state was `device-absent`.
- Run `96250.0` tested the post-DTB normal Android-init/direct-handoff profile
  with the source-backed DMA-ownership fix, entry secure-WDT boundary, and
  explicit DMA cache maintenance. QEMU and image audit passed and RAM-only
  `fastboot boot` was accepted; the host observed only the Fastboot USB
  disconnect, with no Fullerene attach or `1234:0001`, and the final state was
  `device-absent`.
- Run `114346.0` tested the matching pre-DTB profile. The artifact hash was
  `116690ea2a86c0a771c172c7acd8681161337bbe722f6587bc08cdb82a18f28a`, QEMU
  and image audit passed, and RAM-only `fastboot boot` was accepted; the host
  again observed only the Fastboot USB disconnect, with no Fullerene attach or
  `1234:0001`, and the final state was `device-absent`. The DTB-scan ordering
  therefore did not discriminate the normal Android-init failure.
- Run `133532.0` repeated the pre-DTB profile with synchronous-exception trace
  instrumentation. QEMU, image audit, and RAM-only `fastboot boot` passed; the
  host again observed only the Fastboot disconnect, with no Fullerene attach or
  `1234:0001`, and the final state was `device-absent`. The run artifact SHA256
  was `f1d5c9687993f94c75e7cf4f895f8690e16331f93144ca37137cf979dac1a02f`.
- Run `150913.0` exercised the automated two-candidate recovery plan. Both the
  pre-DTB and post-DTB candidates passed QEMU, image audit, and RAM-only
  `fastboot boot`, but neither produced Fullerene USB; the host saw only the
  bootloader's descriptor requests and both attempts classified as
  `fastboot-fallback`. The final state was `fastboot-available`.
- Run `256005.0` used the existing `sof` signal gate on the attach-reaching
  direct USB2 profile. RAM-only `fastboot boot`, QEMU, and image audit passed;
  the host reached HS attach but still timed out on the address-0 descriptor
  (`-110`). The gate produced no controlled stop, so no same-window SOF
  progress was demonstrated; Android fallback and automatic Fastboot return
  completed normally.
- Run `259639.0` used the existing source-backed `phyretry` gate, which
  re-initializes the USB2 PHY after the link reaches U0. It produced the same
  HS-attach / descriptor `-110` classification, with no `1234:0001`; the
  Android fallback and automatic Fastboot return again completed normally.
- Run `279359.0` combined the source-backed HS-PHY EUD bypass with the
  qpr1-compatible gadget-start-at-final-Run/Stop experiment. It still reached
  HS attach and failed at the address-0 descriptor with `-110`/`-71`, produced
  no `1234:0001`, and returned through Android to Fastboot automatically.
- Run `283504.0` used the `event-cut` diagnostic gate. It produced no
  controlled stop, while the host again saw the same zero-payload descriptor
  failure. This is consistent with no DWC3 event-ring record being observed
  by the polling gate during the attach window; host timing makes this a
  boundary diagnostic rather than proof of a particular electrical cause.
- Runs `287432.0` and `291548.0` repeated the `utmi-progress` diagnostic. The
  retained same-boot progress mask was `2` in both observations: the initial
  setup TRB retired, but no event-delivery, setup-payload, or SOF-progress bit
  was observed. Both runs still failed at the same descriptor boundary and
  recovered automatically.
- Run `294979.0` preserved the bootloader-provided USB2 PHY interface field,
  matching the Bramble DT's lack of an explicit `phy_type`/HS-PHY interface
  property. It did not change the HS-attach / descriptor `-110` result.
- Run `299942.0` re-armed the Android-order USB2 clock branches
  (`iface`/`core`/`sleep` before UTMI). It did not change the result: the host
  again reached HS attach, then failed on the zero-payload descriptor.
- Run `312633.0` selected the Bramble DT's Android HS core-clock performance
  state (`core-clk-rate-hs=66.666667 MHz`) instead of the normal Nominal vote.
  QEMU, image audit, and RAM-only boot passed; usbmon again recorded one
  zero-payload `-2` completion followed by three zero-payload `-71`
  completions, and the host failed at the same address-0 descriptor boundary.
  Android fallback and automatic Fastboot return completed normally.
- Run `308523.0` used the read-only DWC3 GDBGLTSSM timing gate. It did not
  produce a controlled Fullerene disconnect or a new USB identity; the host
  again reached HS attach and failed at the same descriptor timeout before
  Android/Fastboot recovery.
- Run `317617.0` repeated the read-only raw-link-state gate with the canonical
  direct-handoff profile. It did not produce a controlled Fullerene disconnect
  or a trustworthy link-state readout; the host again reached HS attach and
  failed at the same descriptor timeout before automatic recovery.
- Run `321699.0` repeated that raw-link-state gate with a 60-second observation
  window, long enough to include the roughly 40-second Fastboot-disconnect to
  HS-attach delay. It still produced no controlled disconnect or new readout;
  usbmon again recorded one zero-payload `-2` completion followed by three
  zero-payload `-71` completions, then Android/Fastboot recovery completed.
- Run `330525.0` used the source-backed `lnkrawdb` DSTS-word/time-bucket
  readout with the same 60-second observation window. It produced no
  host-visible timing discriminator, controlled disconnect, or Fullerene
  descriptor; the host again saw `-110` followed by `-71` retries and the
  automatic Android/Fastboot recovery completed.
- Run `337851.0` routed the probe through the Type-C parent IRQ path. It did
  not produce the expected controlled Type-C disconnect or a new identity;
  the host again saw zero-payload descriptor `-110`/`-71` failures and the
  automatic Android/Fastboot recovery completed.
- Run `342866.0` placed the read-only raw-link-state gate at the 46-second
  point, overlapping the host's descriptor-retry window. It still produced
  no controlled Fullerene disconnect or host-visible readout; the host again
  saw one zero-payload `-2` completion followed by three zero-payload `-71`
  completions after HS attach, then automatic Android/Fastboot recovery.
- Run `348929.0` used the source-backed DWC3 internal event-queue
  `SPACE_AVAILABLE` readout after the descriptor observation window. It did
  not produce a host-visible readout or Fullerene identity; the host again
  reached HS attach and saw the same zero-payload `-110`/`-71` descriptor
  failure before Android/Fastboot recovery.
- Run `353079.0` used the stage-0 DWC3 event-queue free-space readout. It
  produced no Fullerene attach/reconnect sequence; the only later SuperSpeed
  identities were stock Android recovery identities (`18d1:4ee7`/`18d1:4ee0`),
  so the stage readout is unavailable through this transport and remains a
  qualified non-discriminator rather than evidence of an empty hardware queue.
- Run `363507.0` removed only the source-exact Run/Stop guard while retaining
  qpr1 USB2 SUSPHY, HS-PHY, DMA, and start-after-connect settings. It still
  produced HS attach with zero-payload descriptor `-110`/`-71` failures and
  automatic Android/Fastboot recovery; this one-variable A/B did not restore
  USB2 RX or EP0 response.
- Run `368463.0` routed the same source-backed profile through the Type-C
  role-notification parent IRQ path. It still produced HS attach with a
  zero-payload descriptor `-110` followed by `-71` retries, with no Fullerene
  identity or EP0 response; automatic Android/Fastboot recovery completed.
- Run `374656.0` removed only `--start-after-connect`, allowing the
  source-confirmed pre-Run/Stop EP0 SETUP STARTTRANSFER order. This changed the
  boundary: no Fullerene or HS attach appeared, while stock Android later
  returned as SuperSpeed identities during automatic recovery. The pre-connect
  ordering is therefore not a fix and is not a reason to abandon the deferred
  start used by the attach-reaching profile.
- Run `381615.0` attempted to publish the corrected aliased-SETUP progress
  mask with a 10-second gate window. The gate evaluated before Bramble's
  roughly 37-second HS attach point, so it produced no Fullerene/HS attach and
  only the normal stock Android recovery identities; no progress code was
  readable.
- Run `385191.0` repeated that readout with a 45-second window. It likewise
  produced no Fullerene/HS attach before recovery, so the timing-gate transport
  remains unavailable for this measurement; no new USB conclusion is inferred.
- All twenty runs passed QEMU preflight and image audit, used RAM-only
  `fastboot boot`, and completed the automatic Android fallback/Fastboot
  recovery. No flash, erase, unlock, slot mutation, configfs rebind, or other
  persistent device operation was used.
- The runner already automates Android/ADB to Fastboot recovery by default:
  it issues `adb reboot bootloader` and waits for Fastboot when ADB is visible.
  It also retries transient mixed USB snapshots while waiting for recovery.
  A `device-absent` state has no USB transport, so it can only wait and record
  the required physical recovery; it cannot manufacture a Fastboot command.
- When the handset is absent, no ADB, Fastboot, build, or boot operation is
  issued. The candidate runner records a bounded recovery wait and can resume
  the same candidate after Android/Fastboot becomes visible.

## Fixed hardware and safety contract

| Item | Fixed value / rule |
| --- | --- |
| Device | Pixel 4a 5G / Bramble / Qualcomm SM7250 (Lito) |
| Serial | `26191JECB00076` |
| Bootloader | `b5-0.6-10489838`, unlocked |
| Allowed device operation | RAM-only `fastboot boot`; read-only ADB/Fastboot/USB observations |
| Forbidden operations | flash, erase, partition mutation, unlock, slot mutation, factory reset, configfs rebind, analyzer, user-data operation |
| Fullerene identity | `1234:0001`, only valid when returned by Fullerene's own USB stack |
| DWC3 wrapper | `0x0a600000`; child register window `0xcd00` |
| Apps-SMMU stream | `0xe0` |
| DMA pool | `0x90000000..0xf0000000` |
| USB2 PHY | `0x088e3000`; `GCC_QUSB2PHY_PRIM_BCR` |

## Decisive boundary

| Observed boundary | Meaning | Action |
| --- | --- | --- |
| No Fullerene attach | Failure is before host-visible USB identity | Investigate handoff / PHY ownership only |
| HS attach, no descriptor payload | USB2 link/pull-up exists, but no usable RX/SOF or EP0 response is proven | Obtain a source-backed or known-good USB2 PHY RX/SOF / event-ingress discriminator |
| Valid `1234:0001` descriptor | EP0 data path is alive | Only then inspect downstream endpoint/TRB details |
| Device absent | No transport exists for recovery | Wait only; do not issue device commands |

## Closed or deliberately deferred branches

The following families have already been covered by source audits and/or
isolated A/B runs without moving the pre-descriptor boundary:

- UTMI width/timing, reference-clock and clock-stability variants
- USB2 HS-PHY reset, SUSPHY, rail, voltage, and source-order variants
- DWC3 reset, Run/Stop, DEVTEN, DALEPENA, DBM, event-ring, resource-order,
  and gadget-start variants, including the qpr1-style final-Run/Stop start
- USB2 PHY interface preservation, Android-order USB2 clock-branch re-arm, and
  the event-ingress/EP0/SOF progress diagnostics
- EP0 MPS, SETUP timing, TRB form, and downstream EP0 permutations
- SuperSpeed QMP, lane, Type-C, VBUS, and old-session cleanup variants
- Factory XBL/ABL and Android-init profile replays

The exact run-by-run evidence, commands, timestamps, artifact hashes, and
negative results remain in the [full status history](../evidence/bramble/CONTEXT_STATUS_FULL.md.gz).

## Next useful work

1. Progress now requires external evidence: a known-good USB2 PHY comparison,
   USB protocol analyzer, or permitted JTAG/secure-debug register capture at
   the PHY RX/SOF and DWC3 event-ingress boundary.
2. The prescribed pre-DTB/post-DTB normal Android-init pair and the standalone
   attach-reaching path have been compared without discrimination; no further
   guessed EP0/TRB or packet-format A/B is justified.
3. If the handset returns, retain the safety log and use only an externally
   motivated, source-backed change. Android-to-Fastboot recovery is automatic
   when ADB is visible; only a physically absent handset still needs a
   cable/button recovery.
4. Do not add guessed EP0/TRB, packet-format, or register mutations while the
   host still receives no Fullerene descriptor payload.

## Loading policy

| Task | Read by default | Read only when needed |
| --- | --- | --- |
| Bramble status / planning | This file | Full status history |
| DT, USB, AArch64 hardware contract | This file and [compact hardware notes](HARDWARE_aarch64.md) | Full AArch64 ledger |
| Cross-platform hardware | [HARDWARE.md](HARDWARE.md) | Its Bramble section only if required |
| Build / image audit | [BUILD.md](BUILD.md) | Exact run evidence |
| Attribution experiment | [BRAMBLE_USB_20260909_ATTRIBUTION.md](BRAMBLE_USB_20260909_ATTRIBUTION.md) | — |

The full ledgers are evidence archives, not default context. Use a Run ID or
the relevant source-audit topic to retrieve a targeted section.
