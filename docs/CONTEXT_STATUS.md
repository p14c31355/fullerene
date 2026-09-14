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
- The current source-exact control remains the reference path. Runs
  `554551.0`, `567394.0`, and `571352.0` tested, respectively, the qpr1
  device-core reset boundary, clearing the non-DT `GCTL.U2EXIT_LFPS` bit,
  and a source-exact `dwc3_set_prtcap(DEVICE)` tail that writes only
  `PRTCAPDIR`. All three reached HS attach before the same zero-payload
  descriptor timeout/error sequence and produced no Fullerene identity.
  The Factory-ABL `usb_shared_hs_phy_init()` cleanup and settle-delay A/B,
  the separate HS-PHY SLEEPM-clear A/B, and a later DCFG SuperSpeed A/B
  (`542605.0`) likewise did not provide a USB fix; the latter removed even
  the HS attach.
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
- Run `499758.0` repeated the current source-exact attach-reaching control after
  the aliased-SETUP diagnostic correction. The artifact passed QEMU preflight,
  image audit, and RAM-only `fastboot boot`; the host saw HS attach, then an
  address-0 Device Descriptor timeout (`-110`) followed by three zero-payload
  `-71` retries. The passive usbmon capture is recorded at
  `tmp/fullerene-bramble-loop.499758.0/usbmon-all.bin` (SHA-256
  `3057e8b6865e5a0eb20c6e80241c736f43f14b7896a8550499de54c984600c4d`). No
  `1234:0001` appeared; stock Android returned after 68 seconds and the
  runner automatically restored Fastboot. The boot artifact SHA-256 is
  `05d3c911dd93bb06d317ed9103dac965f715dec2680305c0931f7d57df8dd527`.
- Run `511007.0` repeated the same attach-reaching control with passive usbmon
  and read-only stock-Android USB observability. QEMU preflight, image audit,
  and RAM-only `fastboot boot` passed; the host again reached HS attach, then
  saw a zero-payload address-0 descriptor timeout (`-110`) followed by three
  zero-payload `-71` retries. The artifact SHA-256 remained
  `05d3c911dd93bb06d317ed9103dac965f715dec2680305c0931f7d57df8dd527` and
  `usbmon-all.bin` SHA-256 is
  `ff618bdd29403bdd690ea401b443ca09042c707db8216acb428f52545e82ecc4`.
  After Android fallback, the read-only capture showed `sys.usb.controller`
  and `/sys/class/udc` both using `a600000.dwc3`; stock Linux reported the
  DWC3 line as IRQ 272, which is DT GIC SPI 240 plus Linux's 32-offset. The
  EUD line remained at zero, and USB debugfs was unavailable to the authorized
  shell. This confirms the known-good Android controller/UDC identity but does
  not expose the PHY RX/SOF state needed to explain Fullerene's zero payload.
  Stock Android returned with `bootreason=watchdog`, and the runner restored
  Fastboot automatically.
- Run `529570.0` tested the previously untried Factory-ABL shared HS-PHY
  cleanup/settle sequence (`--abl-shared-hs-phy`) on the same source-exact
  direct USB2 control. QEMU preflight, image audit, and RAM-only `fastboot
  boot` passed; the host again reached HS attach, then saw a zero-payload
  address-0 descriptor timeout (`-110`) followed by three zero-payload `-71`
  retries. No `1234:0001` appeared. Stock Android returned after 69 seconds,
  and the runner automatically restored Fastboot. This source-backed A/B did
  not change the pre-descriptor boundary.
- Run `536156.0` tested the separate HS-PHY `UTMI_CTRL0.SLEEPM` clear after
  source-exact analog initialization (`--hsphy-clear-sleepm`). The wrapper was
  extended to pass this existing main build option; QEMU preflight, image
  audit, and RAM-only `fastboot boot` passed. The host again reached HS attach,
  then saw zero-payload descriptor `-110`/`-71` failures, with no `1234:0001`.
  Stock Android returned after 69 seconds and the runner restored Fastboot
  automatically. The A/B did not change the boundary; the artifact SHA-256
  was `7edffc324384f5cd611ad1b8c768239d14b90bec358c3266fbd7472bc1efa`.
