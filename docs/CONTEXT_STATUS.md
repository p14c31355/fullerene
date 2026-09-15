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

## Current state (last evidence update: 2026-09-15)

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
- Run `1343716.0` repeated the source-exact attach-reaching control from a
  freshly booted stock Android state, with automatic ADB-to-Fastboot
  transition, RAM-only `fastboot boot`, Fullerene usbmon capture, and automatic
  Android-to-Fastboot recovery enabled. QEMU preflight, image audit, and
  `fastboot boot` passed; artifact SHA-256 is
  `d2ffd256f5208a5e5119fe26fdfa008b0602c341250b1b7edddc0e8626f21bbd`.
  Fullerene HS attach occurred at `19:58:02 JST`; the host then submitted the
  standard address-0 Device Descriptor request (`GET_DESCRIPTOR`, device,
  `wLength=64`) but received no data: one `-110` timeout at `19:58:07`, then
  three zero-payload `-71` completions at `19:58:07`. The independent Wireshark
  capture is
  `tmp/tshark-bramble-android-to-fastboot-20260914/usbmon-all.pcapng` with
  SHA-256
  `5d349bc8cfe4053c0fd13a6d659d9f05bc9112b7c37a137e706c04774154610e`.
  Stock Android returned at `19:58:28`, and the runner restored Fastboot
  automatically at `19:58:36`; no manual handset restart was needed. This
  warm-Android A/B therefore does not resolve `-71` and confirms the failure is
  pre-descriptor-response, not malformed Fullerene descriptor data.
- Run `1358269.0` added the read-only `--signal-probe --signal-cmd-gate
  always` flow-map instrumentation to the same attach-reaching profile. The
  QEMU preflight, image audit, RAM-only `fastboot boot`, and automatic Android
  to Fastboot recovery all completed. The host saw one Fullerene high-speed
  attach, but no additional host-visible disconnect/re-enumeration beacon; the
  independent tshark capture recorded three immediate zero-payload `-71`
  descriptor completions, followed by a `-2` timeout and another two `-71`
  retries. The run classified as `usb-attach-or-descriptor-failure--71`, with
  artifact SHA-256
  `8a9664a5e44d00c839618f1a1b66af75b3f9ea2f50d75d76ba7e5dd8018eda19` and
  pcap SHA-256
  `48fe1453568662044a164f1921633f7aaf783ec1837a4021a61cc111a3611a37`.
  The flow-map did not yield a Fullerene descriptor or a stable re-enumeration;
  it only confirms that host-visible `-71` timing is sensitive to the failed
  handoff boundary.
- Run `1363832.0` tested the remaining targeted HS-PHY POR recovery A/B:
  `--signal-probe --signal-cmd-gate hsphy-por-clear-after-runstop` reapplied
  the qpr1 POR-clear write immediately after the final Run/Stop. QEMU
  preflight, image audit, RAM-only `fastboot boot`, and automatic Android to
  Fastboot recovery passed. The host still reached HS attach followed by the
  same zero-payload address-0 `-110` completion and three `-71` retries; no
  `1234:0001` appeared. Artifact SHA-256 is
  `fec41eed803f1e63fdfd719f4fa69403c73934797feb87e6deb548db7dd8fd7d`.
  Reapplying POR after Run/Stop does not move the pre-descriptor boundary.
- Run `1378512.0` exercised the current-HEAD normal Android-init pre-DTB
  candidate after reproducing its build with the exact child environment. The
  artifact SHA-256 was
  `287bc17cbde1e2764f7381ff96452c8feaa1ca90e394789276a053b387170263`;
  QEMU preflight and image audit passed and RAM-only `fastboot boot` was
  accepted. After the Fastboot disconnect, the host saw no Fullerene,
  Android, or bootloader transport and the bounded recovery window expired;
  usbmon retained `918100` bytes with no usable Fullerene identity. The run
  classified as `google-logo-or-software-unrecoverable-suspected`, so the
  post-DTB candidate was not issued. This is the current-HEAD retake of the
  older corrected candidates from `1131105.0`; it does not resolve the USB
  boundary and requires physical recovery before another device operation.
- Run `1396532.0` started the corrected candidate plan with the current-HEAD
  SHA gates and `--recovery-wait-secs 900`. The initial host state was
  `device-absent`; the runner polled all host transports for the full 900
  seconds, recorded `device-absent` on every sample, issued no ADB, Fastboot,
  build, or boot operation, and terminated with the same manual-recovery
  requirement. This validates the autonomous recovery wait but provides no
  new USB evidence; the pre-DTB candidate was not issued and post-DTB was not
  considered.
- Run `1729882.0` resumed from a manually recovered Fastboot handset and
  issued both current-HEAD candidates. QEMU preflight, image audit, and
  RAM-only `fastboot boot` acceptance passed for pre-DTB
  (`287bc17c...`) and post-DTB (`e36d73a9...`), but neither produced
  Fullerene USB; both classified as
  `google-logo-or-software-unrecoverable-suspected`, and the final host state
  had no ADB, Fastboot, or USB transport. The run is preserved at
  `tmp/fullerene-bramble-candidates.1729882.0/` and does not resolve `-71`.
- A separate passive Tshark capture for `1729882.0` was started after the
  post-DTB device had already disappeared. Its pcap SHA-256 is
  `5c77b6b7fb704232d4d0bfb0911b53f41927604f5fb857757ef7a67593e5768f` and it
  contains only host/root-hub polling (`-115`/`-2`), not the Pixel's
  descriptor request or `-71`; the earlier captures in `1343716.0` and
  `1358269.0` remain the actual Tshark descriptor-error evidence.
- The USB diagnostic quiet-window contract was tightened: both the ordinary
  poll loop and the Android-init timer fallback now check the quiet deadline
  before reading any DWC3 MMIO status. Formatting, diff checks, and the
  Fullerene/flasks test suites pass after this source-only safety correction.
- All recorded runs through `1143741.0` passed QEMU preflight and image audit,
  used RAM-only `fastboot boot`, and preserved the no-flash/erase/unlock/slot
  mutation/configfs-rebind safety contract. The latest late-MMU candidate
  stopped at the device-absent recovery gate and requires physical recovery.
- The runner contains an optional Android/ADB-to-Fastboot command path, but
  transport recovery observed in this investigation has been performed
  manually. That path only issues `adb reboot bootloader` when ADB is already
  visible; it is not a physical reboot or a recovery from `device-absent`.
  A `device-absent` state has no USB transport, so the runner can only wait
  and record the required manual recovery.
- When the handset is absent, no ADB, Fastboot, build, or boot operation is
  issued. The candidate runner records a bounded recovery wait and can resume
  the same candidate after Android/Fastboot becomes visible.
- The candidate plan's SHA gate now tracks the reproducible current-HEAD
  hashes: pre-DTB
  `287bc17cbde1e2764f7381ff96452c8feaa1ca90e394789276a053b387170263` and
  post-DTB
  `e36d73a9c4413133279390772dcdae5dae935eb6f0b318258f901c788e023fb3`.
  The older exact artifacts from `1131105.0` remain preserved as separate
  evidence and are not silently substituted. The earlier `704579.0` image
  remains preserved as a separate safety record and will not be reused.
- The Rust harness now accepts `--tshark` and starts a wireshark-group
  Tshark usbmon capture before build/boot, writing the pcap, SHA-256, interface
  list, and I/O summary. If Tshark cannot start, it refuses to issue
  `fastboot boot`; the capture is passive and does not alter USB traffic.
- The historical safe replay queue `1782267.0` was started with 155 safe
  manifests deduplicated to 115 distinct current CLI conditions. Every replay
  condition forces both `--tshark` and `--usbmon`; the queue skips no safe
  condition, including the restored EUD device-mode branch. Step 1 passed
  QEMU, audit, Tshark startup, and RAM-only `fastboot boot`, then classified
  `google-logo-or-software-unrecoverable-suspected`. Its integrated Tshark
  pcap has 725,498 frames, 127,461,532 bytes, and SHA-256
  `c39e5fe5f4ee02ed03d5597de795e08fa735c47e133e78b6b30b2ff39989bb05`.
  The queue initially waited for physical Fastboot/ADB recovery before step 2
  and issues no device command while the transport is absent.
- After Fastboot returned, replay step 3 (the historical `fastboot-wait=0`
  Android-init/pre-DTB condition) completed its RAM-only boot and integrated
  Tshark capture. It classified
  `google-logo-or-software-unrecoverable-suspected`; the pcap contains
  480,811 frames and 98,888,464 bytes with SHA-256
  `48a200572de38926335189bcf842d3805cbc03c3f5f581f8de61ca6a0c0ca6b2`.
  Tshark saw no Device Descriptor GET in that attempt. Its decoded USB status
  histogram was `0` = 239,130, `-115` = 240,404, and `-2` = 1,277, with no
  `-71`; the host journal recorded only the Fastboot disconnect
  (`usb 2-1: USB disconnect, device number 52`) and no subsequent
  `1234:0001` or Android/Fastboot re-enumeration. The replay returned to its
  autonomous recovery window for the next condition; no device command is
  issued while the transport is absent.
- After the next Fastboot recovery, replay step 4 exercised the same
  early-USB-before-DTB-scan profile with `no-adb-reboot-to-fastboot=true`.
  RAM-only boot and integrated Tshark completed, but the result was again
  `google-logo-or-software-unrecoverable-suspected`. The pcap contains
  479,741 frames and 98,714,792 bytes with SHA-256
  `67e19859f4c33626d69d076fafa68afac5c91f5c176d3e5c772e1233b64aa47d`.
  Tshark saw no Device Descriptor GET; its status histogram was `0` =
  238,565, `-115` = 239,870, and `-2` = 1,306, with no `-71`. The final
  host state was device-absent with no Fullerene, Android, or Fastboot
  identity, and the replay is now in the next autonomous recovery window.
- After the next autonomous recovery, replay step 5 exercised the following
  `hold=8`/power-refresh profile with the same integrated Tshark path. It was
  again classified `google-logo-or-software-unrecoverable-suspected`. The
  pcap contains 482,441 frames and 99,283,168 status bytes (summary
  85,523,758 bytes) with SHA-256
  `53fe2629f2a98984a8bf751df44397416d543fb1e7b789026f18a607ee76ba51`.
  Tshark saw no Device Descriptor GET; its status histogram was `0` =
  239,945, `-115` = 241,220, and `-2` = 1,276, with no `-71`. The final
  host state was again device-absent with no Fullerene, Android, or Fastboot
  identity, and the replay remains in its next autonomous recovery window.
- The replay harness recovery default is now 150 seconds rather than the
  900-second candidate maximum. Tshark/usbmon guards are dropped before the
  run result is recorded, and the outer replay advances when Fastboot/ADB is
  visible; that recovery observation is passive and may reflect a manual
  Fastboot action. A still-absent handset is polled only through this shorter
  bounded window, with no device-side command issued while absent; if it
  remains absent, replay stops with `manual-recovery-required` instead of
  issuing the next condition without a transport.
- The old 900-second process nevertheless completed replay steps 6 and 7
  before the timeout-policy change. Both were classified
  `google-logo-or-software-unrecoverable-suspected`, ended `device-absent`,
  and had no Device Descriptor GET or `1234:0001`: step 6 captured 501,830
  frames (`103037704` bytes, SHA-256
  `115541eb4e2150887f0f506fbf60b73faa99b07364b0874df16697dde70fe50b`) with
  statuses `0` = 249,601, `-115` = 250,915, `-2` = 1,314; step 7 captured
  482,851 frames (`99332680` bytes, SHA-256
  `e24fae8bcc4832f3967a8bf124c3c903aaf502c2eb726089471dcbfa8eb298f8`) with
  statuses `0` = 240,149, `-115` = 241,424, `-2` = 1,278. The restarted
  replay is now using the 150-second setting and has resumed from step 7;
  the capture window is separate from that passive recovery wait. The
  subsequent retry was stopped before boot when the handset remained absent;
  a Fastboot/ADB observation is required before another condition is issued.
- The corrected harness was then exercised at replay step 10 with the handset
  absent. It recorded exactly one `mode=passive-observation` recovery window,
  waited 150 seconds without issuing a device command, and stopped with
  `manual-recovery-required`; `next-condition-not-issued-without-transport=true`
  is recorded in `next-experiment.txt`. This is the authoritative behavior for
  a complete USB disappearance: the harness does not claim or simulate an
  automatic physical reboot.
- The resumed Tshark replay then completed steps 10–12 with the handset
  available long enough to boot. Steps 10 and 11 reached the USB2 attach and
  recorded four standard 64-byte Device Descriptor GETs, one five-second
  `-110` timeout, and three immediate `-71` completions; no `1234:0001`
  descriptor appeared. Step 12 ended at `device-absent` and stopped before
  step 13. The follow-up `start-after-connect=true` plus `start-ungated=true`
  A/B ended at 62.6 s with zero Device Descriptor GETs and the handset absent
  (`0=82364`, `-115=82794`, `-2=428`). The subsequent
  `start-at-connect-done=true` A/B reproduced the same pre-GET/device-absent
  boundary at 62.8 s (`0=82289`, `-115=82720`, `-2=431`). The stale-DSTS
  bypass and Connect Done arm are therefore not fixes; both captures stopped
  at the enumeration window and did not hold Tshark open for recovery.
- Run `1995244.0` resumed with the new `--post-dtb-only` harness from manually
  recovered Fastboot and ran the post-DTB artifact with integrated Tshark and
  usbmon. The SHA gate, QEMU preflight, image audit, and RAM-only `fastboot
  boot` all passed. The bootloader USB endpoint (bus 2, address 68) vanished
  at about 27.18 s; the 88.9-second pcap then contained only root-hub traffic:
  no standard Device Descriptor GET, with status counts `-115=91504`,
  `-2=471`, and `0=91034`. The final state stayed `device-absent` through the
  150-second passive recovery window. This confirms the deferred EP0 timing
  profile fails before USB enumeration, so the candidate plan now restores the
  known attach-reaching pre-connect baseline for the next PHY/RX/SOF A/B. The
  rebuilt baseline SHA-256 gates are pre-DTB
  `9e3da64c1d61ca5cf7d4f0ebf3575169acaa0299cac4140867064f06ab0d8923` and
  post-DTB `6ff736b1444c921a2488678f6b83bd4ba2945b2997e025cd4d2d14c008e35790`.
- Run `2012128.0` exercised that rebuilt post-DTB pre-connect baseline with
  integrated Tshark/usbmon. QEMU preflight, image audit, SHA gate, and
  RAM-only boot passed; the bootloader endpoint disappeared at about 18.91 s.
  The 62.4-second capture had no standard Device Descriptor GET and recorded
  `-115=83131`, `-2=429`, `0=82703`; the final state remained device-absent
  after the 150-second passive recovery. The next isolated implementation A/B
  is the existing Run/Stop re-attach (`--arm-blip`) path.