- Run `542605.0` tested keeping `DCFG.SPEED=SuperSpeed` at the direct USB2
  Run/Stop boundary (`--dcfg-superspeed`), matching the platform's declared
  maximum speed. QEMU preflight, image audit, and RAM-only `fastboot boot`
  passed, but no Fullerene identity or HS attach appeared during the bounded
  window; only the automatic stock-Android SuperSpeed fallback was observed.
  The result is a negative control, not a USB fix; the runner restored
  Fastboot automatically.
- Run `554551.0` tested the source-backed qpr1 device-core soft-reset A/B
  (`--usb2-core-reset-at-runstop`) on the attach-reaching direct USB2 profile.
  QEMU preflight, image audit, and RAM-only `fastboot boot` passed; the host
  again saw HS attach followed by one zero-payload `-110` descriptor timeout
  and three zero-payload `-71` retries. No `1234:0001` appeared; Android
  fallback and automatic Fastboot return completed. Artifact SHA-256:
  `c432107a5c30aa5489c3cc1e3805c9ce5601a0cc5b67fa3d7558df9fd2c79688`.
- Run `567394.0` tested the qpr1 DT/source correction that leaves
  `GCTL.U2EXIT_LFPS` clear by default. It preserved the same direct profile
  and recovery contract, but the host result was unchanged: HS attach,
  zero-payload `-110`/`-71` descriptor failures, no `1234:0001`, then stock
  Android and automatic Fastboot return. Artifact SHA-256:
  `19005d8eb0bdbe2d9b8472a12df22701ca0bc3c5a4b7547325bed1bb6af893f7`.
- Run `571352.0` tested the source-exact qpr1 device-mode tail: after
  selecting `PRTCAPDIR=DEVICE`, Fullerene no longer forced qpr1-inapplicable
  `U2RSTECN`/`PWRDNSCALE` policy, while the optional `--u2exit-lfps` remained
  off. QEMU preflight, image audit, and RAM-only `fastboot boot` passed; the
  host again recorded HS attach followed by zero-payload `-110`/`-71`
  descriptor failures, with no `1234:0001`. Android fallback and automatic
  Fastboot return completed. Artifact SHA-256:
  `9adbfc8972673acc1f2f3c09a40586164a3b48a0c99644c138e3a3cca3eab854`.
- Run `586035.0` replayed the regenerated normal Android-init candidate pair.
  Both pre-DTB and post-DTB artifacts passed QEMU preflight, image audit, and
  RAM-only `fastboot boot`; neither produced Fullerene USB or `1234:0001`.
  After the bootloader disconnect, the host saw no Android/Fullerene transport
  during the bounded recovery windows and both candidates classified as
  `google-logo-or-software-unrecoverable-suspected`. The handset was then
  manually recovered to Fastboot; no persistent device operation was used.
- Run `613956.0` tested the qpr1 source-order variant that arms EP0 SETUP
  before the final Run/Stop boundary, while retaining source-exact device reset,
  USB2 SUSPHY, qpr1 DEVTEN, command guards, Run/Stop, and source-exact HS-PHY.
  QEMU preflight, image audit, and RAM-only `fastboot boot` passed, but Fullerene
  never attached and no `1234:0001` or Fullerene descriptor request appeared in
  usbmon. The host returned through stock Android `18d1:4ee7` after 68 seconds,
  then Fastboot `18d1:4ee0`; usbmon contained only the stock Android descriptor
  submissions. This confirms that the current attach-reaching baseline must keep
  SETUP deferred until after connect. Artifact SHA-256:
  `64a2be7097ee392aab9b0d054f53222a6f42e96651915ce319d2b9cca4499459`.
- Run `626011.0` added only the qpr1 50 ms minimum stop-to-start interval
  (`--min-runstop-delay`) to the current source-exact, attach-reaching USB2
  profile. QEMU preflight, image audit, and RAM-only `fastboot boot` passed;
  the artifact SHA-256 is
  `01bf4a1b46c5f0bd6b679711aa8b17de84e22b03f958585009af7a137ed7f98c`.
  The host again reached Fullerene HS attach at `10:14:31 JST`, then the
  address-0 Device Descriptor timed out with `-110` at `10:14:37`; usbmon
  recorded one zero-payload `-2` completion followed by three zero-payload
  `-71` retries. No `1234:0001` appeared. Stock Android `18d1:4ee7` returned
  after 69 seconds with `bootreason=watchdog`, and the runner restored Fastboot
  `18d1:4ee0` automatically. The 50 ms qpr1 timing delta did not move the
  attach-to-descriptor boundary.
- Run `638702.0` re-tested the current attach-reaching profile while preserving
  the bootloader-provided USB2 PHY interface field
  (`--usb2-preserve-phy-interface`), with the current qpr1 device-reset,
  DEVTEN, command-guard, Run/Stop, SUSPHY, core-reset-at-Run/Stop, source-exact
  HS-PHY, no-SMMU, EUD-bypass, and deferred-SETUP controls. QEMU preflight,
  image audit, and RAM-only `fastboot boot` passed; the artifact SHA-256 is
  `9dd53c82cc96bca6767a9644180b5be7f42a1d4ceb24643ecd15fe76db334b7a`.
  The host reached Fullerene HS attach at `10:22:45 JST`, then the address-0
  Device Descriptor timed out with `-110` at `10:22:51`; usbmon recorded one
  zero-payload `-2` completion followed by three zero-payload `-71` retries.
  No `1234:0001` appeared. Stock Android `18d1:4ee7` returned at `10:23:12`
  with `bootreason=watchdog`, and the runner restored Fastboot `18d1:4ee0`
  automatically at `10:23:24`. Preserving the PHY interface therefore did
  not move the attach-to-descriptor boundary.
- Run `648064.0` was an exploratory test of the qpr1 source order that
  initializes/resets the Bramble HS-PHY before DWC3 `DCTL.CSFTRST`
  (`--hsphy-before-reset`). Its command omitted the PHY-interface preservation
  flag used by `638702.0`, so it is not a single-variable A/B; it still
  reproduced the same boundary. QEMU preflight, image audit, and RAM-only
  `fastboot boot` passed; the artifact SHA-256 is
  `bd7e89a143228962fb11cb8d1f274fa2bdb5ceb8f0b6da4a8e932d772333b9e7`.
  The host reached Fullerene HS attach at `10:28:50 JST`, then the address-0
  Device Descriptor timed out with `-110` at `10:28:55`; usbmon recorded one
  zero-payload `-2` completion followed by three zero-payload `-71` retries.
  No `1234:0001` appeared. Stock Android `18d1:4ee7` returned at `10:29:17`
  with `bootreason=watchdog`, and Fastboot `18d1:4ee0` returned at
  `10:29:27`.
- Run `663720.0` repeated the qpr1 pre-reset HS-PHY ordering with the
  `638702.0` PHY-interface preservation flag restored, making
  `--hsphy-before-reset` the only intended A/B variable. QEMU preflight, image
  audit, and RAM-only `fastboot boot` passed; the artifact SHA-256 is
  `6f24131195aecceecd4aaf5f0e42a6f4a33f4f98bec7bfb9197084d6cbf267bd`.
  Fullerene HS attach occurred at `10:40:10 JST`, followed by the address-0
  Device Descriptor timeout `-110` at `10:40:15`; usbmon again recorded one
  zero-payload `-2` completion and three zero-payload `-71` retries. No
  `1234:0001` appeared. Android `18d1:4ee7` returned at `10:40:36` with
  `bootreason=watchdog`, and Fastboot `18d1:4ee0` returned at `10:40:44`.
  The qpr1 pre-reset PHY order therefore did not move the boundary.