- The first arm-blip hardware attempt was stopped by the SHA gate before
  `fastboot boot`: the initial offline hash was generated without the
  harness's effective `UFS_EXECUTE=0` child environment, so the observed
  post-DTB image differed and no device operation was issued. Rebuilding with
  the exact child environment passed QEMU and boot audit; the corrected
  arm-blip gates are pre-DTB
  (`0af599cfade24888b9767609b926a31a8ec9d22f47a35f39e3a3fbbcaa08186d`) and
  post-DTB
  (`7d9b29e8f35530ac2a3babac455a2637e15fae27c1d49d977ff63dc04e93ab72`).
  These gates include the Android-init late-U0 queue fix: successful
  handoff now queues the differential and the timer IRQ consumes it once the
  link is actually U0.
  The harness selects these hashes only with `--arm-blip`; baseline hashes
  remain separate.
- The candidate harness now accepts `--post-dtb-only`, so after a preserved
  pre-DTB failure the next hardware run can resume directly at the post-DTB
  condition with the same SHA gate and passive Tshark/usbmon capture; it does
  not repeat the already measured pre-DTB image.
- Run `2053713.0` retried the corrected post-DTB `--arm-blip` image
  (`7d9b29e8...`) from manually recovered Fastboot. The exact image passed
  QEMU, audit, SHA, and RAM-only boot; the 80.1-second Tshark pcap contained
  no Device Descriptor GET and only Fastboot bulk traffic for address 71
  (`-115=82607`, `-2=429`, `0=82179`) until the bootloader endpoint vanished
  at about 18.87 s. The final state remained `device-absent` after the
  separate 150-second passive recovery wait. This does not show the queued
  Android-init Run/Stop blip executing. The follow-up source change keeps the
  U0-guarded blip pending but also tries it synchronously at handoff return,
  covering the interval before the Android-init timer IRQ is enabled; the
  rebuilt pre/post-DTB gates for the immediate-at-handoff retry are
  `86aafe408c...` and `98c8b530...`, respectively.
- The next independent A/B is now source-backed by the Bramble qpr1 Android
  DWC3 gadget path: immediately before the final Run/Stop transition it
  republishes the event buffer and repeats gadget restart/EP0 start setup,
  instead of only toggling Run/Stop. Fullerene exposes this as
  `--gadget-restart-at-runstop`, mutually exclusive with `--arm-blip`.
  Exact offline artifacts passed QEMU and boot audit with gates
  `739d617f8e49b5df3f43d9ebe2d6ce8e457a729e57bbe6d9e03dd2e925cf36b9`
  (pre-DTB) and
  `c2faf6c0a200f633cd41efae0c8a1e14a90adb6d6a2988519a13d277290b8af3`
  (post-DTB). No hardware result exists yet because the handset remains
  absent; physical Fastboot recovery is still manual and required before the
  SHA-gated RAM-only boot.
- Run `2082288.0` executed the post-DTB gadget-restart candidate from manually
  recovered Fastboot with integrated Tshark/usbmon. QEMU, boot audit, SHA gate,
  and RAM-only boot acceptance passed. The capture stopped at 62.5 s with no
  Fullerene GET_DESCRIPTOR; it contained only Fastboot bulk traffic through
  about 21.8 s before the bootloader endpoint vanished, with status counts
  `-115=85784`, `0=85343`, `-2=442`. The pcap SHA-256 is
  `ffff53bd3cbee1ba1311363aa5b11bca53dbf152d82963e145a781edd8ae4b81`.
  The separate 150-second passive recovery still ended `device-absent`.
  Source comparison then showed that the preceding candidate wrote DCFG.SPEED
  before, rather than after, the restarted EP0 construction. The next
  source-order A/B adds `--gadget-start-only-at-runstop`; its exact offline
  pre/post gates are `460cc4c2a30b83ed5d7cf831fcbd7ee4a4a5c2d28fd86841f7db98ef23db5d9d`
  and `c7695d54d13c672ad6c8da23c99c0bef9b086611ed98589524b0e12c0911a14e`.
- Run `2098194.0` executed that source-order post-DTB A/B from the next
  observed Fastboot state. QEMU, audit, SHA, and RAM-only boot passed; the
  62.5-second capture again had zero Device Descriptor GETs and only Fastboot
  bulk traffic until about 21.9 s. Status counts were `-115=87086`,
  `0=86641`, `-2=443`; pcap SHA-256 was
  `20d17dd03d46ba81a440037b228a8014eb32833ccacb4bc559350e3cf5a06872`.
  The 150-second passive recovery ended `device-absent`. Moving DCFG.SPEED
  after the EP0 restart therefore did not move the pre-descriptor boundary.
  The next run should use the already prepared pre-DTB half of this same
  source-order pair, then return to PHY/RX/SOF and DWC3 event-ingress evidence
  if it reproduces the boundary.
- The qpr1 maximum-speed A/B is complete:
  `--qpr1-gadget-speed-profile` requires the two gadget-restart flags and
  enables only the qpr1 DT/gadget-start speed state (`DCFG.SPEED=SuperSpeed`
  plus initial EP0 MPS 512), without selecting the separate SuperSpeed PHY
  bring-up path. Its exact offline gates are pre-DTB
  `6c0ae1e3dd1e8b54d3c092f38607ad741418397d3d04631e3e9283a760d80d1f` and
  post-DTB
  `6b8d31a5f1b5266b172ffcd3bfba3ae70b17072fd8531431f33d648e269073f1`.
  Both passed QEMU and boot audit. Run `2114713.0` executed the PRE-DTB half
  from observed Fastboot; its 62.6-second Tshark capture had zero Fullerene
  GET_DESCRIPTOR requests and only the bootloader traffic until address 74
  disappeared at about 20.8 s (`-115=85782`, `0=85341`, `-2=442`). The pcap
  SHA-256 is
  `429275da2cbf634ca1f5247166d837244841b74b537e718e077dc7053fee66d0`.
  The 150-second recovery observation ended `device-absent`, so POST-DTB was
  not issued in that process. After the next observed Fastboot state, Run
  `2124221.0` executed POST-DTB; it reproduced the same pre-descriptor result
  (`-115=85689`, `0=85245`, `-2=446`) with pcap SHA-256
  `fcb0eb22e360672f0b52d0ffcf85b5aeaf0cb22d92638eebe274a03b0e89b95f` and
  again ended `device-absent` after passive recovery. The qpr1 speed state
  therefore did not move the boundary.
- Run `1782267.0` step `13` then exercised the next queued Android-init
  early-USB/start-ungated profile from observed Fastboot, with the integrated
  Tshark/usbmon capture. QEMU, boot audit, SHA, and RAM-only boot acceptance
  passed, but the capture stopped at 62.289 s with zero Fullerene
  GET_DESCRIPTOR requests. The bootloader endpoint carried only Fastboot
  traffic through about 18.55 s; all captured URB statuses were
  `-115=82379`, `0=81950`, and `-2=430`. The Tshark pcap SHA-256 is
  `c62d722e889570dbd881701651fd9007ea3cf0b5e9c38b6cc6254f61244331bc`.
  The separate 150-second passive recovery ended `device-absent`, so this
  early-handoff/start-ungated profile also does not move the boundary.
- Run `1782267.0` step `14` then exercised the paired Android-init
  `start-at-connect-done` profile from the next observed Fastboot state. The
  QEMU preflight, boot audit, SHA gate, and RAM-only boot acceptance passed.
  The integrated capture stopped at 62.344 s with zero Fullerene
  GET_DESCRIPTOR requests; only Fastboot traffic was present through about
  21.56 s. Its URB status histogram was `-115=85537`, `0=85101`, and
  `-2=439`; the Tshark pcap SHA-256 is
  `921582fbff0dd3c9579acb416067a4b4ba1ab8fbd84cc1445f421112f846b270`.
  The separate 150-second passive recovery again ended `device-absent`, so
  the Connect Done start timing does not move the boundary either.
- Run `1782267.0` step `15` then exercised the post-DTB half of the same
  Android-init `start-at-connect-done` profile after Fastboot reappeared.
  QEMU, boot audit, SHA, and RAM-only boot acceptance passed. The integrated
  capture stopped at 62.274 s with zero Fullerene GET_DESCRIPTOR requests;
  Fastboot traffic ended around 21.56 s. The URB histogram was
  `-115=85530`, `0=85087`, and `-2=444`; the Tshark pcap SHA-256 is
  `2c0fc24c10bc119ce741a1635066823e4c1bb682fea2742c1d42cb01651311cb`.
  The separate 150-second passive recovery ended `device-absent`, completing
  this start-timing pair without moving the boundary.
- Run `1782267.0` step `16` then exercised the post-DTB pre-connect baseline
  from Fastboot after the transport reappeared. QEMU, boot audit, SHA, and
  RAM-only boot acceptance passed. The integrated capture stopped at 62.325 s
  with zero Fullerene GET_DESCRIPTOR requests; the Fastboot endpoint was the
  only Pixel address observed. URB statuses were `-115=85551`, `0=85106`,
  and `-2=442`; the Tshark pcap SHA-256 is
  `2f29f13064659f19875e2f2b0491c27f09b7d7e8bcddb5ff5d78855b0d4969de`.
  The separate 150-second passive recovery ended `device-absent`. This
  post-DTB baseline also remains pre-descriptor.
- Run `1782267.0` step `17` then exercised the post-DTB `arm-blip=true`
  condition after the integrated watcher observed Fastboot. QEMU, boot audit,
  SHA, and RAM-only boot acceptance passed, but the 62.594 s integrated
  capture had zero Fullerene GET_DESCRIPTOR requests. URB statuses were
  `-115=87983`, `0=87531`, and `-2=453`; the Tshark pcap SHA-256 is
  `a545782c154eb79c9c98d3c0b54fd36cd330de7e78b717d71e3a5b898bd9eb05`.
  The separate 150-second passive recovery ended `device-absent`; the arm
  blip did not move the boundary.
- Run `1782267.0` step `18` then exercised the independent post-DTB
  `gadget-restart-at-runstop=true` condition. QEMU, boot audit, SHA, and
  RAM-only boot acceptance passed, but the 62.189 s integrated capture had
  zero Fullerene GET_DESCRIPTOR requests. URB statuses were
  `-115=86485`, `0=86037`, and `-2=449`; the Tshark pcap SHA-256 is
  `76963dd1d17c71b90978608098c9df65df0dcc11e690a13a28aca196d5fa2c21`.
  The separate 150-second passive recovery ended `device-absent`; the
  qpr1-style gadget restart did not move the boundary.
- Run `1782267.0` step `19` then replayed the post-DTB qpr1 source-order
  speed-write condition from the integrated watcher's next observed Fastboot
  state. QEMU, boot audit, SHA, and RAM-only boot acceptance passed, but the
  62.563 s Tshark capture had zero Fullerene GET_DESCRIPTOR requests. URB
  statuses were `-115=85819`, `0=85375`, and `-2=445`; the pcap SHA-256 is
  `c9525a680541cd4050d15b75240e20da15b37ebde556b86e25698c868ddd72cf`.
  The separate 150-second passive recovery ended `device-absent`; this
  source-order speed write did not move the boundary.
- Run `1782267.0` step `20` then replayed the pre-DTB qpr1 maximum-speed/EP0
  MPS condition after the watcher observed Fastboot. QEMU, boot audit, SHA,
  and RAM-only boot acceptance passed, but the 62.572 s Tshark capture had
  zero Fullerene GET_DESCRIPTOR requests. URB statuses were `-115=85541`,
  `0=85097`, and `-2=445`; the pcap SHA-256 is
  `851ddfa335341708651f0379e8994ae805ba83f27e9181a5e6016d16b1868bb9`.
  The separate 150-second passive recovery ended `device-absent`; the
  pre-DTB speed/MPS state did not move the boundary.
- Run `1782267.0` step `21` then replayed the post-DTB half of the qpr1
  maximum-speed/EP0-MPS condition. QEMU, boot audit, SHA, and RAM-only boot
  acceptance passed, but the 63.001 s Tshark capture had zero Fullerene
  GET_DESCRIPTOR requests and only `1,425,244` captured bytes. URB statuses
  were `-115=3461`, `0=3027`, and `-2=432`; the pcap SHA-256 is
  `50300cbb692397b8fde72acff63685ac00aef0479d643ee41c0d615a43e79542`.
  The separate 150-second passive recovery ended `device-absent`; the
  post-DTB speed/MPS state did not move the boundary.
- Runs `1782267.0` steps `22` and `23` then replayed the two source-backed
  HS-PHY ownership branches (`hsphy-ignore-eud` and `hsphy-eud-device-mode`).
  Both 62.297/62.355 s Tshark captures contained four address-0 64-byte
  Device Descriptor submissions: one timed out and three completed
  immediately with `-71`, with no response payload. Their URB histograms were
  respectively `-115=2249, 0=1875, -2=370, -71=3` and
  `-115=2396, 0=1981, -2=414, -71=3`; pcap SHA-256 values were
  `1a831cdb5d07e5de9c8edf0578737175934dacc282e936c5bc8c6fb92cc52e61` and
  `e80484c1ef7371305429814aceb258adf8666a415686434b40e8a3caf9f33a1d`.
  The EUD ownership A/B therefore remains non-discriminating at the same
  pre-payload boundary.
- Runs `1782267.0` steps `24`–`26` then replayed the direct-handoff baseline,
  `android-block-reset`, and `usb2-dis-sleep-mode` variants. All three
  62.244–62.343 s captures reproduced four address-0 descriptor submissions,
  one timeout, and three immediate `-71` completions with zero payload; their
  URB histograms were `(-115=2208, 0=1839, -2=366, -71=3)`,
  `(-115=2208, 0=1839, -2=366, -71=3)`, and
  `(-115=2198, 0=1831, -2=364, -71=3)`. The pcap SHA-256 values are
  `e28f227775445a26e2fcdf6327d74825bd3754a183b5494d7ce63082df8c5a3f`,
  `274fe0524ffe7803e3a36a5185ddb3d23a18420861b2d8dc4485a8a9c069468a`,
  and `6f9f22ecba745a91d78cb4fa2d93ea04d4a1fee157ddf86c99ff607e181cda6c`.
  Neither Android block reset nor the USB2 sleep-mode delta moves the
  boundary.
- Run `1782267.0` step `27` then exercised the always-gated signal probe.
  The 62.376 s capture had five immediate `-71` completions and no Fullerene
  payload (`-115=2200, 0=1832, -2=364, -71=5`), with pcap SHA-256
  `1ecbfcf71cefb1bef6503a3fc530d63d8edc184ad8a63b980e4eb7aa93f3c4c6`.
  The probe changed the host error count but not the enumeration boundary.
- Runs `1782267.0` steps `28` and `29` then replayed the post-Run/Stop POR
  clear and the XBL-exact HS-PHY/controller-IRQ profile. Both 62.193/62.395 s
  captures reproduced four address-0 descriptor submissions, one timeout,
  and three immediate `-71` completions with no payload. URB histograms were
  `-115=2195, 0=1827, -2=365, -71=3` and
  `-115=2421, 0=1999, -2=420, -71=3`; pcap SHA-256 values were
  `5f4fd1058d9862082e1223b7e3349308d1d1b0a748aea08539c75e0194978c65` and
  `d71100c21b32dd482189ba560e02e8bb7518bed107b79e1ceee73125b6dd4767`.
  These source-backed PHY/reset/IRQ variants also do not move the boundary.