- Run `669578.0` enabled the stock DT's `snps,has-lpm-erratum` behavior at
  the Android HS Connect-Done boundary (`--android-hs-lpm --android-lpm-errata`)
  on the current attach-reaching profile. QEMU preflight, image audit, and
  RAM-only `fastboot boot` passed; the artifact SHA-256 is
  `735544f5c37b07a4adb94fd8449cbec9347809663131f532cc9efd5455e7989c`.
  Fullerene HS attach occurred at `10:42:44 JST`, followed by the address-0
  Device Descriptor timeout `-110` at `10:42:50`; usbmon again recorded one
  zero-payload `-2` completion and three zero-payload `-71` retries. No
  `1234:0001` appeared. Android `18d1:4ee7` returned at `10:43:11` with
  `bootreason=watchdog`, and Fastboot `18d1:4ee0` returned at `10:43:22`.
  The stock HS-LPM erratum handling did not move the boundary.
- Run `682713.0` enabled the existing direct-handoff EP0 TX FIFO repair
  (`--ep0-txfifo-fix`), which raises a degenerate `GTXFIFOSIZ(0)` depth while
  preserving its start address. QEMU preflight, image audit, and RAM-only
  `fastboot boot` passed; the artifact SHA-256 is
  `f63574dfdfa3d1541d5b88c1804b8427723d22ba1039ccc8094fa1e7c63e0c75`.
  The host reached HS attach at `10:49:51 JST`, then the address-0 Device
  Descriptor timed out with `-110` at `10:49:56`; usbmon recorded the same
  zero-payload `-2` completion followed by three `-71` retries. No `1234:0001`
  appeared. Android returned at `10:50:17` and Fastboot at `10:50:25`.
  The EP0 TX FIFO repair therefore did not move the boundary.
- Run `687622.0` replayed qpr1's `dwc3_msm_block_reset(false)` DBM
  reset/enable immediately before direct USB2 gadget start
  (`--usb2-android-dbm-reset`) while retaining the source-exact PHY interface.
  QEMU preflight, image audit, and RAM-only `fastboot boot` passed; the artifact
  SHA-256 is `b0f899f13937b072a4474f6a8ed11c0c43ca0daee0bb7042d7c5ec75590cb209`.
  HS attach occurred at `10:52:52 JST`, followed by the same zero-payload
  descriptor `-110`/`-71` sequence at `10:52:57`; no Fullerene identity
  appeared. Android returned at `10:53:18` and Fastboot at `10:53:30`. The
  qpr1 DBM ownership boundary did not move the result.
- The `u2-freeclk` A/B had a harness issue on its first attempt: the
  source-exact PHY-interface-preserve fast path returned before applying the
  independent free-clock bit. That path was corrected so the explicit A/B
  writes and reads the capability bit without changing PHYIF/TRDTIM. The
  corrected Run `696508.0` set the qpr1 source-default
  `GUSB2PHYCFG.U2_FREECLK_EXISTS` bit (`--u2-freeclk-set`) on the current
  profile. QEMU preflight, image audit, and RAM-only `fastboot boot` passed;
  artifact SHA-256 is
  `7cfb7cbec77eb1b2ef311305ed23ff835715d28a2f886836ea8eb9f51f1c8a27`.
  HS attach occurred at `10:57:57 JST`, the address-0 descriptor timed out
  with `-110` at `10:58:02`, and usbmon again recorded three zero-payload
  `-71` retries. No `1234:0001` appeared; Android returned at `10:58:23` and
  Fastboot at `10:58:31`. The corrected free-clock A/B did not move the
  boundary.
- Run `704579.0` exercised the tracked normal Android-init pre-DTB candidate
  with the intended DMA-cache A/B and passive usbmon (`--android-init
  --early-usb-before-dtb-scan --dma-cache-maintenance`). QEMU preflight, image
  audit, and RAM-only `fastboot boot` passed; the artifact SHA-256 was
  `fd364f28d3f4f06cb0bf0fe2b03b8b29b65db7b7df87be4263f76007c6bb6f82`.
  Unlike the attach-reaching direct profile, the host saw no Fullerene,
  Android, or bootloader transport after the Fastboot disconnect; passive
  usbmon captured `918572` bytes but no usable Fullerene identity. The bounded
  recovery wait expired with final state `device-absent`, classified as
  `google-logo-or-software-unrecoverable-suspected`, so the post-DTB candidate
  was not issued. No persistent device operation was used; the handset now
  needs physical recovery to Fastboot before another run.
- A subsequent source audit found that the `--dma-cache-maintenance` cfg was
  generated but was not consumed by `cache_clean()` / `cache_invalidate()`;
  with the stock coherent DT path, Run `704579.0` therefore did not actually
  force `dc cvac`/`dc ivac` and is not a valid cache-maintenance A/B. That
  implementation is now corrected. The build-only corrected candidate hashes
  are pre-DTB `6d071ae308fb54a33b7dcc06aaed5270266f2ca75b3ba47da3e8631c247b5048`
  and post-DTB
  `5131d44b5c37ce7f6a45c6f05f591b368895b816524f33c4566fe239ee91d9cf`; neither
  has been booted on hardware yet.
- Run `1131105.0` hardware-ran both corrected cache-maintenance candidates.
  The pre-DTB artifact SHA-256 was
  `6d071ae308fb54a33b7dcc06aaed5270266f2ca75b3ba47da3e8631c247b5048` and
  the post-DTB artifact SHA-256 was
  `5131d44b5c37ce7f6a45c6f05f591b368895b816524f33c4566fe239ee91d9cf`.
  Both passed QEMU preflight, image audit, and RAM-only `fastboot boot`, but
  neither produced Fullerene USB. The host saw only bootloader descriptor
  requests and both attempts classified as `fastboot-fallback`; the handset
  returned to Fastboot after each attempt. Passive usbmon captured
  `919840` bytes for pre-DTB and `710320` bytes for post-DTB, with only
  descriptor-submit records (`status=-115`) in the candidate summaries.
- Run `1143741.0` tested the late-MMU normal Android-init ordering: the same
  source-exact USB2 and corrected DMA-maintenance settings, but without
  `--early-usb-handoff` or `--early-usb-before-dtb-scan`. QEMU preflight and
  image audit passed, the artifact SHA-256 was
  `924b3d19fa7589733d816be2892c86ef9e51e129ce7d8a55cb1f4717b911e17a`, and
  RAM-only `fastboot boot` was accepted. It produced no Fullerene or Android
  USB and ended `device-absent` with classification
  `google-logo-or-software-unrecoverable-suspected`; physical Fastboot
  recovery was required afterward. This rules out the late-MMU ordering as a
  safe discriminator for the current normal-path failure.
- A source audit after `1143741.0` found that Android-init enters the
  non-returning `launchd::run()` path, so the normal boot-loop `usb::poll()` is
  no longer reached after user space starts. The controller-IRQ path already
  polls the DWC3 ring, but there was no bounded fallback when that SPI is not
  delivered. The source-backed fix adds a narrow timer-IRQ EP0/event-ring
  poll, gated by a successful Bramble handoff and limited to event consumption
  plus SETUP re-arm; it does not run Type-C, power, SMMU, or diagnostic
  Run/Stop work in timer context. The late normal path now marks the same
  successful-handoff state used by the fallback.
- Build-only validation of that timer-IRQ fix passed QEMU preflight and the
  Bramble image audit. The unbooted artifact is
  `tmp/bramble-android-init-timer-irq.img` with SHA-256
  `b8a595c467e6280df44aed8a2c2e5c7c6df59886b49658a24415dcb2f8304734`.
  It has not been issued to the handset; physical Fastboot recovery remains
  the prerequisite for the next bounded RAM-only test.
- The Android/Qualcomm DWC3 source comparison also confirmed that the
  `pwr_event` IRQ is a low-power wake/resume route, not an always-on initial
  gadget IRQ. Fullerene now keeps that GIC SPI masked during initial
  enumeration and toggles it only at the runtime suspend/resume boundary.
  Static tests passed; the updated unbooted artifact is
  `tmp/bramble-android-init-timer-irq-pwr-masked.img` with SHA-256
  `e569e503c87ec97cdcd3de4a74ec5a6f6058bef393950de94db01f28f2b42446`.