- Run `1782267.0` step `30` then replayed the Android-init early-USB/current
  handoff profile. QEMU, boot audit, SHA, and RAM-only boot acceptance passed,
  but the 62.294 s integrated capture had no Fullerene GET_DESCRIPTOR request
  and ended with every host transport absent. URB statuses were
  `-115=2482`, `0=2061`, and `-2=422`; the pcap SHA-256 is
  `98c29ce11ccab801a3f7792cb48d46908b750ba29b841018468f542e5de805a9`.
  The separate 150-second passive recovery also ended `device-absent`; this
  early-USB/current-handoff profile did not move the boundary.
- The same replay queue then hardware-tested steps `31`–`34` with integrated
  Tshark/usbmon. All four passed QEMU, image audit, SHA gating, and RAM-only
  `fastboot boot`, then reproduced the attach-reaching address-0 descriptor
  timeout: `capture_stopped_after_boot_ms` was `62360`, `62265`, `62368`, and
  `62277`, respectively; each classification was
  `usb-attach-or-descriptor-failure--110`; no `1234:0001` descriptor appeared.
  Tshark pcap SHA-256 values were, in order,
  `3a7358795f1176ee7cbcf2d74d745a3a31642db23f2df24ea7844f6bb01ab0d4`,
  `0a581ba8a40bbe749b1b05fbd9766793b131d98f7f2a12a6b3c8bde0e12affd2`,
  `85146f50febeda442e5a92282668e121f0fc6763971b1b448383b8a56eba87a3`, and
  `4fbb0866562f3e34541e375831112d79a38ee1d9d7175bb42b8af9a47ce8c36c`.
  The source-backed gadget-restart, Run/Stop reset/order, and post-event DMA
  command-gate variants therefore do not move the boundary. Android/Fastboot
  recovery completed automatically during the queue; step `35` is the
  clock-branch re-arm plus the same attach-reaching diagnostic profile.
- The replay then completed steps `35`–`38` with the clock-branch re-arm,
  U0-arm, gadget-start-defaults, and `signal-early-drop=3` A/Bs. Their
  61.329–63.023 s captures all classified as
  `usb-attach-or-descriptor-failure--110`; no `1234:0001` appeared. The pcap
  SHA-256 values were, in order,
  `27a49cf370b2e00c5c7780cfb500f407308a425c7299c57175121af13fa1356c`,
  `96eb862fb2973afda51887b2373ed3a797388ede0b093989a624e2c1c8c27f79`,
  `31baefa29a38d25ee624a913887864ca4dbcdf6dc42bc82f6b750f7b19e3aca2`, and
  `48911b2cf7da87120ae435f33eb46cad119919e7097df123181d2823bdc3f0e7`.
  These A/Bs do not move the HS descriptor boundary.
- Steps `39`–`43` tested the separate SuperSpeed early-failure family,
  including PHY-state preservation, Connect Done HIRD clearing, direct
  handoff, gadget-start defaults, and minimum Run/Stop delay. All five
  stopped after 31.233–32.264 s and classified as
  `google-logo-or-software-unrecoverable-suspected`; none produced Fullerene
  USB or an HS descriptor request. Their pcap SHA-256 values were, in order,
  `e1369c254eb614d99859cde0aae2d1ee67072ff627bef430e6e3ae930f883536`,
  `d312db0a2d4c8bc0b5c2317356b188d451a02b27dbd8ad30146851380c82e639`,
  `ef3c7c6f7371d85d9fbfccac864e2ae3c37f0d3e86f355da13e454bb2cfedba4`,
  `d77bd394feeac23c6fdb9fa84ba2e5bc565f683e2346e194af07f5f37b223a29`, and
  `27c4918e516fd062807c8d48b04fe4a2f2c03046b36d460305fa03ff5c345f2a`.
  This is a distinct SuperSpeed/boot-stability failure and is not evidence
  for changing the measured HS EP0 path. Android/Fastboot recovery continued
  automatically throughout these runs; step `44` was the next queued
  SuperSpeed minimum-delay variant.
- Steps `44`–`46` completed the remaining nearby SuperSpeed A/Bs for
  minimum Run/Stop delay, the no-core-reset profile, and SuperSpeed setup
  retry. They all retained the same
  `google-logo-or-software-unrecoverable-suspected` classification, with
  32.130–32.220 s capture windows and no Fullerene or HS descriptor request.
  The pcap SHA-256 values were
  `3c4d1780a768b9dce27e82f31989d8b3c2addcc01ecfcd05395ad55c9f7bdb8e`,
  `ec85e5ed08d1f6dfe2457f61488faf9ec28c1bd85fd304e165921ad84c29df95`, and
  `caf205e6e020ad750a536c6ca12d59058f942d72a4feb341a9eb7488498b4c38`.
  Steps `47`–`55` then covered SuperSpeed clock/QMP/DBM/SUSPHY and resource
  order variants. All nine retained the same early-failure classification;
  their 31.111–32.202 s pcap SHA-256 values were, in order,
  `e223f61f1c5e164b3bb04d92462f77cc1212eca2eed1742110946750aef88a68`,
  `88ad4f3e5fd5a8ccb9e853a598675ca9d74563fc9f66186991ea4f6349810b27`,
  `7975387532ceadc9c7efe4378cf0cd53a8ed5741071728ff775178c2ef430019`,
  `8c3dfe19703950770c3a32b7cbb50c01ccf951690f85519fae7bbd7730822b62`,
  `3307b570913932d66a57bb0e02095918bb0fe57bc84024ad3e9dfdf03dd8c480`,
  `ad057b5382729f6b62f74e70e5f00db69af025d70c76147dde33f40c80f3d2f6`,
  `170e94350247a8041e8b0277ed1083a97e89742eef0e9e31c036bb2684477933`,
  `6ccc2da407a463419a51f930ce7ec8d9810cedb3387f2a5ecb35afd35ae871af`, and
  `07aad9670870898acffa5c80483739146e901d14394763f58a80f202ad0f891d`.
  Steps `56`–`57` added HS-PHY rail refresh and signal probes but also failed
  early (`f3790833987e43095964ec1062693ad3ed28d4f1e077301beacba398c34d0ee9`,
  `670b884446f9b319bf5ab6de36510b14b87134e6058cba423ab711eb4c66ce89`).
  The empty-condition control at step `58` reached automatic Android fallback
  after 61.265 s without Fullerene (`6fa9761bd9b0d854a61fccf97f2735dd9c8abf10c2ac33c31664df1ab991181e`);
  steps `59`–`60` again failed early while extending observation and moving
  DEVTEN before Run/Stop. The recovery loop remains live.
- Step `61` removed the ADB reboot/return wrapper while retaining the
  source-exact HS-PHY, signal-probe, and 60-second observation profile. It
  reproduced the decisive HS attach / address-0 descriptor boundary at
  `62.161 s` (`usb-attach-or-descriptor-failure--110`); host kernel logs show
  `new high-speed` followed by `device descriptor read/64, error -110`, and
  Tshark's pcap SHA-256 is
  `104f659dc3cab9537ff9181feacc90425d0054085a26ddd203894686d9085a0c`.
  The capture's GET DESCRIPTOR completion had zero payload, so the signal
  probe did not promote this into a usable EP0 response or `1234:0001`.
  Step `62` repeated the profile through Android-init/ADB return; it failed
  early as `google-logo-or-software-unrecoverable-suspected` (pcap SHA-256
  `ba592826995ddab72393129206d6682dda5386acee86d8bde67bfe5dbf1283bf`).
  The watcher then exhausted its 150-second recovery window and paused at
  step `62` with the handset absent; no device mutation was issued.
- The resumed replay then completed steps `63`–`75` with integrated
  Tshark/usbmon captures. Steps `63`, `64`, and `66`–`73` all preserved the
  attach-reaching boundary: the host saw four address-0, 64-byte Device
  Descriptor GETs, one `-110` timeout, and three immediate `-71` retries with
  zero response payload. The pcap SHA-256 values, in step order, were
  `708d60d132eb1ac83be51ec735a5a98f8f3dcd6a4b537b67476381db5dfbb58b`,
  `598f7c22c0d81871dc25b6a036c946fd7a5987140db0137ec1ea91176379a752`,
  `22cfde6b24b2dd3e1fcb450acbb865a086c55a389f408d342fd002bf17330b2f`,
  `437197537c9fbac0ea29695caf5c126651b15ca96bccb47ecd31de6c3b334b69`,
  `438ab492380702a3814a3358166da8c6521202a0a4b59b6afe40c9218d8f9866`,
  `6d52eac1bc51297ae7743a9d7682b08273cae1924a4acc71b25e40ba2c0fe365`,
  `9cba587190d61faeb2536a0485a5132f3b3a4ef0ac3a39ea7b232dafb3dc62d1`,
  `d83a5636bab739532c83afef74f139008aea1a2adca25dcc272a3adb7a9433b6`,
  `82f0d318833d739f69358bbe0e9c98a0079db896845f39711ef0804d5a4a466`,
  and `62721f85a03c881c4cf46718fcfc32d7b476be7f9d412176ff44a6b258e2e0c8`.
  These covered the event/DMA gate, link and UTMI readouts, both qpr1
  SUSPEND_N restore forms, POR-clear, raw-link, post-Run/Stop UTMI, UTMI-link
  gate, and DMA-cache-maintenance variants; none produced `1234:0001`.
  Step `65` was an Android-ADB transport control and issued no Fullerene boot.
  Step `74` classified as `android-fallback` (pcap SHA-256
  `573d883d81880190921768e2c8abd6abd7e480350765053556be2df3f00498ce`),
  while step `75` classified as
  `google-logo-or-software-unrecoverable-suspected` (pcap SHA-256
  `d13875a5853769c018ed38b8e1126afc0568b6bb7a86bda166432a1f0c1a7c20`).
  The queue stopped after its 150-second passive recovery at step `75` with
  no ADB/Fastboot/USB transport and issued no operation while absent. The
  remaining work is still source-directed PHY/RX/SOF versus DWC3 event
  ingress evidence; no source fix is justified by these nondiscriminating
  captures yet.
- A host-kernel correlation collected on 2026-09-15 shows the same handset
  serial (`26191JECB00076`) repeatedly enumerating on the SuperSpeed path
  (`usb 2-1`, `18d1:4ee7` and `18d1:4ee0`) between `08:50:48` and `09:01:35`
  JST, while the paired high-speed path (`usb 1-9`) repeatedly reaches
  `new high-speed USB device` and then fails
  `device descriptor read/64, error -110` (08:50:22 through 08:59:28 JST).
  This strengthens the USB2-HS-only boundary: the host and the Pixel's USB3
  path are alive, but it does not distinguish an analog HS-PHY/RX failure
  from a DWC3 event/SOF-ingress failure. The observation used only the host
  journal; no device command or Configfs operation was issued.

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
  and the previously tested gadget-start variants, including the qpr1-style
  final-Run/Stop start; the first source-backed gadget-restart-at-Run/Stop
  A/B also ended pre-descriptor, while the source-order speed-write A/B is
  prepared but not hardware-tested.
  the stale-`DSTS.DEVCTRLHLT` bypass and Connect Done EP0-arm A/B both ended
  before a descriptor request, with no evidence of a fix
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

The source audit also ruled out promoting `snps,usb3-u1u2-disable` directly into
the HS fix: Android's DWC3 implementation uses that DT property to reject or
gate the SuperSpeed U1/U2 feature requests, while the measured failure is the
USB2 HS address-0 `GET_DESCRIPTOR` `-110`/`-71` boundary. Keep it out of the
unmeasured HS path until a SuperSpeed-specific trace justifies it.

1. The integrated Tshark replay queue is complete: the automatic continuation
   consumed all 51 remaining conditions (the step-75 retry plus steps 76--125).
   The persistent
   `bramble-usb replay --watch --run-dir tmp/fullerene-bramble-replay.1782267.0
   --recovery-wait-secs 150` watcher remains alive for recovery observation, but
   `--dry-run` reports no pending condition. Every continuation condition has
   an integrated Tshark capture and advances only while a transport is present;
   device absence is handled by a short passive recovery observation, not a
   device command.
2. The continuation produced no `1234:0001`. Of the 51 conditions, 39
   reproduced the HS attach plus zero-payload address-0 descriptor boundary,
   9 stopped at Google-logo/software-unrecoverable before that boundary, two
   were Android fallback, and one ended at HS attach without a registered
   descriptor because its shortened enumeration window closed before a GET.
3. No stable source-fix discriminator was found. The next implementation
   work must obtain independent USB2 HS-PHY RX/SOF versus DWC3 event-ingress
   evidence (permitted serial/register trace or equivalent), then implement the
   smallest source-backed fix at the identified boundary and rerun the exact
   Tshark condition. Do not guess another EP0/TRB mutation.
4. The handset may disappear during each RAM-only boot, but the watcher has
   already observed Fastboot/ADB return and automatically continued through
   step 125. It does not issue a device operation while the handset is absent.
5. Do not add guessed EP0/TRB, packet-format, or register mutations while the
   host still receives no Fullerene descriptor payload.
6. A new source-backed ABL event-consume A/B is staged in
   `tmp/fullerene-bramble-loop.2668821.0` with integrated Tshark and the
   automatic recovery path. The loop now waits for an initially absent Pixel
   transport to reappear as ADB/Fastboot within the bounded Fastboot wait; as
   of this update the host still reports no ADB, Fastboot, Android, or
   Fullerene USB, so no device-side operation has been issued for this run.
7. Run `2702252.0` executed the remaining QPR1 `dwc3_phy_setup()` PIPE-side
   pre-reset delta (`--usb2-source-phy-setup`) with integrated Tshark,
   automatic ADB-to-Fastboot transition, and RAM-only `fastboot boot`. QEMU,
   audit, and boot acceptance passed, but the candidate did not reach the
   Fullerene HS attach boundary before the device disappeared; the capture
   lasted 73.2 s, retained 4,948 frames / 2,744,438 bytes, and has pcap SHA
   `367f2412d770fa88c3d2084ae2ed91b10f3c89f581eb89eaad4a65e4999a4b58`.
   The final state is `device-absent`; this source-backed PIPE delta is not a
   fix and the handset currently needs physical recovery before another
   device operation.
8. The opt-in `--usb2-runtime-power-keepalive` is now source-backed at both
   domain and clock-branch level. Every 500 ms after USB2 Run/Stop it re-sends
   the Android active USB2 RPMh CX/interconnect/HS-PHY votes, forces the USB30
   GDSC on, re-arms the USB2 controller branches in iface/core/sleep/utmi
   order, and re-sends the HS-PHY reference-clock vote. It runs from both the
   normal poll loop and the Android-init timer-IRQ fallback. It does not reset
   DWC3, retune controller clock sources, alter EP0/TRBs, or touch configfs.
   Flasks type-check/build, QEMU USB self-test, Bramble audit, and RAM-only
   image generation pass; the updated hardware artifact is pending the next
   transport return.