- A controlled build-only HS-PHY A/B now explicitly selects the official
  Bramble private-DT override pairs (`0x67/0x6c`, `0xc8/0x70`) while retaining
  the timer-IRQ fallback, DMA cache maintenance, and masked initial
  `pwr_event` route. QEMU preflight and the Bramble boot-image audit passed.
  The unbooted artifact is `tmp/bramble-android-init-dtbo-pvt.img` with
  SHA-256 `4baa963c0b0ccc89e4d26b6d34adb031fdc90950a3adc45960bb89220bde728e`;
  it has not been issued to the handset.
- The DT override is now exposed as the explicit
  `--usb-gadget-handoff-hsphy-dtbo-bramble-pvt` option in `flasks build` and
  `bramble-usb loop`; the CLI-generated image
  `tmp/bramble-android-init-dtbo-pvt-cli.img` has the same SHA-256 and passed
  the same preflight/audit. This removes the prior need to inject the build
  environment variable manually.
- The first short runner-gate execution using that explicit option is recorded
  as `1189958.0`. It saw `device-absent` before boot, stopped with
  `device 26191JECB00076 is not available in Fastboot`, and issued no
  `fastboot boot` or persistent device operation.
- Run `1241052.0` re-tested the direct USB2 profile with the qpr1-inspired
  `--hsphy-before-reset` ordering as the only intended hardware variable. QEMU
  preflight, image audit, and RAM-only `fastboot boot` passed; the artifact
  SHA-256 is
  `3beb4ac490921837e4f8ed131b88126a005683d2f2c9eb1bda37842f6585cb90`.
  The host again reached HS attach, then failed the address-0 Device Descriptor
  with `-110` followed by `-71` retries. No `1234:0001` appeared; Android
  `18d1:4ee7` and Fastboot `18d1:4ee0` returned automatically. The pre-reset
  HS-PHY ordering therefore did not move the attach-to-descriptor boundary.
- Run `1245324.0` issued the source-backed Android-init candidate containing
  the timer-IRQ event-ring fallback, initial `pwr_event` masking, and explicit
  Bramble private-DT HS-PHY override. QEMU preflight, image audit, and
  RAM-only `fastboot boot` passed; the artifact SHA-256 is
  `33c6fbf12ced52251d9c3a6394308999bdaa5f215ba138ebdcc5c6b6231f2384`.
  It produced no Fullerene, Android, or bootloader USB transport after the
  Fastboot disconnect, and no descriptor request was captured. The bounded
  recovery wait ended `device-absent` with
  `google-logo-or-software-unrecoverable-suspected`; the handset now requires
  physical recovery to Fastboot. No persistent device operation was used.
- Run `1269310.0` was a negative control based on a non-Bramble Qualcomm EUD
  implementation: it reasserted the HS-PHY rails, wrote `PWRDOWN_B` at offset
  `0xa4`, waited 50 ms, and preserved the EUD state. QEMU preflight, image
  audit, and RAM-only `fastboot boot` passed; artifact SHA-256 was
  `13520f06814fa194696600411c120350d44591171c74208c08976800b6236ccd`.
  The host again reached HS attach, then saw the address-0 Device Descriptor
  timeout `-110` and no-payload retries; usbmon recorded no Fullerene
  descriptor and no `1234:0001`. Android fallback occurred and the runner
  automatically returned the handset to Fastboot. The actual Bramble qpr1
  source only checks EUD ownership and returns without this write, so the
  implementation and CLI A/B have been removed; the run remains evidence
  against that non-Bramble hypothesis.
- Run `1294745.0` replayed the source-exact direct USB2 control after removing
  that non-Bramble EUD write and using the stock qpr1 HS-PHY override table.
  QEMU preflight, image audit, and RAM-only `fastboot boot` passed; artifact
  SHA-256 was
  `d2ffd256f5208a5e5119fe26fdfa008b0602c341250b1b7edddc0e8626f21bbd`.
  The result was unchanged: host HS attach, address-0 descriptor `-110`, then
  three zero-payload `-71` retries, no `1234:0001`, Android fallback, and
  automatic Fastboot recovery. This confirms the source-faithful cleanup and
  stock qpr1 table do not move the boundary.
- Run `1299233.0` tested the official qpr1 DWC3 controller link-clock/core-reset
  boundary (`--android-block-reset`) on the same attach-reaching profile.
  QEMU preflight, image audit, and RAM-only `fastboot boot` passed; the
  artifact SHA-256 was
  `1a75cd244cd3610ba414888809133707018d25c33c39f91e4ebab84cefee9fb6`.
  It reproduced the same HS attach, address-0 `-110`, and three zero-payload
  `-71` retries, followed by Android fallback and automatic Fastboot return.
- Run `1304051.0` combined that controller reset boundary with qpr1's
  `dwc3_dis_sleep_mode()` USB2 controls (`--android-block-reset
  --usb2-dis-sleep-mode`). QEMU preflight, image audit, and RAM-only
  `fastboot boot` passed; the artifact SHA-256 was
  `bab13e806dd9d101f01f64d652f02f852fb896db9fe53a3f4196dcd42d4901c4`.
  The result remained unchanged: HS attach, address-0 `-110`, three
  zero-payload `-71` retries, no `1234:0001`, then automatic Android/Fastboot
  recovery.
- The USB diagnostic quiet-window contract was tightened: both the ordinary
  poll loop and the Android-init timer fallback now check the quiet deadline
  before reading any DWC3 MMIO status. Formatting, diff checks, and the
  Fullerene/flasks test suites pass after this source-only safety correction.
- All recorded runs through `1143741.0` passed QEMU preflight and image audit,
  used RAM-only `fastboot boot`, and preserved the no-flash/erase/unlock/slot
  mutation/configfs-rebind safety contract. The latest late-MMU candidate
  stopped at the device-absent recovery gate and requires physical recovery.
- The runner already automates Android/ADB to Fastboot recovery by default:
  it issues `adb reboot bootloader` and waits for Fastboot when ADB is visible.
  It also retries transient mixed USB snapshots while waiting for recovery.
  A `device-absent` state has no USB transport, so it can only wait and record
  the required physical recovery; it cannot manufacture a Fastboot command.
- When the handset is absent, no ADB, Fastboot, build, or boot operation is
  issued. The candidate runner records a bounded recovery wait and can resume
  the same candidate after Android/Fastboot becomes visible.
- The candidate plan's SHA gate now tracks the corrected build-only hashes
  above; the earlier `704579.0` image remains preserved as a separate safety
  record and will not be reused for the corrected A/B.

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
  the qpr1 free-clock capability bit and the event-ingress/EP0/SOF progress
  diagnostics
- EP0 TX FIFO resource, EP0 MPS, SETUP timing, TRB form, and downstream EP0
  permutations
- SuperSpeed QMP, lane, Type-C, VBUS, and old-session cleanup variants
- Factory XBL/ABL and Android-init profile replays

The exact run-by-run evidence, commands, timestamps, artifact hashes, and
negative results remain in the [full status history](../evidence/bramble/CONTEXT_STATUS_FULL.md.gz).

## Next useful work

1. Progress now requires external evidence: a known-good USB2 PHY comparison,
   USB protocol analyzer, or permitted JTAG/secure-debug register capture at
   the PHY RX/SOF and DWC3 event-ingress boundary.
2. The corrected DMA-maintenance pair, late-MMU ordering, direct pre-reset
   HS-PHY ordering, and the Android-init timer-IRQ/pwr-event/DT override
   candidate were hardware-tested without a Fullerene descriptor. Further
   progress requires external PHY RX/SOF or DWC3 event-ingress evidence.
3. The handset is currently recovered in Fastboot (`26191JECB00076`); the
   latest bounded run returned automatically after Android fallback. Keep the
   Fastboot gate before another RAM-only run.
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