9. Run `2721897.0` started that keepalive condition with Tshark armed, but the
   host remained `device-absent` for the full 300-second initial transport
   wait. The harness issued no ADB/Fastboot command, created no capture, and
   wrote `classification=device-absent`; this is a transport-recovery result,
   not a USB-fix result. The next experiment remains physical Pixel recovery.
10. Run `2735620.0` did observe Fastboot return and automatically executed the
    keepalive image, but the candidate did not reach Fullerene USB attach; the
    host lost the Fastboot transport immediately after boot acceptance and the
    final classification was `google-logo-or-software-unrecoverable-suspected`.
    Tshark retained 6,340 frames / 2,845,182 bytes for 75.5 s; no
    `1234:0001` device appeared. This is not a keepalive rejection because the
    selected profile did not reach the prior attach boundary.
11. The harness now has `--wait-for-transport` for a persistent initial
    transport observer. It records `initial-transport-wait.tsv`, sends no
    device command while the handset is absent, and continues automatically
    into the existing ADB-to-Fastboot/RAM-only/Tshark path when transport
    returns. Run `2778673.0` exercised the prior attach-reaching
    `start-after-connect` profile with only the USB2 runtime power/clock-branch
    keepalive added. Fastboot boot was accepted, but the host saw only a new
    high-speed attach followed by `device descriptor read/64, error -110`;
    no `1234:0001` appeared. Tshark retained 3,615 frames / 2,344,623 bytes
    for 65.8 s (pcap SHA
    `2970f1df270b2e97e4821b6fc4d569a2b411a4e2de9c891fc4acb6f3ddc6083`).
    Run `2800411.0` repeated the same profile without explicit USB2 SUSPHY;
    it reached the same `-110` boundary and Tshark also saw three address-0
    descriptor completions with `-71`. It retained 3,593 frames / 2,343,023
    bytes for 65.1 s (pcap SHA
    `4ebf92f05a57416bbc9d5409d8a614862cd45a84c9f690845603040707d21c83`).
    The SUSPHY A/B therefore did not move the boundary. Run `2810674.0`
    then added both source-backed Android peripheral-start lifecycle
    operations (`DBM reset/enable` and `DWC3 sleep-mode disable`) while
    retaining the runtime power/clock keepalive. It again produced the same
    HS attach, four address-0 descriptor submissions, and zero response
    payload (`-110` in the host kernel, with three captured `-71` completions);
    no `1234:0001` appeared. Tshark retained 3,593 frames / 2,343,023 bytes
    for 65.4 s (pcap SHA
    `6659240fccc332e9331c606a32bf029156038fe6763a6d8732fe130f699598d2`).
    DBM/sleep and SUSPHY are now closed as non-discriminating at this
    boundary. The next implementation work is the source-directed
    HS-PHY-RX versus DWC3-event-ingress discriminator, not another EP0/TRB or
    lifecycle register permutation.
12. Run `2826414.0` made that event-ingress comparison pure: the recent
    `main`-loop change prevents the controller-IRQ profile from polling the
    same DWC3 event ring concurrently, so the controller SPI alone consumed
    events. The result was unchanged: HS attach, four address-0 Device
    Descriptor submissions, host `-110`, three immediate `-71` completions,
    and no response payload or `1234:0001`. Tshark retained 3,307 frames /
    2,324,431 bytes for 56.4 s (pcap SHA
    `f967509053bf347c1cf2a3f28ba0d181fd401c837a33e860f94ede660a0127ca`).
    This closes polling-versus-controller event-ring ownership as the
    discriminator; the next edit target remains the source-backed USB2
    HS-PHY RX/SOF or DWC3 event-generation boundary.
13. Runs `2844716.0` and `2850964.0` moved event-ring ownership to the
    Android-init timer IRQ, leaving the DWC3 controller SPI disabled. Both
    retained the known attach-reaching profile and integrated Tshark; both
    reproduced HS attach, four address-0 Device Descriptor submissions, host
    `-110`, and three zero-payload `-71` completions. Their pcaps retained
    3,553 / 3,619 frames and SHA-256 values
    `b4ad1a667053ca337052c6e236f5f30336d512ef435fdfaeb48162471e4f988a` /
    `cf756effa4df4fae89d01e585e8fd884f0a3f09530aba656691062ed96208848`.
    The timer-only event consumer is therefore non-discriminating; the
    `--hsphy-clear-sleepm` A/B in the second run is also closed.
14. Run `2857487.0` added the existing source-backed `--arm-blip` marker to
    that timer-only profile. It retained 3,593 frames for 65.2 s, with pcap
    SHA `59d96d0fecd394d9bdb120ae8f0ed4e67c8b9d09246c3418894e7dc26ad20c44`.
    The host still saw only the HS attach and the same `-110`/`-71` boundary;
    no additional disconnect/reconnect pair appeared, so the successful-EP0
    arm marker did not fire. This narrows the next source audit to the
    pre-EP0 arm boundary (DSTS gate versus STARTTRANSFER completion) and
    avoids another downstream descriptor/TRB permutation. All three runs
    used only RAM-only `fastboot boot`, integrated Tshark, and the bounded
    five-second passive recovery grace after USB disappearance.
15. The follow-up arm-stage diagnostic was corrected in three steps. Run
    `2870605.0` confirmed that the original marker was too early for the host
    attach window (3,353 frames / 58.6 s, pcap SHA
    `9329ce83e22aef8181133545de0b54f750c151f4ec2bf3ee7cd795c19d9cf08e`).
    Runs `2875035.0` and `2878869.0` then queued the marker on the cold
    fallback path as well, but still reproduced the same HS attach and
    address-0 `-110` boundary (3,233 / 3,210 frames; pcap SHAs
    `ced93d4d3f7dc066ecbab26e7186ab693c1e271bbb39d044233fa92cb96e6e47`
    / `251cb8b01e5fae9d2bccf38c03e28a32e79c800f87ae68c12bdd558a8883df52`).
16. Run `2882783.0` added a 30-second deadline so the stage-coded marker
    could not be suppressed by a persistent non-U0 readback. The host still
    exposed only one HS attach and the same zero-payload descriptor timeout;
    no independent Run/Stop pair was observable. Tshark retained 3,199 frames
    for 56.2 s, pcap SHA
    `c72f364d531399a66facf17af927c75781f6225c07831320e22dbf8779cdc213`.
    This closes the current arm-blip transport as a reliable discriminator:
    it does not identify DSTS-gate versus STARTTRANSFER status on this handoff.
    The next edit must therefore use a retained device-side trace/register
    channel or a source-backed link-state/Run-Stop correction, not another
    host-only blip permutation.
17. Run `2889382.0` added qpr1's event-ring republish and gadget restart at
    the USB2 Run/Stop boundary while retaining timer-only event consumption.
    It retained 3,593 frames / 2,343,023 bytes for 65.2 s, pcap SHA
    `c024df16e7fa500b377c34737aaaa4c8712103161329a74006578684c67f69f9`.
    The host result was unchanged: one HS attach, address-0 zero-payload
    descriptor traffic, `-110`, then three `-71` completions; no `1234:0001`.
18. Run `2892610.0` added qpr1's gadget-start-only-at-Run/Stop ordering to
    the same profile. It retained 3,593 frames / 2,343,023 bytes for 65.2 s,
    pcap SHA
    `b06c4ec676a1edd75154c53691bb8cfbcc630310a7728e69303686ec58922914`.
    It reproduced the exact same HS attach and `-110`/`-71` boundary. The
    Android gadget-start boundary family is therefore non-discriminating;
    the next hardware A/B is the source-backed HS-PHY resume boundary after
    DWC3 global-control setup.
19. Run `2901798.0` initially attempted that HS-PHY resume A/B through an
    inherited shell environment, but the loop runner intentionally sanitized
    that variable before the child build; it is not counted as an effective
    A/B. The formally wired `--hsphy-ref-after-gctl` retry was `2906386.0`:
    the generated build-script output contains
    `rustc-cfg=fullerene_aarch64_usb_hsphy_ref_after_gctl`. It retained 3,953
    frames / 2,366,423 bytes for 74.9 s, pcap SHA
    `a6bc197773adf3672569404855be77d1ab227e5535ea7cab1a1c518e76beab23`.
    The host still saw Fastboot disconnect, one HS attach, and
    `device descriptor read/64, error -110`; no `1234:0001` appeared. The
    ref-clock resume boundary is therefore non-discriminating on Bramble.
    The new CLI/build wiring is retained for reproducibility, while the next
    source target returns to read-only RX/SOF versus DWC3 event-ingress
    evidence rather than another clock reassertion permutation.

20. Run `2998264.0` applied the upstream msm_hsphy initialization details that
    were missing from the source-exact branch: ATE reset, TEST1 test-data and
    toggle bits, VATESTENB/TEST0 cleanup, and separate `SUSPEND_N_SEL` then
    `SUSPEND_N` writes. The integrated Tshark capture retained 3,233 frames /
    2,319,623 bytes for 55.6 s (SHA
    `a20f0f68fe8c32c0b57fc4df1d8b567f3a7674db6a006baded5810a00c50e7b7`). The
    host still reached HS attach and `device descriptor read/64, error -110`;
    no `1234:0001` appeared. This source-exact PHY delta is non-discriminating.
21. Run `3004815.0` removed the ABL-style event-ring EHB preservation while
    retaining the same profile. It reproduced the prior 3,593-frame /
    2,343,023-byte, 65.1-second capture (SHA
    `f8a721c8f9db79c7a8476acfac21732cb77df4a09597d5cd7772233883b6c9`) and the
    same zero-payload address-0 `-110` boundary. EHB clearing is therefore not
    the missing event-ingress fix.
22. The loop recovery path was corrected after observing that its five-second
    post-capture grace expired before Android had re-enumerated. The grace is
    now 45 seconds, separate from the 150-second candidate/replay wait. Run
    `3010590.0` validated it with integrated Tshark: 3,233 frames /
    2,319,623 bytes for 55.7 s (SHA
    `4df658ebf16f863c00ba72782a5a3dbb40a7dfe6020ac92c432938e0c6d8aaec`),
    Android was detected at 64 s, and the harness automatically returned the
    handset to Fastboot using only the allowed ADB-to-bootloader transition.
    The USB bring-up boundary itself remains unchanged: HS attach followed by
    the address-0 descriptor timeout, with no Fullerene identity.
23. Run `3023138.0` made the qpr1 pre-reset ordering test source-faithful by
    selecting the newly audited source-exact `msm_hsphy_init()` sequence when
    `--hsphy-before-reset` is enabled, before DWC3 CSFTRST. The integrated
    Tshark capture retained 3,593 frames / 2,343,023 bytes for 65.0 s (SHA
    `e86fc310007829904740d11f968e36cb5cf6b92a152a98955b98a202b4a73c62`). It
    reproduced HS attach followed by the same address-0 `-110`; no
    `1234:0001` appeared. The pre-reset source-exact ordering is now closed as
    non-discriminating; the remaining fault domain is HS RX/SOF or DWC3 event
    generation/ownership rather than another EP0/TRB permutation.
24. Run `3035418.0` added the source-backed active-resume differential
    `--hsphy-clear-sleepm` to the same attach-reaching source-exact profile.
    The integrated Tshark capture retained 3,548 frames / 2,340,095 bytes
    for 65.8 s (pcap SHA
    `296f1a671e629aa123105e5727a274ae273c54bce79ffa94fe4f5980d063c827`).
    The host again saw one HS attach, an address-0 descriptor timeout
    (`-110`), and three zero-payload `-71` retry completions; no `1234:0001`
    appeared. Android was detected at 64 s and the harness automatically
    returned the handset to Fastboot. Clearing SLEEPM after the source-exact
    PHY init is therefore non-discriminating; the remaining fault domain is
    still HS RX/SOF or DWC3 event generation/ownership.
25. Run `3042098.0` removed `--usb2-preserve-phy-interface` from the same
    profile, so qpr1's pre-reset `dwc3_hs_phy_setup()` UTMI/PHYIF/TRDTIM
    programming was exercised instead of preserving Fastboot's interface
    fields. Tshark retained 3,593 frames / 2,343,023 bytes for 65.5 s (pcap
    SHA `9538ee954a197760ba1a447456762b2178a9e24e2768b03729e6c442be69563c`).
    The result was unchanged: HS attach, four address-0 descriptor requests,
    zero payload, three `-71` retries, and no `1234:0001`. Android and
    Fastboot recovery again completed automatically. The pre-reset UTMI
    interface programming is non-discriminating.
26. Run `3046227.0` added `--hsphy-restore-suspend-n-after-runstop`, restoring
    the raw qpr1 HS-PHY `SUSPEND_N` bit immediately after the DWC3 Run/Stop
    write. Tshark retained 3,593 frames / 2,343,023 bytes for 65.3 s (pcap
    SHA `23fda80a8d75a4a3d70139b26e4985bf15285b8126ed37b079432e7f341e6a6e`).
    The host still saw HS attach, four address-0 descriptor submissions with
    zero payload, three `-71` retries, and no `1234:0001`. Android and
    Fastboot recovery completed automatically. A post-Run/Stop SUSPEND_N
    restore is non-discriminating; the remaining fault domain is DWC3 event
    generation/interrupt delivery or its ownership boundary.
27. Run `3059955.0` added the opt-in `--usb2-extended-setup-arm`, extending
    the initial EP0 SETUP STARTTRANSFER retry window from 400 ms to 5 s after
    Run/Stop. This directly covers the measured roughly 5-second interval
    between Bramble HS attach and its first Device Descriptor request. The
    integrated Tshark capture retained 3,919 frames / 2,364,215 bytes for
    75.5 s (pcap SHA
    `dcce417404829763ce4926cd80928d01dac0e710ead4338839a67431359f5e7f`).
    The host still saw one HS attach, four address-0 descriptor submissions
    with zero response payload, and three `-71` completions after the initial
    `-110`; no `1234:0001` appeared. Android returned at 64 s and the harness
    automatically returned to Fastboot. The extended initial-arm window is
    non-discriminating; the remaining target is DWC3 event generation/
    interrupt delivery/ownership or HS RX/SOF ingress.
28. Run `3068240.0` added the read-only `--signal-probe --signal-early-drop 5`
    diagnostic to the same profile, with an 80-second observation window. No
    SOF-triggered pull-up drop occurred, while the integrated Tshark capture
    retained 3,671 frames / 2,348,074 bytes for 66.2 s (pcap SHA
    `d31b56b7f522669ab943dc04c2d2bc12e483c562005923734ca2d972d177a6e3`).
    The host still saw HS attach, the address-0 `-110` timeout, and three
    zero-payload `-71` retries; no `1234:0001` appeared. This is a bounded
    read-only indication that DSTS SOF progress is absent before the event
    consumer boundary; it shifts the next source target toward HS-PHY/RX/SOF,
    while not by itself proving whether the PHY or DWC3 link layer owns the
    missing packet.
29. Run `3075860.0` added `--usb2-clear-susphy-after-runstop`, clearing the
    DWC3 USB2 `SUSPHY` bit immediately after the qpr1 Run/Stop guard restored
    it. Tshark retained 3,687 frames / 2,349,119 bytes for 68.3 s (pcap SHA
    `802e61c7675e00c71b383ae595ea030ae0e8fde51b48535aba3b9c3ca21cab08`).
    The host result was unchanged: HS attach, four zero-payload address-0
    descriptor submissions, `-110`, three `-71` completions, and no
    `1234:0001`. Android and Fastboot recovery completed automatically. The
    post-Run/Stop DWC3 SUSPHY clear is non-discriminating.
30. Runs `3081083.0` and `3084628.0` used the read-only `--signal-link-state`
    and `--signal-raw-link` selectors on the same profile. Neither produced a
    host-visible diagnostic disconnect cycle, so the retained signal path did
    not observe a stable classified DSTS link state or a nonzero raw nibble
    before the observation window ended. Their integrated Tshark captures
    retained 3,654 frames / 2,346,979 bytes for 61.4 s (SHA
    `bb9871eedaad58a88af1036672c05b03cb1cc243793965ead336b1c83c496886`) and
    the corresponding raw-link run ended at the same HS-attach/descriptor
    timeout boundary. This keeps the diagnosis qualified: the DWC3 link
    readout is not a reliable discriminator on this handoff, so the next fix
    remains a source-level HS-PHY reference/power boundary rather than a
    guessed event-consumer rewrite.
31. Run `3090295.0` exposed the existing qpr1-derived
    `HSPHY_REF_AFTER_RUNSTOP` implementation through the CLI and reasserted
    the HS-PHY reference clock immediately after Run/Stop, retaining the
    extended 5-second EP0-arm window. Tshark retained 3,913 frames /
    2,363,823 bytes for 74.1 s (pcap SHA
    `01703a43d3daff1d28b6acf673d390db525f790d0af6cfd2f65a9a2d189e34bc`).
    The host still saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android and Fastboot recovery completed automatically. The immediate
    reference-clock reassert is non-discriminating.
32. Run `3097126.0` added the newly exposed
    `--hsphy-power-after-runstop`, re-sending the three qpr1 HS-PHY RPMh rail
    enables immediately after Run/Stop while retaining the 5-second EP0-arm
    window. Tshark retained 3,879 frames / 2,361,615 bytes for 74.5 s (pcap
    SHA `d32283014d6aa460bf501eee919b00bc8f2136a32bb718b6a8efe9cfe91d23d9`).
    The host still saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android and Fastboot recovery completed automatically. Immediate
    post-Run/Stop HS-PHY rail reassert is non-discriminating.
33. Run `3103635.0` exposed the existing qpr1-derived 150-us HS-PHY POR
    settle delay through `--hsphy-por-delay-150`, retaining the same direct
    handoff, source-exact PHY, extended EP0-arm, and automatic-recovery
    profile. Tshark retained 3,913 frames / 2,363,823 bytes for 74.2 s (pcap
    SHA `6d82407bbac22be800c5706699561a971291afe3f91eb0af1478364d86e5ba29`).
    The host again saw one HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android returned at 63 s and the harness automatically returned to
    Fastboot. The POR settle-delay A/B is non-discriminating; the next
    source target remains the HS-PHY RX/SOF ingress boundary.
34. Run `3114788.0` added the isolated legacy `RTUNE_SEL` write through
    `--hsphy-rtune` to the qpr1 source-exact HS-PHY sequence. Tshark retained
    3,753 frames / 2,353,423 bytes for 69.4 s (pcap SHA
    `7547f4394fae8751a50eb8c1b7c84621fb79416f0d282caeadb5ddaf28185dfe`).
    The host again saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android returned at 64 s and the harness automatically returned to
    Fastboot. The legacy RTUNE write is non-discriminating; the next
    source-backed power boundary is the RPMh regulator-set ownership.
35. Run `3119222.0` enabled the existing `--hsphy-all-regulator-sets` A/B,
    sending each of the three HS-PHY rail requests through both the active and
    Sleep RPMh TCS families while retaining the source-exact PHY profile.
    Tshark retained 3,593 frames / 2,343,023 bytes for 65.1 s (pcap SHA
    `b1bdaf7b00237f9a4f1af431fe03bba462a1b219ed0e7fd2374b4171d2a4de31`).
    The host again saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android returned at 64 s and the harness automatically returned to
    Fastboot. Active-plus-Sleep TCS ownership is non-discriminating; the next
    source-level candidate remains the HS-PHY power mode/clock boundary.
36. Run `3125431.0` exposed the existing regulator-mode differential through
    `--hsphy-vdd-lpm`, requesting LPM rather than HPM for the HS-PHY `vdd`
    rail while keeping the active-TCS, source-exact, and automatic-recovery
    profile otherwise unchanged. Tshark retained 3,965 frames / 2,367,188
    bytes for 76.2 s (pcap SHA
    `5ed8c614433f3ae5265ca2ead0b92f6894d5a6f2a8620fa5e57de22eae531a62`).
    The host again saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android returned at 65 s and the harness automatically returned to
    Fastboot. The HS-PHY vdd HPM/LPM choice is non-discriminating.
37. Run `3130948.0` enabled the existing EUD ownership-release path through
    `FULLERENE_AARCH64_USB_DISABLE_EUD=1`: SCM mode-manager disable followed
    by the direct Bramble EUD_EN CSR clear, before source-exact HS-PHY init.
    Tshark retained 3,639 frames / 2,346,015 bytes for 68.0 s (pcap SHA
    `3cb04018466ef848b1d93e8d6675c4653d4ccead4ceb33e0e5ac71420be8fba1`).
    The host again saw HS attach, four zero-payload address-0 descriptor
    submissions, `-110`, three `-71` completions, and no `1234:0001`.
    Android returned at 64 s and the harness automatically returned to
    Fastboot. Releasing EUD ownership is non-discriminating for this boundary.
38. Run `3137358.0` restored raw HS-PHY `SUSPEND_N` immediately after the
    DWC3 CSFTRST boundary. Tshark retained 3,833 frames / 2,358,623 bytes
    for 72.0 s (pcap SHA
    `d8bbaa5cec8c54e94632da8ad1cdada8ae9ef4b90266e2a4187506bd98079fca`);
    the host still reached HS attach followed by the same
    zero-payload address-0 descriptor timeout/retries, with no `1234:0001`.
    Android and Fastboot recovery completed automatically.
39. Run `3144426.0` extended the deferred EP0 arm window to 10 seconds with
    `--usb2-long-setup-arm`. Tshark retained 3,725 frames / 2,351,588 bytes
    for 69.2 s (pcap SHA
    `b92b717b9373231cc96ca639fa3947346b5d39adfbfdbe76ee3686224bdb01e4`).
    The HS attach and address-0 descriptor boundary were unchanged; no
    `1234:0001` appeared.
40. Run `3155228.0` added the one-shot automatic EP0 arm-window recovery.
    Tshark retained 3,673 frames / 2,348,223 bytes for 67.5 s (pcap SHA
    `ffa0d6ffac47b7d5751db34bfe6cf6bc26cbbc4bcbf9a698e61f529fe6603a0b`).
    The recovery completed automatically, but the host still timed out at
    the zero-payload Device Descriptor boundary; no `1234:0001` appeared.
41. Run `3162319.0` added the late 30-second automatic EP0 soft-reset
    recovery. Tshark retained 3,247 frames / 2,320,519 bytes for 56.6 s
    (pcap SHA
    `6512eb224566e1265e42b9d5fd3bbb28503102fce650d9a700172b8e0bc3338a`).
    The host boundary remained `-110` with no `1234:0001`; Android and
    Fastboot recovery completed automatically.
42. Run `3170305.0` cleared USB2 `SUSPHY` at the host USB Reset boundary.
    Tshark retained 3,679 frames / 2,348,615 bytes for 69.0 s (pcap SHA
    `1615c6b1a3f31218500eb0446e74fcafe25b53431f092fdfc73ae6131e6d401c`).
    The HS attach, zero-payload descriptor failure, and automatic recovery
    were unchanged; no `1234:0001` appeared.
43. Run `3184206.0` added the source-defined QMP USB3 link-training
    workaround at USB Reset. Tshark retained 3,873 frames / 2,361,223 bytes
    for 72.9 s (pcap SHA
    `db6158ddd8fa2bffc49731f0f3b021292f5ca94ced8a004a1d05be95ebbd252d`).
    The host still reached HS attach and `-110`; no `1234:0001` appeared.
    The status-gated QMP operation was non-discriminating for this USB2
    boundary.
44. Run `3189101.0` removed the Fullerene-only EP0 stop/rearm reset ordering,
    preserving the armed Setup transfer as in qpr1's USB Reset path. Tshark
    retained 3,593 frames / 2,343,023 bytes for 65.2 s (pcap SHA
    `2aff1380e1939b990d4803f1d16cc5adb9aa50d51998f296b9f8ae6e363d175b`).
    The host boundary was unchanged: HS attach, zero-payload address-0
    descriptor failure, and no `1234:0001`; automatic Android-to-Fastboot
    recovery completed.
45. Run `3195668.0` tested the existing `--ep0-initial-512` candidate after
    the qpr1 EP0 source audit. Tshark retained 4,743 frames / 2,425,388
    bytes for 70.4 s (pcap SHA
    `bd6ef665baed23b22cdcc7227a55eea25a233a4dda778dad65d58d9c8b8db2ca`).
    The host boundary was unchanged: HS attach, zero-payload address-0
    descriptor failure, and no `1234:0001`; automatic recovery completed.
46. Run `3208380.0` attempted the first condition-3 EP0 SETUP readout with
    integrated Tshark. It retained 199,779 frames / 36,798,998 bytes for
    96.4 s (pcap SHA
    `2485eec57f009fe855d84fbc581f83931b57108b3156d51b1438e884f4588d80`).
    The run was invalid as a USB discriminator: the probe's boolean failure
    path interpreted selector `3` as an immediate pull-up drop, so the host
    saw Fastboot disconnect but no new device attach or Device Descriptor
    request. The condition-selector handling was corrected before retry.
47. Run `3217535.0` retried the corrected condition-3 readout. It retained
    175,839 frames / 32,645,894 bytes for 84.7 s (pcap SHA
    `83d16029237c6a68c990f5d2d1a210a79c62aaad3db7449ff256ebf4813b6cb7`).
    The host still saw only Fastboot disconnect and no subsequent attach or
    Device Descriptor request. Source review found that the synchronous
    observation window still ran before the U0-gated `start-after-connect`
    arm; the next patch defers that window to the normal polling owner.
48. Run `3226741.0` tested that deferred condition-3 readout. Tshark retained
    177,647 frames / 32,988,448 bytes for 85.8 s (pcap SHA
    `76ddd4a0639fc7ce3a367a60847fafe3d88b824a4b9f1745679b0339be280a47`).
    The host again saw only the Fastboot disconnect and no new attach. The
    remaining false-positive was in `update_signal_latches()`: it inspected
    the aliased setup/TRB memory before `EP0_SETUP_ARMED` became true. The
    readout is now gated on that ownership bit; hardware validation is pending
    the next Fastboot-visible run.
49. Run `3235760.0` validated the setup-ownership-gated condition-3 retry with
    integrated Tshark. It retained 172,485 frames / 32,195,396 bytes for
    83.7 s (pcap SHA
    `f23fa3e0fdd65c70352345e94748606d886ebd88f5c0ea84835427df7dae52be`).
    The host saw Android SuperSpeed `18d1:4ee0` and then a disconnect, but no
    Fullerene attach or Device Descriptor GET; `1234:0001` did not appear.
    Automatic recovery completed, leaving the handset device-absent. The next
    control removes the signal probe while retaining the same attach-reaching
    USB profile to test whether the diagnostic path itself is perturbative.
50. Run `3245338.0` removed `--signal-probe` as a control while retaining the
    same CLI-level USB profile. This is not a passive toggle: without the flag
    the image selects the normal Android-init compile-time path. Tshark
    retained 204,431 frames / 37,458,034 bytes for 98.0 s (pcap SHA
    `3a661c419b2ff152362543df70fdf2e0670e6931d6bb1f8f1098b4be456faade`). The
    host saw Android SuperSpeed `18d1:4ee0` and then a disconnect, with no
    Fullerene attach, Device Descriptor GET, or `1234:0001`; this is not a
    valid probe-perturbation A/B. The next run returns to the known
    HS-attach-reaching standalone profile.
51. Run `3254225.0` reused the known HS-attach-reaching standalone profile
    with the setup-ownership-gated condition-3 diagnostic. Tshark retained
    153,605 frames / 28,609,548 bytes for 74.4 s (pcap SHA
    `deb244f36fc9652e6a64f17fff7173f7c7e53f771c987a32954a3547f35fd1d1`). The host reached HS
    attach, then the same descriptor `-110` boundary and three `-71`
    completions; condition 3 did not fire, no Fullerene `1234:0001` appeared,
    and Android/Fastboot recovery completed automatically. This rejects the
    setup-buffer false positive and leaves the boundary at USB2 RX/SOF or DWC3
    event ingress before software-visible SETUP payload; downstream EP0/TRB
    permutations remain out of scope.
52. Run `3259735.0` moved the early-drop selector to condition 5 (SOF) while
    retaining the corrected host-USB-reset gating. Tshark retained 121,821
    frames / 23,173,207 bytes for 59.3 s (pcap SHA
    `ac8aca7e3941e19fa066e2b7f586255a18b6ded89fd4587b8c07693270026c24`).
    The host reached HS attach then `-110`; no condition-5 drop occurred and
    no `1234:0001` appeared.
53. Run `3264802.0` repeated condition 5 after the USB-reset latch was moved
    into the signal sampler. Tshark retained 125,514 frames / 23,693,671
    bytes for 60.5 s (pcap SHA
    `2cbc083a3bf8c8e8298c0261c3a3232f401fef5e0b7b03bd4a82e01760789846`).
    The host again saw HS attach then `-110`, with no post-reset SOF readout or
    condition-5 drop; this removes the pre-reset SOF false-positive.
54. Run `3268544.0` narrowed condition 1 to a DWC3-consumed host USB Reset
    event. Tshark retained 4,815 frames / 2,430,583 bytes for 70.4 s (pcap
    SHA `8d28f16a89f5939ef352baa6bd274442fa242d89c41f5218888f6af2c1d83a1f`).
    The host reached HS attach, `-110`, and three `-71` completions; condition
    1 did not fire, so no software-visible USB Reset event was observed.
55. Run `3272197.0` added the source-backed Factory-ABL event-consume/EHB
    ordering A/B. Tshark retained 147,992 frames / 27,605,079 bytes for
    71.6 s (pcap SHA
    `5fa8430867caecad7bb0efd39146bdd0d0707b31a0eb4314b4a7b476b0a33cee`).
    HS attach, descriptor `-110`, three `-71`, and no condition-1 drop were
    unchanged; EHB/consumer ordering is not the boundary fix.
56. Run `3278205.0` added a pre-connect synthetic event-DMA probe plus CPU
    readback and an event-word gate. Tshark retained 152,687 frames /
    28,363,013 bytes for 73.6 s (pcap SHA
    `1d5f5545942fbb850cbbbb84fec6eed43f74b47102c4765ad55d144b9bbd57ee`).
    Because the HS pull-up was published with `--signal-evt-data-gate 1`, the
    probe's nonzero event-word gate passed; this is evidence that a synthetic
    command event can reach the event ring, not evidence that the host Reset
    event arrived. The host still stopped at HS attach plus descriptor
    `-110`; Android/Fastboot recovery completed automatically.
57. Run `3285863.0` attempted the post-Run/Stop event-DMA probe while also
    publishing `post-code`. It retained 241,807 frames / 44,110,536 bytes for
    117.1 s (pcap SHA
    `ab674bcc3bc78127253d3e105ea89a28aea545ef1c80ff74fdc7e279d633f54e`).
    The probe ended the live EP0 transfer before host attach, so the host saw
    only Fastboot disconnect and the handset did not return within the bounded
    recovery grace. This is an invalid discriminator and the profile is not to
    be repeated; physical Fastboot recovery is required before the next run.
58. Run `3295741.0` tested the Type-C parent-IRQ route with the same integrated
    Tshark/usbmon capture. It retained 294,802 frames / 53,095,128 bytes for
    142.0 s (pcap SHA
    `c5bda06ad2ce045e021bc17a417824f60e53882a0bae1248f11b7ca5334b8366`).
    The handset disappeared from Fastboot before any Fullerene HS attach or
    descriptor request; the host log contains only the Fastboot disconnect.
    This route is perturbative/non-discriminating for the current handoff and
    is not to be repeated. The post-Run/Stop probe has now been changed to the
    read-only DWC3 GETEPSTATE command; no hardware result exists yet.
59. Run `3309787.0` exercised that read-only post-Run/Stop probe without the
    Type-C IRQ route, with Tshark/usbmon retained for 117.0 s: 240,583 frames /
    43,990,100 bytes (pcap SHA
    `becdaed4a00b6a870c825f82779cbcb39a38e95acc500b38a6b40eed7cd0869f`).
    Only the Fastboot disconnect was observed; there was no Fullerene HS
    attach, descriptor request, `1234:0001`, or host-visible `post-code`, so
    this run cannot discriminate the GETEPSTATE result. The handset ended
    device-absent and requires physical Fastboot recovery; do not repeat this
    exact timer-poll profile before recovery.
60. Run `3320590.0` repeated the known HS-attach-reaching direct USB2 control
    with Tshark/usbmon. The 95.3 s pcap retained 195,842 frames /
    35,968,650 bytes (SHA
    `9e2e752cc82d5b5c723023975ccd801eda664a81cd90a648f10830be2344d8d6`).
    The host reached HS attach, timed out on the Device Descriptor with
    `-110`, then produced the same `-71` retries; stock Android SuperSpeed
    fallback and automatic Fastboot recovery completed. This preserves the
    control boundary for the read-only post-Run/Stop probe.
61. Run `3324678.0` added the read-only DWC3 GETEPSTATE post-Run/Stop probe to
    that same control. The 93.9 s pcap retained 193,615 frames /
    35,533,609 bytes (SHA
    `d5c5914c90f4897f3222a0aff5f92a785dc4b01b3469cc59567fb8f02c40a814`).
    Host behavior was unchanged: HS attach, descriptor `-110`/`-71`, no
    `1234:0001`, and automatic Android-to-Fastboot recovery. Because Fullerene
    never enumerated, no `post-code` result was available; the probe did not
    perturb the known boundary.
62. Run `3335122.0` added the source-backed HS-PHY `COMMON0.SIDDQ` clear to
    the known control profile. Tshark retained 186,026 frames /
    34,183,663 bytes for 90.1 s (pcap SHA
    `9266949552d994871f2ea4603799520bd13f8a3fcfe1cd22d7e1fa4da05a7f70`).
    The host still reached HS attach and the same zero-payload descriptor
    `-110`/`-71` boundary; no `1234:0001` appeared. Android fallback and
    automatic Fastboot recovery completed, so this single missing PHY write is
    rejected as the fix; the next source-backed A/B is the EUD-owned device
    branch.
63. Run `3340631.0` exercised the official Bramble EUD-owned device-mode
    branch (`--hsphy-eud-device-mode`) on the same known control profile.
    Tshark retained 187,297 frames / 34,378,320 bytes for 90.5 s (pcap SHA
    `dd87ba1c9cd4437543e54f6e5cf932f7f392e0cfee9f536b81ef4e1254677ca7`).
    HS attach, the zero-payload address-0 descriptor timeout `-110`, and the
    three `-71` retries were unchanged; no `1234:0001` appeared. Android
    fallback and automatic Fastboot recovery completed, so the EUD-owned
    source branch is rejected as the fix.
64. Run `3382739.0` added the remaining qpr1 `dwc3_phy_setup()` USB3-side
    pre-reset delta (`--usb2-source-phy-setup`) to the corrected source-exact
    HS-PHY/direct-USB2 control. Tshark retained 193,758 frames / 35,496,170
    bytes for 93.6 s (pcap SHA
    `7ab5304af30294fc49c7c351cafc8265979c8a227c33347eab842f84d7737895`).
    The host still reached HS attach, timed out on the zero-payload address-0
    Device Descriptor with `-110`, then produced the same `-71` retries; no
    `1234:0001` appeared. The USB3-side pre-reset delta is therefore
    non-discriminating for this USB2 boundary. Android fallback and automatic
    Fastboot recovery completed, with final state `fastboot-available`.
65. Run `3388238.0` added the official qpr1 `dwc3_device_core_soft_reset()`
    DCTL write/poll boundary (`--usb2-source-exact-device-reset`) on top of
    the corrected source-exact HS-PHY plus USB3-PHY-setup profile. Tshark
    retained 189,903 frames / 34,953,498 bytes for 92.4 s (pcap SHA
    `48725ab06167463d75bf91156779b3271b7f64865f1396a3b1a1aabcf5fd0e9a`).
    The host again reached HS attach, issued a zero-payload address-0 Device
    Descriptor request with `-110`, then the same `-71` retries; no
    `1234:0001` appeared. The source-exact DWC3 reset boundary is therefore
    non-discriminating. Android fallback and automatic Fastboot recovery
    completed, with final state `fastboot-available`; raw usbmon SHA-256 is
    `7e499aab2a937d8031fb0f18dfec7579143995433de9a54ccdf496568e1e98b7`.
66. Run `3438675.0` added the source-ordered qpr1 peripheral-start prefix
    (`--usb2-source-peripheral-start`) before the direct DWC3 device-core
    reset: VBUS/session override, DEVICE port mode, and `dis_sleep_mode()`.
    Tshark retained 5,069 frames / 2,440,057 bytes for 87.2 s (pcap SHA
    `aafe1009094ab24ca54c02418501823ea55771b2af3c3124b891fec79cfd5d85`);
    raw usbmon SHA-256 is
    `5882700ad53553d03bfc2ae403c9b58978a10e2aa9102c82c834aac9496703ef`.
    The host still reached HS attach, timed out on the zero-payload address-0
    Device Descriptor with `-110`, then produced the same three `-71`
    completions; no `1234:0001` appeared. Android fallback and automatic
    Fastboot recovery completed, with final state `fastboot-available`. This
    source-order correction is non-discriminating; collection therefore
    narrows the remaining fault to USB2 receive/clock recovery or ownership
    below the DWC3 gadget-start prefix, rather than ending the investigation.
67. Run `3453297.0` added the qpr1 DP/DM charger-detection datapath-override
    clear (`--hsphy-clear-datapath-override`) after the existing normal OPMODE
    restore. Tshark retained 6,566 frames / 2,546,572 bytes for 95.5 s (pcap
    SHA `4501230b94182b6faa89ae43357ba857b8e21ae501dbae484a22d8278060385b`);
    raw usbmon SHA-256 is
    `74ffc5ed4295b269595c5a00ef6d61b321c29bedcf5be4e278cf62e39a977f1d`.
    The host still reached HS attach, timed out on the zero-payload address-0
    Device Descriptor with `-110`, then produced the same three `-71`
    completions; no `1234:0001` appeared. Android fallback and automatic
    Fastboot recovery completed, with final state `fastboot-available`. The
    datapath-override clear is therefore non-discriminating; source-directed
    follow-up remains at USB2 PHY RX/SOF or event ingress rather than EP0/TRB.
68. Run `3462884.0` kept that same source-directed profile and added the
    read-only post-Run/Stop HS-PHY state readout
    (`--utmi-postrun-readout hsphy-state-mask`). Tshark retained 4,972 frames /
    2,436,930 bytes for 85.3 s (pcap SHA
    `f67475880eac6d2ff813978288af4fe84a2c5a381294428dbeb3ec38209b474a`);
    raw usbmon SHA-256 is
    `48823319ec55d965e72fdd0d03e2fc2416c813987e67ae827bf38371793d3790`.
    The host-visible boundary remained HS attach, zero-payload descriptor
    `-110`, then `-71` retries, with no `1234:0001`. The readout did not
    produce a distinct host-visible state bucket, so it does not justify a new
    PHY write; Android fallback and automatic Fastboot recovery completed.
69. Run `3482487.0` added the Bramble-PVT DT override candidate
    (`--hsphy-dtbo-bramble-pvt`) to the same source-directed profile, now that
    the handset was manually placed in Fastboot. Tshark retained 4,898 frames /
    2,428,026 bytes for 84.8 s (pcap SHA
    `8bc3c9b5b9e60aaf2bba276191521bc46669ba367cb5d392ba990dae2b399a13`);
    raw usbmon SHA-256 is
    `e5e14699d7cdcb2d5c07cb526c5be712c6a61cae51356647661ea3b022a3d043`.
    The host-visible boundary remained HS attach, zero-payload descriptor
    `-110`, then `-71` retries, with no `1234:0001`. The PVT override is
    therefore non-discriminating for this failure; Android fallback and
    automatic Fastboot recovery completed with final state
    `fastboot-available`.
70. Run `3495589.0` exercised the repaired qpr1-style Type-C parent/child IRQ
    setup and deferred threaded-handler path (`--irq-route typec`) on the same
    source-directed USB2 profile. Tshark retained 5,292 frames / 2,457,730
    bytes for 94.4 s (pcap SHA
    `6b82ff63f412c9db2cb8c8bd39224ebe2428029121b0cccb02dffd0f55265dd0`);
    raw usbmon SHA-256 is
    `c0fcd6eeb61b4f937f68255f93caf15058fab98ea9e644ed668607dc47082670`.
    The repaired Type-C route still reached HS attach but returned a
    zero-payload address-0 Device Descriptor timeout `-110`, followed by
    `-71` retries; no `1234:0001` appeared. The source-order/IRQ-threading
    correction is therefore non-discriminating. Android fallback and
    automatic Fastboot recovery completed with final state
    `fastboot-available`; the run remained RAM-only with no flash or configfs
    operation.
71. Run `3507548.0` added the qpr1 Android resume-order controller clock
    branch rearm (`--clock-branches-rearm`), enabling `iface`, `core`, and
    `sleep` before the existing UTMI branch rearm. Tshark retained 7,380
    frames / 2,612,219 bytes for 85.1 s (pcap SHA
    `17f51aa8f66fca1b0ba38b76e6d5dee96da2c404cdd166ff0f93623cf50ee998`);
    raw usbmon SHA-256 is
    `1ae573d67246da191a8d1ce78609d97600e692c77ad84982248549659b3552fa`.
    The host-visible result was unchanged: HS attach, zero-payload address-0
    descriptor `-110`, then `-71` retries, with no `1234:0001`. Automatic
    Android/Fastboot recovery completed and the final state was
    `fastboot-available`. The initial controller-branch state is therefore
    non-discriminating for this failure; the remaining boundary is still
    below successful USB2 control-response ingress.

72. Run `3515794.0` added the broader USB2 direct-handoff controller reset
    (`--usb2-full-core-reset`): after the DCTL device reset it asserted the
    DWC3 core soft-reset and USB2 PHY-facing reset as a single A/B boundary.
    Tshark retained 8,635 frames / 2,703,432 bytes for 89.3 s (pcap SHA
    `634a2e9e959b7826aec68818e0b95b3aad8d337c05e27b979bda3365933ca7b4`);
    raw usbmon SHA-256 is
    `d577c5549b454ea7932d694d980b67956637a20c053507ba8c8e9f1dbc7321af`.
    The host-visible result was unchanged: HS attach, a zero-payload
    address-0 descriptor timeout `-110`, then immediate `-71` retries, with
    no `1234:0001`. Automatic Android/Fastboot recovery completed and the
    final state was `fastboot-available`. A broader controller-domain reset
    therefore does not repair the first USB2 control response and is
    non-discriminating at this boundary; no flash or configfs operation was
    used.

73. Run `3520992.0` removed only the explicit QUSB2 PHY BCR reset pulse
    (`--skip-usb2-phy-reset`) while retaining the qpr1 source-exact analog
    sequence and the rest of the attach-reaching profile. Tshark retained
    5,144 frames / 2,448,120 bytes for 89.5 s (pcap SHA
    `994ea3f99774d720a8424bf390afd36eafd20bc755859525630cce632c2f27a5`);
    raw usbmon SHA-256 is
    `7338330227e5d3d0111cfa002ae3196e21511cb84bb461463589ef0e12ab4e8d`.
    HS attach and the zero-payload address-0 descriptor `-110`/`-71`
    boundary were unchanged, with no `1234:0001`. Android/Fastboot recovery
    completed automatically and final state was `fastboot-available`. The
    BCR pulse is therefore not the differentiator; no flash or configfs
    operation was used.

74. Run `3526881.0` repeated the late automatic EP0 arm-window recovery
    (`--usb2-arm-window-recovery`) on the current source-directed profile.
    Tshark retained 5,092 frames / 2,444,730 bytes for 88.8 s (pcap SHA
    `e899701192028748dec816fb5e839ff6feec537e353f1d8a21205517602e9f12`);
    raw usbmon SHA-256 is
    `92f52279486fca1f06da4e13cfa7cdd5b1583db079210a1b87484fffb6e8dbf7`.
    The host still reached HS attach and the zero-payload address-0 descriptor
    `-110`/`-71` boundary, with no `1234:0001`. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The late
    automatic reset/re-arm path is therefore non-discriminating; no flash or
    configfs operation was used.

75. Run `3542414.0` combined the qpr1 pre-reset HS-PHY ordering with the
    DT-correct choice to preserve the inherited USB2 PHYIF/TRDTIM fields
    (`--hsphy-before-reset --usb2-preserve-phy-interface`). The integrated
    Tshark capture retained 195,038 frames / 35,672,158 bytes for 94.1 s;
    pcap SHA-256 is
    `544dea1da461d48a599df8ea17146b76624c3c15561471f16ad603b50a6a8f79` and
    raw usbmon SHA-256 is
    `566e0de7bacfc84c706b260dd45a17d39322f32064ee4b9d523a5272c31c845a`.
    The host again reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` and three immediate `-71` completions; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically
    and final state was fastboot-available. Preserving the DT-omitted
    interface fields does not move the HS-PHY/RX/SOF boundary; no flash or
    configfs operation was used.
76. Run `3560984.0` removed the extra `COMMON0.SIDDQ` clear from the
    source-exact qpr1 HS-PHY path, correcting it to the upstream
    `msm_hsphy_init()` register sequence. The build/QEMU/audit gates passed;
    the integrated Tshark capture retained 166,725 frames / 30,926,468 bytes
    for 81.0 s (pcap SHA-256
    `e7537c5d6d2aca9a3c4708ca0cf4b52b3e7820f8fd7027087f239faa276cff93`),
    and raw usbmon retained 169,407 records / 26,707,708 bytes (SHA-256
    `99951d4ab4c1e082353d0ee45e6180071fa8507bc0749850d1222628a05da7f3`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` and three immediate `-71` retries; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically
    and final state was fastboot-available. Removing the non-upstream SIDDQ
    write therefore does not move the HS-PHY/RX/SOF boundary; no flash or
    configfs operation was used.
77. Run `3569364.0` removed the extra post-init `SLEEPM` clear
    (`--hsphy-clear-sleepm`) so the direct path retained qpr1's HS-PHY
    source state through the handoff. The build/QEMU/audit gates passed; the
    integrated Tshark capture retained 190,680 frames / 35,090,705 bytes for
    92.8 s (pcap SHA-256
    `2e990b4f835903ce6ce7942b23f53bccdaf3ecaeb23bd2d54814243080af836e`),
    and raw usbmon retained 193,343 records / 30,485,977 bytes (SHA-256
    `2632a78865e4f2eb62503a47d7b4aae68b17262bbf29b7d80df7828061cc8094`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` and three immediate `-71` retries; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically
    and final state was fastboot-available. Keeping qpr1's SLEEPM state does
    not move the HS-PHY/RX/SOF boundary; no flash or configfs operation was
    used.
78. Run `3580131.0` added a new, isolated USB2 PHY BCR reset-hold A/B:
    `--hsphy-reset-delay-150` changes only the fixed reset assert hold from
    100 to 150 us, matching the upper bound of qpr1 `usleep_range(100, 150)`;
    it is separate from the existing post-POR delay flag. The build, QEMU,
    image-audit, and RAM-only boot gates passed. Integrated Tshark retained
    5,235 frames / 2,454,945 bytes for 93.4 s (pcap SHA-256
    `5598e3d851dc2519143f0220ae1148eaad1431d3216f317fbab9c43788b2508b`),
    and raw usbmon retained 5,246 records / 397,457 bytes (SHA-256
    `ad161f65e01069ec10a99e1a29323d2275d8c70ccd2fd30efb564a0aa4fae14c`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The reset
    hold upper-bound A/B therefore does not move the HS-PHY/RX/SOF boundary;
    no flash or configfs operation was used.
79. Run `3597618.0` was intended to add the source-audit USB2 VBUS/session
    A/B `--usb2-source-vbus-only`, clearing inherited `SW_SESSVLD_SEL` while
    retaining qpr1's `UTMI_OTG_VBUS_VALID` path. A later source audit found
    that the flag was emitted by the build harness but had no consumer in the
    direct USB2 handoff, so this run is not valid evidence for that hardware
    change. Its build, QEMU, image-audit, and RAM-only boot gates passed;
    Tshark retained 5,069 frames / 2,444,153 bytes for 87.4 s (pcap SHA-256
    `5f776f972254139e6bb1f10494c0c0d4fa67e2dbaf538d4b7b88810c73e40860`),
    and raw usbmon retained 5,080 records / 389,321 bytes (SHA-256
    `a8c7875e6b71f8427e315e4128914a87485138558f230d04e390eb213f800030`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    effective source-vbus-only implementation was corrected and tested in
    run `3683212.0` below; no flash or configfs operation was used.
80. Run `3608624.0` added the qpr1-derived HS-PHY `CTRL2.AUTO_RESUME` pulse
    A/B (`--hsphy-auto-resume-pulse`), holding the bit for 750 us after the
    final HS-PHY init and before the USB2 gadget contract/pull-up. The build,
    QEMU, image-audit, and RAM-only boot gates passed. Integrated Tshark
    retained 5,223 frames / 2,454,161 bytes for 90.9 s (pcap SHA-256
    `3460839ac0018a6246c887d91ebde8ff93ba4969ad1d95a8d2a2b44fa2f863d2`),
    and raw usbmon retained 5,223 records / 396,321 bytes (SHA-256
    `4447e5f4737a8fa4a2f2ad0feb8658105257c98dfd025e84fc8b680a35b661c0`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    auto-resume pulse is rejected as the fix; no flash or configfs operation
    was used.
81. Run `3625442.0` added the source-audit USB2 power-event A/B
    `--usb2-source-power-events`. It narrows the initial QSCRATCH PWR_EVENT
    mask to qpr1's P3-in notification and resolves simultaneous P3-in/P3-out
    status from the DWC3 link state, matching `dwc3_pwr_event_handler()`.
    Build, QEMU, image-audit, and RAM-only boot passed. Integrated Tshark
    retained 5,069 frames / 2,444,153 bytes for 87.3 s (pcap SHA-256
    `39b2ed9db56daeee789355d46d9ea9d604c3948db1f4314301b32b8ee1b1c9b1`),
    and raw usbmon retained 5,080 records / 389,321 bytes (SHA-256
    `53586aa990a03916b01df35412980e2518f8d4697e51ae8930edd23eec1aa4c5`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. This
    power-event correction does not move the HS-PHY/RX/SOF boundary; no flash
    or configfs operation was used.

82. Run `3642677.0` added the qpr1 source-order A/B
    `--hsphy-resume-clocks-after-reset`: with `--hsphy-before-reset`, Fullerene
    skips a duplicate post-reset analog `init_hsphy()` and resumes only the
    HS-PHY clock boundary, matching qpr1's `usb_phy_set_suspend(false)` after
    DWC3 reset. Build, QEMU, image-audit, and RAM-only boot passed. Integrated
    Tshark retained 4,669 frames / 2,418,153 bytes for 76.5 s (pcap SHA-256
    `7236c989a030edf4a152787876d4f069e8ab45c69060a15b727c346fb72d78d6`), and
    raw usbmon retained 4,680 records / 369,721 bytes (SHA-256
    `18cfe12dbacc67454533483baa762701454097f0f5f2af3920953952dbf6fcdd`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate `-71` retries; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically
    and final state was fastboot-available. The post-reset clock-resume
    ordering does not move the HS-PHY/RX/SOF boundary; no flash or configfs
    operation was used.

83. Run `3659508.0` added the source-boundary A/B
    `--hsphy-restore-suspend-n-after-reset` to restore the raw HS-PHY
    `CTRL2.SUSPEND_N` bit immediately after DWC3 CSFTRST, before endpoint
    resources and Run/Stop. Build, QEMU, image-audit, and RAM-only boot
    passed. Integrated Tshark retained 4,986 frames / 2,437,826 bytes for
    85.8 s (pcap SHA-256
    `1177ac48bb1410932f204aabaec3af8d92f0f3eac88a693e6765f048076f41`), and
    raw usbmon retained 4,986 records / 383,778 bytes (SHA-256
    `b0172f0ed6d51e5de074d15320bdc0fb20b32bc59c82176738857753070c0968`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. Restoring
    raw SUSPEND_N after reset does not move the HS-PHY/RX/SOF boundary; no
    flash or configfs operation was used.

84. Run `3683212.0` corrected and exercised the source-audit USB2
    VBUS/session boundary `--usb2-source-vbus-only`. The direct handoff now
    matches qpr1's `dwc3_override_vbus_status(true)` on the HS side: it keeps
    `UTMI_OTG_VBUS_VALID` and explicitly clears inherited `SW_SESSVLD_SEL`,
    which the old OR-only helper could not do. Build, QEMU, image-audit, and
    RAM-only boot passed; the manifest and emitted build command confirm the
    flag reached the kernel. Integrated Tshark retained 4,978 frames /
    2,437,322 bytes for 86.9 s (pcap SHA-256
    `93abfdac8fbe50cd013c53c5d3f3022ec9107fbc3fcabee5401153c5bf5b51ef`),
    and raw usbmon retained 4,978 records / 383,402 bytes (SHA-256
    `1d3239aae5273a12e506a2a36473464db73108a6c5ca3693309f5f18e4e293f4`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    effective VBUS/session correction is therefore rejected as the fix, and
    the remaining boundary is still USB2 HS-PHY/RX/SOF or DWC3 event ingress;
    no flash or configfs operation was used.

85. Run `3696298.0` corrected the qpr1 `dwc3_otg_start_peripheral()`
    ownership order in the direct USB2 path: VBUS/session publication now
    precedes the optional DBM reset, and DEVICE/sleep-mode setup follows it;
    the source-confirmed DWC31 LFPS exit-response timer is also applied for
    the USB2 source-peripheral-start profile. Build, QEMU, image-audit, and
    RAM-only boot passed, with the source-VBUS-only cfg still active.
    Integrated Tshark retained 4,726 frames / 2,420,938 bytes for 77.2 s
    (pcap SHA-256
    `1c8c1d8ecc7fc6b3cc87fd358b5cc9e815c8c1e4ffacd8ca867127da80f7096d`),
    and raw usbmon retained 4,726 records / 371,050 bytes (SHA-256
    `f169cfeaa0323b13fbe2f21b4eab828426b67a81cdc699c1a07583a04cfc8dcb`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. This
    qpr1 controller-ownership correction does not move the HS-PHY/RX/SOF
    boundary; no flash or configfs operation was used.
86. Run `3709093.0` exercised the remaining qpr1 gadget-start resource-order
    difference with `--android-resource-order`: after `DEPSTARTCFG`, Fullerene
    allocated transfer resources for the advertised hardware endpoints before
    either EP0 `SETEPCONFIG`, matching Android 4.19's
    `dwc3_gadget_start_config()`. QEMU, image-audit, and RAM-only boot passed;
    the manifest confirms the condition reached the kernel. Integrated Tshark
    retained 4,972 frames / 2,436,930 bytes for 85.2 s (pcap SHA-256
    `a82b1ac9286d6e4c720555374a6995ab10ea04d9da26303be7166ca9336de977`), and
    raw usbmon retained 4,972 records / 383,106 bytes (SHA-256
    `34cadc6760137da64ae4dbcaabd229a8adcc77dc69eb94cbe53d95d8427d7506`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. Resource
    preallocation order is rejected as the fix; the boundary remains USB2
    HS-PHY/RX/SOF or DWC3 event ingress, with no flash or configfs operation.
87. Run `3726217.0` applied the qpr1 `msm_hsphy_enable_power()` RPMh request
    order on the source-exact HS-PHY refresh: `vdd` voltage then enable, and
    `vdda18`/`vdda33` HPM then voltage then enable, each as separate active
    requests, with no `vdd` mode request. QEMU, image-audit, and RAM-only
    boot passed. Integrated Tshark retained 5,046 frames / 2,437,642 bytes
    for 86.6 s (pcap SHA-256
    `89e5c686e95e3d860089485abc26e4af198616e8315a6b048f6c2d5a7fd0821b`),
    and raw usbmon retained 5,055 records / 383,066 bytes (SHA-256
    `6abfe7697f2c9fde8e9d8356699abe4f93f3acddfe116540fc206f536a73cfc6`).
    The host still reached HS attach and the same zero-payload descriptor
    boundary (`-110`/`-71`; the first parsed completion was `-2` before the
    timeout classification); no `1234:0001` appeared. Android/Fastboot
    recovery completed automatically and final state was fastboot-available.
    The qpr1 RPMh rail-request ordering is rejected as the fix; the remaining
    source-directed candidate is the initial Qualcomm resume clock ordering at
    the USB2 HS-PHY/DWC3 ownership boundary. No flash or configfs operation
    was used.
88. Run `3751595.0` added and exercised `--usb2-source-resume-clocks`, a
    source-directed replay of qpr1's initial `dwc3_msm_resume()` ownership
    prefix: TCXO/GDSC reassertion followed by the Bramble DT's available
    sleep/interface/core/UTMI/bus branch order, before the direct DWC3 device
    reset. The flag reached the image through the bramble-usb harness, flasks
    CLI, build environment, and kernel cfg; QEMU, image-audit, and RAM-only
    boot passed. Integrated Tshark retained 5,494 frames / 2,470,842 bytes for
    97.8 s (pcap SHA-256
    `813e20210d9b996ba8567369d821fac4e314aa39da9e073127a2e2625854996d`), and
    raw usbmon retained 5,568 records / 413,233 bytes (SHA-256
    `b16ba53039b7a3a222e45b5ac39021533aef862223e965b4986ad8341a43f1e4`).
    The host still reached HS attach, then the same zero-payload address-0
    descriptor timeout `-110` followed by three immediate `-71` retries; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically and
    final state was fastboot-available. Initial resume clock ordering is
    rejected as the fix; the remaining boundary is still USB2 HS-PHY/RX/SOF or
    DWC3 event ingress. No flash or configfs operation was used.
89. Run `3789785.0` was a control capture after the qpr1 resume-clock source
    work was staged. Its build/image and integrated Tshark/usbmon capture are
    valid evidence for the unchanged host boundary (4,734 frames /
    2,418,289 bytes for 77.8 s; pcap SHA-256
    `220c2d59c81056596ed7357b1180d019096f7a4aaa9ebdbcae51b7a2c3fc865a`,
    raw usbmon 4,743 records / 368,705 bytes, SHA-256
    `35f700f9a409263f634715f0bab9079b2845e7276681b60c1726f4c05c721231`).
    The recorded manifest explicitly has `usb2_source_resume_clocks=false`,
    so the newly added GCC core CBCR `RETAIN_MEM`/`RETAIN_PERIPH` writes were
    not executed; this run must not be used to reject that candidate. The
    host reached HS attach, then one zero-payload address-0 descriptor
    timeout `-110` followed by three immediate `-71` retries, with no
    `1234:0001`; Android/Fastboot recovery completed automatically and final
    state was fastboot-available. No flash or configfs operation was used.
90. Run `3804644.0` was the valid qpr1 core-clock retention A/B using
    `--clock-branches-rearm`: after enabling the GCC core branch, Fullerene
    set CBCR `RETAIN_MEM` (bit 14) and `RETAIN_PERIPH` (bit 13), matching
    qpr1's `clk_set_flags(core_clk, ...)`. Build, QEMU, image-audit, and
    RAM-only boot passed. Integrated Tshark retained 5,068 frames /
    2,439,985 bytes for 87.3 s (pcap SHA-256
    `19b43709f394b79c32fa5937cc036403080d86a0160ef7cf9ef3ceda762db24e`),
    and raw usbmon retained 5,077 records / 385,057 bytes (SHA-256
    `ee332f22ad28d4d77abd971a9b84d0b13fe0fce8c021bc6e6b18c92c80aa857b`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. Core
    memory/peripheral retention is rejected as the fix; no flash or configfs
    operation was used.
91. Run `3809350.0` exercised the corrected `--usb2-source-resume-clocks`
    path after moving the qpr1 interconnect/bus vote ahead of TCXO, GDSC, and
    controller clock re-enable. Build, QEMU, image-audit, and RAM-only boot
    passed. Integrated Tshark retained 4,718 frames / 2,421,345 bytes for
    77.6 s (pcap SHA-256
    `65dd78b66d3b6ff156363c20b037425588d12b6211cdb3d9e27d1dbfc28557ae`),
    and raw usbmon retained 4,743 records / 372,801 bytes (SHA-256
    `e5dccb34967f5bee19efe9c5b39e17427d8d2b984d2791401d60c5cd887e1cf9`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. Resume
    bus-vote ordering is rejected as the fix; the remaining boundary is still
    USB2 HS-PHY/RX/SOF or DWC3 event ingress. No flash or configfs operation
    was used.
92. Run `3822622.0` added the source-backed qpr1 power-collapse core-reset
    boundary via `--usb2-source-resume-core-reset`: after the bus vote,
    HS-PHY reference/GDSC re-enable, it asserted/deasserted GCC `core_reset`
    for 1 ms before enabling the sleep/interface/core/UTMI/bus branches. The
    manifest confirms that the cfg reached the image. QEMU, image-audit, and
    RAM-only boot passed. Integrated Tshark retained 5,195 frames /
    2,452,351 bytes for 90.8 s (pcap SHA-256
    `d2e85203068edb65de247627d381d6dfa03cebbd8b0cb22b4ff23c38fec22e75`),
    and raw usbmon retained 5,224 records / 396,375 bytes (SHA-256
    `d4af2d1d604001629aedb2dc96228b13ac7f252bbb07ace5f41a745311d92211`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload `-71`
    retries; no `1234:0001` appeared. Android/Fastboot recovery completed
    automatically and final state was fastboot-available. The qpr1
    resume-positioned core reset is rejected as the fix; no flash or configfs
    operation was used.

93. Run `3842344.0` kept the qpr1 source-VBUS-only behavior effective after
    the DWC3 reset, avoiding the common post-reset QSCRATCH reassert that
    restored `SW_SESSVLD_SEL` bit 28. QEMU, image-audit, and RAM-only boot
    passed. Tshark retained 5,103 frames / 2,446,361 bytes for 87.3 s (pcap
    SHA-256
    `e40fe8a13ab617126e105ec6836d103f54b05b637adeaf509e4d7e84b4faf005`),
    and raw usbmon retained 5,103 records / 390,441 bytes (SHA-256
    `42b32b861c1b7091804d85826ba088f936249229a2a6ac935c0701389486ca24`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate `-71` retries; no
    `1234:0001` appeared. Android/Fastboot recovery completed automatically
    and final state was fastboot-available. The post-reset bit-28 correction
    is rejected as the fix; no flash or configfs operation was used.
94. Run `3852064.0` removed the deferred `--start-after-connect` and extended
    EP0-arm controls to test the qpr1 pre-Run/Stop ordering. The image never
    reached Fullerene HS attach and returned through Android/Fastboot
    automatically. Tshark retained 5,431 frames / 2,467,654 bytes for 86.8 s
    (pcap SHA-256
    `9ec8375a58a760e38d71dc5b0f48e1921e62377277e68d64508a52fa33d54ca1`),
    and raw usbmon retained 5,431 records / 406,486 bytes (SHA-256
    `e5bab2871994a1f18e47441a53a35bb85c743a22b93dc4a7c1cf0a2cd9971a`).
    Pre-Run/Stop EP0 arm is rejected; deferred setup remains required to
    preserve the attach-reaching boundary.
95. Run `3863985.0` removed `--no-smmu` while retaining the attach-reaching
    source-directed profile, enabling the DT Apps-SMMU stream `0xe0` mapping
    through `configure_dwc3_smmu()`. QEMU, image-audit, and RAM-only boot
    passed. Tshark/usbmon retained 5,060 parsed records / 387,402 bytes for
    the same USB2 boundary (pcap SHA-256
    `76e1309b63ec258aacef2ec270ac387029202a8574042f80ef3de95899ec2dcf`),
    and raw usbmon SHA-256
    `d2df788be48f1e9014c928108c03a38a2b9f2916c334d51f0db95eac80c9ddca`.
    The host still saw one zero-payload address-0 descriptor timeout `-110`
    and three immediate zero-payload `-71` retries, with no `1234:0001`.
    Android/Fastboot recovery completed automatically and final state was
    fastboot-available. SMMU enablement is rejected as the fix; no flash or
    configfs operation was used.
96. Run `3871713.0` added `--u2-freeclk-clear`, explicitly clearing only
    `GUSB2PHYCFG.U2_FREECLK_EXISTS` while preserving the qpr1 PHYIF/TRDTIM
    state. QEMU, image-audit, and RAM-only boot passed. Tshark/usbmon
    retained 5,103 parsed records / 390,441 bytes (pcap SHA-256
    `458a06d60f38f82f40eebfc2491ddafc0167d1cfb11a8897bb3eed71a30b6ce3`),
    and raw usbmon SHA-256
    `8c45f0c6d3337d09872f89363199567de58e5ad4593743839fbd88bd6f2dd8d4`.
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    free-clock clear is rejected as the fix; no flash or configfs operation
    was used.
97. Run `3886981.0` added `--hsphy-clear-power-down`, clearing only the
    Qualcomm HS-PHY `PWRDOWN_CTRL.PWRDOWN_B` bit after the final analog init;
    EUD-owned state was left untouched. QEMU, image-audit, and RAM-only boot
    passed. Tshark/usbmon retained 5,094 frames / 2,444,842 bytes for 87.5 s
    (pcap SHA-256
    `9d3d740b39afe040d19bde4ce6f5fb3565c00ebb44e6a24da0c9911d4665bd09`),
    and raw usbmon retained 5,142 parsed records / 392,345 bytes (SHA-256
    `9359e1a4774a823cdc663279fc24afa6fd6b823f63b6c22ba7f7691b728df892`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    PWRDOWN_B clear is rejected as the fix; no flash or configfs operation
    was used.
98. Run `3895515.0` added `--hsphy-restore-suspend-n-selected-after-runstop`,
    replaying qpr1's selected `SUSPEND_N_SEL|SUSPEND_N` assertion and selector
    clear immediately after Run/Stop. QEMU, image-audit, and RAM-only boot
    passed. Tshark/usbmon retained 5,131 frames / 2,447,266 bytes for 87.9 s
    (pcap SHA-256
    `6ae9362f90defa0577c2b4603205547f20a1fe2332ea29e5234ac51525595901`),
    and raw usbmon retained 5,177 parsed records / 394,065 bytes (SHA-256
    `66d1d938549a153e563d9af2f944f99d6e9a7b90ae3940933e87dbbe031c3832`).
    The host still reached HS attach, then one zero-payload address-0
    descriptor timeout `-110` followed by three immediate zero-payload
    `-71` retries; no `1234:0001` appeared. Android/Fastboot recovery
    completed automatically and final state was fastboot-available. The
    selected SUSPEND_N sequence is rejected as the fix; no flash or configfs
    operation was used.
99. Run `3900373.0` forced `DCFG_FULLSPEED` as a diagnostic while leaving the
    EP0/TRB and PHY controls unchanged. QEMU, image-audit, and RAM-only boot
    passed. The host changed only the observed attach to full-speed; it still
    received no Device Descriptor payload. usbmon retained 5,078 parsed
    records / 388,302 bytes (SHA-256
    `659c368e3083b02c03853abd390780ac1968f41fdd81ec265cdecec6743e4f8f`),
    with one zero-payload address-0 descriptor completion `-2` and two
    immediate zero-payload `-71` retries and no USB2 response frames. Tshark
    retained 5,078 frames / 2,443,822 bytes for 87.2 s (pcap SHA-256
    `e315612fed51fc7bfb0cdc6b3d79bfc5f7c861e01c7c7de7e73de5a18d79ed1f`).
    Android/Fastboot recovery completed automatically and final state was
    fastboot-available. Full-speed forcing is rejected as the fix; the
    boundary remains USB2 RX/SOF or DWC3 event ingress, with no flash or
    configfs operation used.
100. Run `3917715.0` added the source-backed `--keep-connect-on-start` A/B:
     on DWC_usb31 it restores `DCTL.KEEP_CONNECT` only when `GHWPARAMS1`
     advertises hibernation, matching qpr1 `dwc3_gadget_run_stop(true)`. QEMU,
     image-audit, and RAM-only boot passed. Tshark/usbmon retained 5,208
     frames / 2,453,193 bytes for 89.1 s (pcap SHA-256
     `d93bbc11695db3e2256c09cc6e48da9cdf77db77b6da16b109bfb070ca1bfb98`),
     and raw usbmon retained 5,217 parsed records / 396,025 bytes (SHA-256
     `bb6d8856b01622083f1a6402d26719e9cf2bd780dc27e43114f6fa615dcaec0f`).
     HS attach and the zero-payload descriptor `-110`/`-71` boundary were
     unchanged; no `1234:0001` appeared. Android/Fastboot recovery completed
     automatically and final state was fastboot-available.
101. Run `3925367.0` added the read-only
     `--utmi-preconnect-readout dwc3-hib` discriminator to the same
     KEEP_CONNECT A/B. The normal and readout runs both reached USB2 attach
     41 s after `fastboot boot`, so the readout returned the zero-delay,
     non-hibernation bucket: this hardware does not advertise the qpr1
     KEEP_CONNECT capability branch. Tshark/usbmon retained 5,068 frames /
     2,444,081 bytes for 87.7 s (pcap SHA-256
     `cdbe856629074af70015310fb76e127590c8886a328b9653a7c9e951b5fe1038`),
     and raw usbmon retained 5,092 parsed records / 389,897 bytes (SHA-256
     `47f4a28ba85560edefc4f3dbfb632f5a6888d53355f4f774ab11bc3b1f6b4b0c`).
     KEEP_CONNECT is rejected as the fix; the remaining failure is still
     before any USB2 response/SOF evidence and no flash or configfs operation
     was used.
102. Run `3937458.0` repeated the attach-reaching source-directed USB2 profile
     with `--irq-route typec-role`: the PMIC parent summary was routed, but the
     Type-C child IRQ latch was not programmed. Tshark retained 5,428 frames /
     2,467,481 bytes for 96.6 s (pcap SHA-256
     `1c16dcb9006615c95931949e43719c0e1535bfd96a3bd39347f6e5c510333043`),
     and raw usbmon retained 5,452 parsed records / 407,537 bytes (SHA-256
     `cb310fb7db71b09c06c2019df6d19ff63d523c00781083c138d57cf1f4e69133`).
     The host still reached USB2 HS attach followed by one zero-payload
     descriptor `-110` timeout and three zero-payload `-71` retries; no
     `1234:0001` appeared. Automatic Android/Fastboot recovery completed;
     Type-C child IRQ setup is rejected as the fix, and no flash or configfs
     operation was used.
103. A source audit found that Fullerene's GICv3 path consumes the INTID
     returned by `ICC_IAR1_EL1`, while the Bramble DT contract was installing
     raw `GIC_SPI` numbers. Android reports the known-good DWC3 line as 272
     for DT SPI 240 and `pwr_event_irq` as 176 for DT SPI 144. Fullerene now
     converts those DT SPI cells (and the SPMI parent SPI 481) to INTIDs
     272/176/513 at the DT boundary and updates the no-DTB fallbacks. Run
     `3955956.0` then reran the same attach-reaching profile with
     `--irq-route controller`, using corrected DWC3 INTID 272. QEMU, image
     audit, and RAM-only boot passed; Tshark retained 5,046 frames /
     2,437,642 bytes for 86.8 s (pcap SHA-256
     `535f779eb15e254af44f6657f0786b91ab5f16a053b6a91381400853f9770bc1`),
     and raw usbmon retained 5,046 parsed records / 382,634 bytes (SHA-256
     `718203e0d85b17983d9df877294ca13f0fbd81164272a234c2569d10ca474f0f`).
     The host still reached USB2 HS attach followed by one zero-payload
     descriptor `-110` timeout and three zero-payload `-71` retries; no
     `1234:0001` appeared. Automatic Android/Fastboot recovery completed;
     corrected controller IRQ ownership is not the boundary fix. No flash or
     configfs operation was used.
104. Run `3960238.0` repeated the corrected DT-SPI-to-GIC-INTID boundary with
     `--irq-route typec-role`: Type-C parent INTID 513 was enabled without
     programming the child latch. Tshark retained 4,726 frames / 2,420,938
     bytes for 77.5 s (pcap SHA-256 is recorded in the run directory), and
     raw usbmon retained 4,780 parsed records / 374,003 bytes (SHA-256
     `41ee47430f7adb18d9966a6f98c6a94385156252ad7a00d43bbcedb1258f02ee`).
     The host still reached USB2 HS attach followed by one zero-payload
     descriptor `-110` timeout and three zero-payload `-71` retries; no
     `1234:0001` appeared. Automatic recovery completed to Android ADB.
     Correct Type-C parent routing is not the boundary fix; the remaining
     target is USB2 HS-PHY RX/SOF or an upstream secure/external owner. No
     flash or configfs operation was used.
105. Run `3965895.0` repeated the corrected DT-SPI-to-GIC-INTID boundary with
     the normal `--irq-route typec` path, including the Type-C child latch.
     Tshark retained 5,063 frames / 2,443,761 bytes for 86.8 s (pcap SHA-256
     `cca80632e5adb8f4ce56290b4757e2d52b683e855e6aa8daab54329f497b907e`),
     and raw usbmon retained 5,063 parsed records / 388,481 bytes (SHA-256
     `3950dcc856c6e36391f7b1bcf6727e66f8235c6529cf551100f9e98a85ab4e7d`).
     The host still reached USB2 HS attach followed by one zero-payload
     descriptor `-2` completion and three zero-payload `-71` retries; no
     `1234:0001` appeared. Automatic recovery completed to Fastboot.
     Correct normal Type-C routing is not the boundary fix; the remaining
     target is USB2 HS-PHY RX/SOF or an upstream secure/external owner. No
     flash or configfs operation was used.
106. Android's Lito DT and qcom PDC hierarchy identify PDC USB pins 14/9/15
     as children whose parent hwirqs are DT GIC SPI values 494/489/495.
     Fullerene's early GIC path consumes INTIDs, so the PDC range parent bases
     and the PDC-route constants were corrected to 526/521/527 before hardware
     testing. Run `3976158.0` exercised the corrected `--irq-route pdc` path
     with the same attach-reaching profile. Tshark retained 5,503 frames /
     2,472,401 bytes for 97.8 s (pcap SHA-256
     `8772aae4831ba0f6a85f21fa5924228994d7dbf530c7b698c84d2dccf97a3451`),
     and raw usbmon retained 5,518 parsed records / 410,825 bytes (SHA-256
     `89b67e04c7fd60ac0a5918534561a7ada72edaaa27c5d89dc45b9346c6309acd`).
     The host still reached USB2 HS attach followed by one zero-payload
     descriptor `-2` completion and three zero-payload `-71` retries; no
     `1234:0001` appeared. Automatic recovery completed to Fastboot.
     Corrected PDC routing is not the boundary fix; the remaining target is
     USB2 HS-PHY RX/SOF or an upstream secure/external owner. No flash or
     configfs operation was used.

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
