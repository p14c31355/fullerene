# Context-efficient project status

Read this file first when investigating the current hardware goal. It is a
compact index of the evidence; the linked hardware ledgers retain the full
commands, timestamps, hashes, and per-run notes.

## Current goal

| Goal | Success criterion | Current state |
| --- | --- | --- |
| Pixel 4a 5G (Bramble) USB handoff | Fullerene enumerates as `idVendor=1234`, `idProduct=0001` | Reached: host sees `1234:0001` |
| FullereneOS AArch64 port | Boot the real FullereneOS runtime on Bramble | Early bring-up only: generic runtime not yet entered |
| Recovery safety | Failed handoff returns to Android without flashing or erasing | Confirmed with `fastboot boot` runs |

## Observer-isolation follow-up

Observer-isolation follow-up is now implemented: free-space gates clear the
legacy U0-arm blip unconditionally, entry/stage gates do not continuously poll
GDBGFIFOSPACE/GDBGLSPMUX after their stage latch, and descriptor-window gates
poll only the selected queue. Window results map to `1 same`, `2..4 changed`,
`6 unavailable`, then publish 1/2/3 Run/Stop pairs respectively.

ADB→Fastboot observer-isolation runs after these changes were:
`1068508.0` clean baseline (Android after 38 s), `1070466.0` signal-probe only
(Android after 42 s), and `1072643.0` descriptor-window RXINFOQ (Android after
51 s). Current HEAD bare-pullup control `1085078.0` also returned to Android
after 38 s without a host-visible Fullerene attach. All accepted `fastboot boot`,
but none produced a Fullerene HS attach or Run/Stop pair count; no
SPACE_AVAILABLE category is claimed. Artifact SHA files are retained in each
run directory.

## Corrected SPACE_AVAILABLE transport

The Linux-equivalent `GDBGFIFOSPACE` observer now treats bits 31:16 as
`SPACE_AVAILABLE` free space. After latching the read-only observation, the
probe emits category `1=0`, `2=1`, `3=2..3`, `4=4..7`, `5=>=8`, or
`6=invalid/unavailable` using DWC3 `DCTL.RUN_STOP` stop/run pairs. One completed
pair is one host disconnect/re-attach pair. Zero pairs means the core/link
transport precondition failed, not free-space category zero. Descriptor-window
selectors compare the post-Run/Stop baseline with polling min/max. The code, workspace/tests, and Bramble probe build are verified. Physical run
`630881.0` was attempted after ADB→Fastboot and `fastboot boot`; Android
`18d1:4ee7` returned, but no Fullerene HS attach or Run/Stop pair count was
captured, so no category is claimed. Artifact SHA:
`b8f5573b4df681322fd6e15a8ad3fa8d8f790a3a057d20f338efcadbd2df6b5a`.

## What is actually known


| Boundary | Evidence | Interpretation |
| --- | --- | --- |
| Fastboot handoff | The device leaves Fastboot and Fullerene reaches USB High-Speed attach | Pull-up/attach works; this is not enumeration success |
| First control request | Host submits `GET_DESCRIPTOR(Device)` | The host sees a Fullerene USB device address path |
| Response | usbmon records no returned bytes (`len=0`, `cap=0`) | No descriptor payload exists to wrap or re-encode |
| Failure | A valid capture shows `-2`, followed by zero-length `-71` retries | The error is before a usable EP0 response; Linux `-EPROTO` is a broad host error class |
| Software latches | Corrected `SPACE_AVAILABLE` sampler and Run/Stop pair transport are implemented; no physical pair count was obtained because the handset was unavailable in Fastboot | DWC3 event production/consumption and USB2 RX/UTMI remain unresolved |
| Recovery | Android returns as `18d1:4ee7` | The experiment is non-destructive, but Fullerene still does not enumerate |

## Current fixed baseline

Keep these values fixed while isolating one new boundary at a time:

| Area | Baseline |
| --- | --- |
| Clock | Official Bramble Lito mock-UTMI source at 19.2 MHz from `BI_TCXO`; the 60 MHz path is only an explicit invalid-source negative control |
| UTMI | 8-bit `PHYIF`; `USBTRDTIM=9` |
| Event ring | 4096-byte Android-sized control event buffer |
| EP0 | Linux/Android-style setup TRB and `STARTTRANSFER` with `DEPCMD_PARAM=0` |
| Handoff | Direct USB2 reuse path, no SMMU, attach-reaching `STARTTRANSFER`, Android resource order |
| Safety | `fastboot boot` only; no flash, erase, or persistent modification |

## Most useful negative results

These rows summarize families of experiments; use the full ledger for every
run ID and artifact hash.

| Family | Result | What it rules out for now |
| --- | --- | --- |
| Clock source/rate and settle delay | The earlier 60 MHz runs, branch rearm, and 20 ms settle did not produce data; the official source audit fixes the mock-UTMI baseline at 19.2 MHz | The earlier 60 MHz runs are invalid-source negative controls; the corrected 19.2 MHz baseline is now a reproduced negative result |
| Official 19.2 MHz baseline | Run `2157665.0` used the exact ABL EP-config/command-mask/TRB/event-consume profile with the corrected 19.2 MHz mock-UTMI source | The official clock correction did not move the zero-data boundary; it is now a valid negative baseline, not a clock-source ambiguity |
| UTMI width/timing | 16-bit variants and matched/mismatched `USBTRDTIM` still failed before payload | Descriptor formatting is not the first failure; PHY contract is still a hypothesis, not proof |
| HS-PHY reset/rails/tuning | Android reset timing, rail refresh, ref-clock vote, and termination tune did not move the boundary | The tested power/reset ordering alone is not sufficient |
| DWC3 policy | `SUSPHY`, `ENBLSLPM`, retry policy, LPM policy, and core-clock variants did not produce data | These individual policy bits are not the immediate fix |
| DMA/event publication | XBL addresses, Fastboot event DMA reuse, 4096-byte ring, and Run/Stop republish did not expose a response | No single tested ring-address/publication variant is sufficient |
| EP0 ordering | Connect Done, USB Reset, eager/ungated arm, stall flush, and Android restart variants did not reach `1234` | The remaining issue is still at or below EP0 ownership/event/link handling |
| Protocol classifier | APSS-WDT recovery landed in the common `boot-reason=watchdog` bucket | No xHCI completion subcode or DWC3 protocol code was obtained |
| ABL command-parameter mask | ABL-style selective `DEPCMDPAR` publication still ended at host `-110` and Android recovery | This A/B is negative for enumeration; the later exact EP-config run supplies the parallel `-2`→`-71`/zero-payload classification |
| ABL/Qualcomm EP config | Exact disassembly/msm `SETEPCONFIG` fields plus the ABL command-parameter mask still produced no payload and no `1234` | The EP0 `P0/P1` mismatch is now ruled out as the immediate fix; the `-71` path remains a pre-payload ownership/transport boundary |
| ABL request/TRB flags | ABL's `0x405` request control base (`HWO\|CHN\|ISP_IMI`) also produced `-2`→`-71` with `len=0`/`cap=0` | The `LST\|IOC` versus ABL flag difference alone is not sufficient; resolve request-object/ring ownership before another flag permutation |
| ABL event ownership order | ABL-style per-event dispatch followed by four-byte `GEVNTCOUNT` ACK with EHB preserved also produced `-2`→`-71` with `len=0`/`cap=0` | Event ACK timing/order alone is not sufficient; compare the physical request object and EP0 TRB/ring ownership state |
| EP0 setup DMA/readback semantics | Android msm `ep0.c` targets `CONTROL_SETUP` at `dwc->ep0_trb_addr` and reads the received request from `dwc->ep0_trb`; Fullerene now uses the same EP0 TRB for DMA and parsing | The coherent source-aligned run `2201961.0` still returned no payload, so the remaining suspect is request/ring ownership, DWC3 event delivery, or lower USB2/UTMI reception |
| MMIO write ordering barrier | `write()` in `mmio.rs` lacked the DSB store-barrier that Linux's `writel()` provides via `__iowmb()` | Fix applied: `dsb st` after every `write_volatile` in `write()`. Real-hardware verification (runs 2263604.0, 2266529.0, 2278047.0, 2284470.0, 2287872.0) showed NO behavioral change: HS attach still succeeds, descriptor read still times out with -110. The DSB fix is correct (matches Linux) but was not the root cause. The DALEPENA=0 readback may be a timing-channel artifact rather than a real register state. |
| HS-PHY/QSCRATCH write barrier A/B | Opt-in `dsb st` after HS-PHY and QSCRATCH writes on the current source-exact ADB→Fastboot USB2 path (`834988.0`) | Negative: HS attach at `18:04:07`, Device Descriptor `-110` at `18:04:12`, then Android `18d1:4ee7` at `18:04:33`; usbmon had `GET_DESCRIPTOR` followed by `-2`/`-71` with `len=0`/`cap=0`. Dump SHA `21df0bb7258053e7859bc996b5556cb99bfaad17170f88f8d6c965972c15fff7` |
| `U2_FREECLK_EXISTS` A/B | Set the source-default `GUSB2PHYCFG.U2_FREECLK_EXISTS` bit on the same USB2 path (`843743.0`) | Negative: HS attach at `18:10:50`, Device Descriptor `-110` at `18:10:55`, then Android `18d1:4ee7` at `18:11:16`; usbmon again had no response bytes. Dump SHA `f1548f554d7d465ba24380aefbe151b605ffabbf2b0af0e7b252d15ad4bf5ecf` |
| SuperSpeed reset differential | User report: 35 tests total; full `--super-speed` crashed in all 3, while the reported `--super-speed --no-core-reset` run `2511131.0` also watchdog-crashed | Source audit of the run's build log shows the SS path still passed `reset_core=true`; this is not a valid CSFTRST exclusion. Fixed source now propagates the preserve-core flag, and the QMP path has retained per-stage markers |
| Corrected SuperSpeed no-core run | Run `2583149.0` used the fixed `reset_core=false` propagation and still recovered by watchdog with no Fullerene attach or `1234:0001` | This is the first valid CSFTRST A/B. The host loop cannot read the retained QMP markers; source comparison then identified missing QMP-local regulator/clock reassertion and three Android-equivalent `dmb sy` boundaries |
| QMP ownership/ordering A/B | Run `2600037.0` added QMP-local power, ref clock, `com_aux/aux/pipe` branches, and the three barriers; it still watchdog-recovered with no Fullerene attach | Negative. The QMP marker channel remains unavailable from the host loop, so the exact pre/post-`QMOK` boundary is still unresolved |
| QMP init-order A/B | Run `2605080.0` added the official second common/PCS power-up and AUX divider `1`; it reached Fullerene HS attach but the descriptor read timed out with `-110`, then Android recovered | Partial positive boundary movement. The QMP marker channel remains unavailable from the host loop, so this does not prove `QMOK` or SS-link training; the host-visible result is still not `1234:0001` |
| QMP combo-reset A/B | Run `2608997.0` added the official combo-PHY reset before QMP init; it again reached Fullerene HS attach, then timed out with `-110` and fell back to Android | Partial positive, but SS still failed. The reset moved the run into the known HS attach boundary; no descriptor payload, SS attach, or `1234:0001` was observed, and the exact QMP failure phase is still unknown |
| Previous-boot QMP gate | Run `2625860.0` used `--usb-signal-prev-qmp-gate 1` after `2608997.0`; no Fullerene attach occurred and Android recovered | Inconclusive retained-trace readout. This is consistent with the previous QMP trace being scribbled before the next boot or with no valid `QMPB` marker, so the host loop still cannot identify the QMP phase |
| QMP-before-DWC3 reset ordering | Run `2642655.0` moved QMP reset/init before the optional DWC3 CSFTRST while keeping `--no-core-reset` | Negative ordering A/B. The result remained HS attach, `-110`, then Android recovery; this ordering is not sufficient for SS link training, and the exact QMP phase is still unknown |
| Pre-QMP USB3 SUSPHY | Run `2652242.0` explicitly set `GUSB3PIPECTL.SUSPHY=1` at the official pre-QMP boundary | Negative one-bit/source-order A/B. The result remained HS attach, `-110`, then Android recovery; the retained Fastboot value was not the immediate explanation, and the exact QMP phase is still unknown |
| QMP-complete stage 13 | Run `2660836.0` stopped immediately after QMP init and issued minimal SS Run/Stop with `--skip-typec-spmi` | Negative SS-link boundary, but QMP completion is not independently readable. The host saw HS only and no descriptor request, so the next control is the same stage with the actual Type-C orientation observation enabled |
| Type-C orientation observation | Run `2665116.0` repeated stage 13 without `--skip-typec-spmi` | Inconclusive/negative A/B. The observer-enabled run produced no SS device or `1234:0001`; instead xHCI setup timed out, address 51 failed with `-62`, and the port was power-cycled before Android SS recovery. This changes the failure shape but does not prove either lane selection or observer correctness |
| Explicit QMP lane override | Source audit added `--qmp-lane a\|b`; runs `2679376.0`/`2685319.0` tested lanes B/A with `--skip-typec-spmi`, and `2688865.0` repeated explicit lane A with Type-C observation enabled | Negative/inconclusive explicit-lane and observer A/B. Lane B produced xHCI address `-62`; lane A without observation reproduced HS attach only; lane A with observation returned to the ordinary HS/`-110` path. Neither produced an SS identity or `1234:0001`. The override changes only QMP `TYPEC_CTRL` (`0x02`/`0x03`) and leaves PMIC Type-C role/VBUS writes unchanged |
| Same-boot QMP phase readout | Source correction added `--qmp-phase-stop 1..8` and made stage 13 participate in the automatic recovery gate; implemented with cargo checks, CLI help, and `git diff --check` passing, and phases 1–8 physical runs complete | Each selected QMP marker (`QMPB` through `QMOK`) now stops before the next risky QMP access and falls back to the known USB2 pull-up. Runs `2706606.0`, `2710010.0`, `2712894.0`, `2715864.0`, `2718831.0`, `2722509.0`, `2725423.0`, and corrected `2739860.0` produced the expected same-boot HS attach for phases 1–8, proving the QMP entry/control preamble/table/table-complete/PCS-start/status-read/poll-entry/PHY-ready prefix survives; the fallback does not make partial QMP state or USB2 RX valid |
| Post-QMP SS link boundary | Stage 13 and corrected stage 14 now stop after QMP completion, with stage 14 extending through DWC3 global/PHY setup and minimal SS Run/Stop; negative: `2751078.0`/`2755382.0` are stage-13 controls, `2760948.0`/`2763971.0` predate the stage-14 SUSPHY-clear correction, and corrected `2787804.0` still produced no `1234:0001` | The corrected stage `>=13` path clears USB3 `GUSB3PIPECTL.SUSPHY` before SS Run/Stop, but the host still sees no SS identity and falls back to Android. The `ss-speed` readout timing was not independently decodable from saved host artifacts, so speed/link/PIPE state remains unclassified. SMMU, endpoint, event-ring, and EP0 are still downstream |
| Full SS post-stage-14 tail | `2817124.0` crashed on the unrestricted no-core path; stages 15–29 crossed the staged SS handoff and probe tails; corrected QMP branch runs `3228530.0`/`3233224.0`/`3236181.0` read all three branches as `1`; `3326735.0`/`3332679.0` read retained QMP common/PCS power as `1`; `3317093.0`/`3320525.0`/`3323625.0` read all QSCRATCH UTMI/PIPE bits as `0`; the old `3288638.0`–`3297768.0` LTSSM samples were pre-Run/Stop; `3380073.0` is invalidated by a DWC31 GSNPSID predicate bug; `3383134.0` measured live LTSSM bit0=`0`; `3395716.0` bit2=`0`; `3401041.0` bit3=`0`; `3408278.0` wide bit1=`0`; negative/inconclusive for enumeration, with HS fallback still ending at `-110`, never `1234:0001` | Stage-20 `LINKSTATE=SS_DIS` is reclassified as expected pre-start state because Qualcomm gadget start initializes `SS_DIS` before production `DCTL.Run/Stop`. The stage-21 bit timing channels are now separated, but their live-state interpretation must first be anchored by a corrected `ss-domain-core` read; packet wrapping is downstream |

| SS Run/Stop lifecycle readout | `3009898.0`/`3019380.0` read `ss-dctl=0`; later probes establish controller-domain/DEVICE state, QMP status/start/lane, all three QMP branches as `1`, retained common/PCS power as `1`, and all QSCRATCH UTMI/PIPE bits as `0`; `3380073.0` is invalidated because `ss-domain-core` rejected the Bramble DWC31 prefix; `3383134.0` adds post-Run/Stop `ss-ltssm-bit0=0`; `3395716.0` bit2=`0`; `3401041.0` bit3=`0`; `3408278.0` wide bit1=`0`; negative/inconclusive for enumeration with no `1234:0001` | The old LTSSM rows classify only the expected pre-Run/Stop state. Stage 21 is the live link-FSM boundary, but retake `ss-domain-core` with the corrected `0x3331` predicate before treating the candidate live nibble as authoritative |
| Full Android gadget-restart insertion | `3024664.0` enabled `--gadget-restart-at-runstop` with stage-21 `ss-dctl` requested; negative/inconclusive reachability with no `1234:0001` | No Fullerene HS marker and no stage-21 readout appeared before watchdog recovery; Android SS returned at `18:34:22`. Do not claim an `ss-dctl` value from this run. Inserting the full restart after the current EP0/resource setup introduces an earlier block, so the next A/B isolates only the DCTL write policy |
| Source-exact DCTL Run/Stop | `3037902.0` used `--source-exact-runstop` with stage-21 `ss-dctl` requested; negative A/B with no `1234:0001` | The expected `usb 1-9` HS marker did appear, then its descriptor timed out with `-110`; Android SS returned at `18:43:21`. The narrower DCTL policy did not improve the known boundary; no raw `ss-dctl` value is claimed |

### Latest incremental hardware result

`3264118.0` ran the read-only `ss-qmp-rxeq` selector at stage 20 with lane A, no core reset, no SMMU, and both corrected clock reassertions. Fastboot disconnected at `21:01:27`; the HS marker appeared at `21:01:38` (0-bit timing bucket); the descriptor timed out with `-110` at `21:01:43`; Android `18d1:4ee7` returned at `21:02:04`; watchdog recovery completed. This is a valid `PCS_STATUS2[3]=0` result, with no `1234:0001`; no INSIG or link-training mutation was applied. Artifact SHA: `5b1572b74d6b1b8ad8b2b0914769903c69bd21ccc2a1171b488e73a12230bf81`.

`3283449.0` then ran the USB31-correct raw `ss-ltssm` selector. Fastboot disconnected at `21:14:57`; the HS marker appeared at `21:15:08`; the descriptor timed out with `-110` at `21:15:13`; Android `18d1:4ee7` returned at `21:15:34`; no `1234:0001`. The stage-20 snapshot executed, but the raw 250-ms bucket is not numerically recoverable from whole-second host timestamps; the next readout is `ss-ltssm-bit0..3`.

`3288638.0` measured `ss-ltssm-bit0=0` with the same fixed profile: disconnect `21:18:05`, HS marker `21:18:15`, descriptor `-110` at `21:18:21`, Android `18d1:4ee7` at `21:18:41`, watchdog recovery, and no `1234:0001`. The remaining LTSSM bits are still pending.

`3291760.0` measured `ss-ltssm-bit1=0`: disconnect `21:19:42`, HS marker `21:19:52`, descriptor `-110` at `21:19:58`, Android `18d1:4ee7` at `21:20:18`, watchdog recovery, and no `1234:0001`. LTSSM bits 0 and 1 are clear; bits 2 and 3 remain pending.

`3294678.0` measured `ss-ltssm-bit2=1`: takeover disconnect `21:21:12`, HS marker `21:21:27` (the 4-second bucket), descriptor `-110` at `21:21:32`, Android `18d1:4ee7` at `21:21:53`, watchdog recovery, and no `1234:0001`. The pre-takeover Fastboot `18d1:4ee0` event is excluded; LTSSM bits are now `bit0=0, bit1=0, bit2=1`, with bit3 pending.

`3297768.0` measured `ss-ltssm-bit3=0` at the stage-20 pre-Run/Stop boundary: takeover disconnect `21:22:52`, HS marker `21:23:03`, descriptor `-110` at `21:23:08`, Android `18d1:4ee7` at `21:23:29`, watchdog recovery, and no `1234:0001`. Together with the other old bits it reconstructs pre-Run/Stop `LINKSTATE=4`, the expected initial `SS_DIS`; it is not evidence about the state after production `DCTL.Run/Stop`.

`3317093.0` measured the first read-only QSCRATCH selector, `ss-qscratch-utmi-sel=0`: Fastboot disconnected at `21:36:17`, the HS marker appeared at `21:36:27` (10 s baseline), the descriptor timed out with `-110` at `21:36:33`, and Android `18d1:4ee7` returned at `21:36:55`; watchdog recovery completed and no `1234:0001` appeared. `QSCRATCH_GENERAL_CFG[0]` was therefore clear at the stage-20 snapshot, so stale `PIPE_UTMI_CLK_SEL` is not the immediate `SS_DIS` cause. The remaining two QSCRATCH bits and the corrected retained QMP-power readout are pending.

`3320525.0` measured `ss-qscratch-phystatus-sw=0`: Fastboot disconnected at `21:38:11`, the HS marker appeared at `21:38:22` (11 s baseline with host jitter), the descriptor timed out with `-110` at `21:38:27`, and Android `18d1:4ee7` returned at `21:38:48`; watchdog recovery completed and no `1234:0001` appeared. `QSCRATCH_GENERAL_CFG[3]` was clear at stage 20, so the HS-only `PIPE3_PHYSTATUS_SW` override is also not the immediate `SS_DIS` cause. Only `PIPE_UTMI_CLK_DIS` remains in this read-only QSCRATCH group.
`3323625.0` measured `ss-qscratch-utmi-dis=0`: Fastboot disconnected at `21:39:53`, the HS marker appeared at `21:40:03` (10 s baseline), the descriptor timed out with `-110` at `21:40:09`, and Android `18d1:4ee7` returned at `21:40:29`; watchdog recovery completed and no `1234:0001` appeared. `QSCRATCH_GENERAL_CFG[8]` was clear at stage 20. All three read-only UTMI/PIPE mux bits are clear, excluding stale HS mux state as the immediate USB31 `SS_DIS` cause.
`3326735.0` measured the corrected retained-snapshot `ss-qmp-com-power` selector: Fastboot disconnected at `21:41:36`, the HS marker appeared at `21:41:50` (about 14 s, the 1-bit bucket), the descriptor timed out with `-110` at `21:41:56`, and Android `18d1:4ee7` returned at `21:42:17`; watchdog recovery completed and no `1234:0001` appeared. The stage-20 retained `QMP COM_POWER_DOWN_CONTROL` bit was `1`; `3332679.0` subsequently confirmed the PCS power bit as `1`, so both QMP power controls are on.
`3332679.0` measured the corrected retained-snapshot `ss-qmp-pcs-power` selector: Fastboot disconnected at `21:45:38`, the HS marker appeared at `21:45:53` (about 15 s, the 1-bit bucket), the descriptor timed out with `-110` at `21:45:58`, and Android `18d1:4ee7` returned at `21:46:19`; watchdog recovery completed and no `1234:0001` appeared. The retained PCS power bit was `1`, confirming both QMP common and PCS power are on. The old stage-20 LTSSM value is only the expected pre-Run/Stop state; stage-21 live LTSSM remains pending.

`3351473.0` ran the corrected stage-21 `ss-ltssm-bit0` selector. Fastboot disconnected at `21:59:08`, the HS marker appeared at `21:59:21` (13 s after disconnect), the Device Descriptor did not enumerate, Android `18d1:4ee7` returned at `21:59:45`, and `boot-reason.txt` is `watchdog`; no `1234:0001`. The 13-second interval falls between the calibrated 0-bit (`~10–11 s`) and 1-bit (`~14–15 s`) buckets, so bit 0 is not claimed; post-Run/Stop capture is confirmed and the result is inconclusive.

`3380073.0` ran the post-Run/Stop `ss-domain-core` selector with the corrected stage-21 timing. Fastboot disconnected at `22:16:54`, the HS marker appeared at `22:17:04` (10 s), Android `18d1:4ee7` returned at `22:17:30`, and watchdog recovery followed; no `1234:0001`. This run is now invalidated: the selector's GSNPSID predicate recognized only DWC3 `0x5532/0x5533` and rejected Bramble's DWC_usb31 `0x3331`, so its `ss-domain-core=0` was a false negative. Artifact SHA: `69a9efe05589f251f1ac55b1b7ba90466234f0d67570ece8c916cf94fc89ad18`.

`3422166.0` retook stage-21 `ss-domain-core` after fixing that predicate. Fastboot disconnected at `22:44:16`, the HS marker appeared at `22:44:26` (10 s, no extra-delay bucket), the descriptor timed out with `-110` at `22:44:31`, and Android `18d1:4ee7` returned at `22:44:52`; watchdog recovery followed and no `1234:0001` appeared. The DWC31-aware selector still measured `ss-domain-core=0`, so the post-Run/Stop GSNPSID response drop is now a valid finding; it does not yet distinguish clock/reset/secure-ownership loss. Artifact SHA: `37ed7d2d4a446074a22a2b3fab46709ed55a8f0dfd086897934e616ac9919161`.

`3383134.0` re-ran stage-21 `ss-ltssm-bit0` after the timing correction. Fastboot disconnected at `22:18:31`, the HS marker appeared at `22:18:42` (11 s), Android `18d1:4ee7` returned at `22:19:07`, and watchdog recovery followed; no `1234:0001`. The live post-Run/Stop `DWC31_LINK_GDBGLTSSM[22]` bit was `0`. Artifact SHA: `3b4c8b6f29a65aeea39e2f51b01e19b2d92fa748e615872eb66d8dc1aa3b55aa`.

`3386102.0` re-ran stage-21 `ss-ltssm-bit1`. Fastboot disconnected at `22:19:57`, the HS marker appeared at `22:20:09` (12 s), Android `18d1:4ee7` returned at `22:20:33`, and watchdog recovery followed; no `1234:0001`. The 12-second interval lies between the observed 0-bit and 1-bit timing buckets, so `DWC31_LINK_GDBGLTSSM[23]` is not claimed. Artifact SHA: `8b2a776ef8839ca191eef1295dfa9813d33f40db815e4aff3568ab08d7af6729`.

`3395716.0` re-ran stage-21 `ss-ltssm-bit2`. Fastboot disconnected at `22:26:51`, the HS marker appeared at `22:27:02` (11 s), Android `18d1:4ee7` returned at `22:27:28`, and watchdog recovery followed; no `1234:0001`. The live post-Run/Stop `DWC31_LINK_GDBGLTSSM[24]` bit was `0`. Artifact SHA: `b62ac74705ed04da788dc6788fb966a3fb87fd94a03d9fa581884dc43d137335`.

`3401041.0` re-ran stage-21 `ss-ltssm-bit3`. Fastboot disconnected at `22:29:59`, the HS marker appeared at `22:30:10` (11 s), Android `18d1:4ee7` returned at `22:30:36`, and watchdog recovery followed; no `1234:0001`. The live post-Run/Stop `DWC31_LINK_GDBGLTSSM[25]` bit was `0`. Artifact SHA: `36f9dc604cd90adc5154bcfc09051cdc4be46c8c222ff4a672d9d682c507d4e0`.

`3408278.0` ran the stage-21 `ss-ltssm-bit1-wide` discriminator. Fastboot disconnected at `22:34:33`, the HS marker appeared at `22:34:46` (13 s), Android `18d1:4ee7` returned at `22:35:09`, and watchdog recovery followed; no `1234:0001`. Because the wide selector adds 8 seconds only for a `1` bit, the 13-second no-extra-delay interval gives bit1=`0`. This separates the timing channel, but the live nibble remains provisional until the corrected `ss-domain-core` selector is retaken. Artifact SHA: `a266a668a0416bbf52306ac6f87b8b90f3010387513d2958d0ad8cc5fc16324a`.

`3440845.0` retook stage-21 `ss-domain-core` with the controller-only post-Run/Stop reassertion. Fastboot disconnected at `22:54:39`, the HS marker appeared at `22:54:50`, the descriptor timed out with `-110` at `22:54:55`, and Android `18d1:4ee7` returned at `22:55:15`; watchdog recovery followed and no `1234:0001` appeared. The core bit remained `0`, so the one-shot CX/bus/GDSC/RCG/branch reassertion did not restore the post-boundary domain.

`3449266.0` repeated the same stage-21 core readout with the full Android-style domain refresh, including CX/BCM votes and USB PHY regulator re-enables before GDSC/RCG/branch reassertion. Fastboot disconnected at `23:00:05`, the HS marker appeared at `23:00:15`, the descriptor timed out with `-110` at `23:00:21`, and Android `18d1:4ee7` returned at `23:00:42`; watchdog recovery followed and no `1234:0001` appeared. The DWC31-aware `ss-domain-core` bit still measured `0`. Artifact SHA: `af7c94379ae48f374f2c12f745a1e4bb6914d1ca957992ce587bff3024e9ca42`.

`3453087.0` repeated the stage-21 `ss-domain-gdsc` readout with that same full refresh. Fastboot disconnected at `23:02:11`, the HS marker appeared at `23:02:21`, the descriptor timed out with `-110` at `23:02:27`, and Android `18d1:4ee7` returned at `23:02:47`; watchdog recovery followed and no `1234:0001` appeared. The post-Run/Stop GDSC bit remained `0`. Artifact SHA: `2d3c2ccfa41d8dfed69f7e6b8bee72687e19aead97f5854583a5d02f1d65b9a6`.

`3455402.0` repeated the stage-21 `ss-domain-core-branch` readout with the full refresh. Fastboot disconnected at `23:03:15`, the HS marker appeared at `23:03:26`, the descriptor timed out with `-110` at `23:03:31`, and Android `18d1:4ee7` returned at `23:03:52`; watchdog recovery followed and no `1234:0001` appeared. The GCC USB30 core branch bit remained `0`. Artifact SHA: `f49d95a1395b46781637e64ada7b24dd5149ab62f6442f73733775df9df0afab`.

`3457638.0` repeated the stage-21 `ss-domain-utmi-branch` readout with the full refresh. Fastboot disconnected at `23:04:19`, the HS marker appeared at `23:04:29`, the descriptor timed out with `-110` at `23:04:35`, and Android `18d1:4ee7` returned at `23:04:55`; watchdog recovery followed and no `1234:0001` appeared. The GCC mock-UTMI branch bit remained `0`; artifact SHA: `dc8b8c8eaf36d16572e952c104f4b37be6235f18e8c9c6d686db3b854b544459`.

These four corrected post-Run/Stop domain bits are therefore all `0`, including after the full one-shot CX/BCM, regulator, GDSC, RCG, and branch refresh. That refresh does not restore the DWC31 response or USB30 operating domain. The next source-directed A/B is the Qualcomm glue link-clock reset/ownership boundary, not packet wrapping or EP0 formatting.

`3473325.0` replayed the Qualcomm glue link-clock stop/reset/release sequence immediately after production Run/Stop, while retaining `--no-core-reset`, QMP lane A, and the stage-21 `ss-domain-core` readout. Fastboot disconnected at `23:15:11`; HS attach appeared at `23:15:25`; the first descriptor read returned `-110` at `23:15:31`, followed by `-71` responses during xHCI retry/power-cycle. No Android SuperSpeed fallback appeared in the captured window, and the phone disappeared from USB afterward. The post-Run/Stop link-clock reset is therefore a negative/destabilizing A/B, not a domain recovery. Artifact SHA: `2aa76e81915298b8a3e026fbf7ff9194079bf24027ebe4716c2cab946370bc8`.

The next DBM A/B was source-checked against Qualcomm `dbm.c`: DBM v1.5 uses `wrapper_base + 0xf8000`, while QSCRATCH is the separate `wrapper_base + 0xf8800` window. The local helper was corrected to use `dwc3_base + 0xf8000` for DBM and `qscratch_base + 0x08` only for `DBM_EN`; the DBM-enabled AArch64 build passes. The physical run remains pending because the phone is currently absent from `fastboot`, `adb`, and USB bus 2.

`3507048.0` ran that corrected DBM path at stage 21. Fastboot disconnected at `23:33:32`, the intentional HS marker appeared at `23:33:42`, the descriptor timed out with `-110` at `23:33:48`, and Android `18d1:4ee7` returned at `23:34:09`; `boot-reason=watchdog`. This is only a stage-reach diagnostic because stage 21 deliberately falls back before SS endpoint publication. The next run must use the full SuperSpeed path; the post-Run/Stop GCC reset flag remains excluded. Artifact SHA: `5b8ba03592839d7722140fc044621eba8d0e35ec1294c25a8d0055c1d492e11d`.

`3513063.0` ran the full SuperSpeed path with the corrected, source-confirmed Android DBM reset/enable sequence. Fastboot disconnected at `23:37:31`; Fullerene's intentional HS attach appeared at `23:37:41`; the descriptor timed out with `-110` at `23:37:47`; Android `18d1:4ee7` returned at `23:38:07`; `boot-reason=watchdog`; no `1234:0001`. The full endpoint-publication path therefore reached the same failure boundary as the no-DBM control: DBM reset/enable is not the missing SS-link transition. Artifact SHA: `119bd17828ae571fee648c53ad9895afc40dce66178fccde15af4b6b79605796`.

The source audit then found a concrete remaining Bramble delta: Qualcomm's official `dwc3_otg_start_peripheral()` writes USB31 `DWC31_LINK_LU3LFPSRXTIM(0)` with GEN2=`6` and GEN1=`5` after DBM reset/device-mode selection and before gadget VBUS connect. The local SS handoff had no equivalent write. This is a controller link-handshake setting, not a packet wrapper; it is the next opt-in A/B, with the exact `0xd010`, `[23:16]`, and `[7:0]` fields taken from the official Bramble headers/glue.

`3544447.0` applied the source-confirmed Bramble USB31 LFPS exit-response timer (`DWC31_LINK_LU3LFPSRXTIM(0)`, GEN2=`6`, GEN1=`5`) with the same fixed lane-A/no-core-reset/no-SMMU/QMP/controller-clock/DBM conditions. Fastboot disconnected at `23:58:52`; Fullerene HS attach appeared at `23:59:03`; the descriptor timed out with `-110` at `23:59:08`; Android `18d1:4ee7` returned at `23:59:29`; `boot-reason=watchdog`; no `1234:0001`. The timer delta is negative and the next source comparison is the official `dwc3_phy_setup()` clear of `GUSB3PIPECTL.UX_EXIT_PX`. Artifact SHA: `594a941bb9505900fca579e8cb013e9a032320d4672a70fa62dbac2bc75f96d5`.

`3553220.0` applied the source-confirmed USB31 `dwc3_phy_setup()` clear of `GUSB3PIPECTL.UX_EXIT_PX` (bit 27), retaining the LFPS timer and all other fixed conditions. Fastboot disconnected at `00:05:17`; Fullerene HS attach appeared at `00:05:28`; the descriptor timed out with `-110` at `00:05:33`; Android `18d1:4ee7` returned at `00:05:54` and briefly re-enumerated at `00:05:55`; `boot-reason=watchdog`; no `1234:0001`. The UX_EXIT_PX delta is negative and the next source comparison is official post-`dwc3_phy_setup()` USB3 PHY power/suspend sequencing and local no-core-reset ordering. Artifact SHA: `6aa202f6b127456554e2a960fad303c680c25b5d228b32709913ede3dbe62c83`.

`3584703.0` applied the source-confirmed connected-cable QMP PHY resume delta: after QMP/DWC3 setup, `--ss-clear-qmp-autonomous` cleared the QMP autonomous-mode detectors/clamp before gadget Run/Stop. Fastboot disconnected at `00:27:44`; Fullerene HS attach appeared at `00:27:54`; the descriptor timed out with `-110` at `00:27:59`; Android `18d1:4ee7` returned at `00:28:20`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `186d11e09f24095f4cd331bec34dda395b37a688b7e15de4e7d3d01a14513c8d`.

`3600300.0` applied the source-confirmed QMP clock-enable ordering delta: `--ss-reassert-qmp-clocks-after-gctl` reasserted the QMP AUX/COM_AUX/PIPE clock branches after DWC3 global-control setup and before gadget Run/Stop. Fastboot disconnected at `00:38:42`; Fullerene HS attach appeared at `00:38:53`; the descriptor timed out with `-110` at `00:38:58`; Android `18d1:4ee7` returned at `00:39:19`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `19ca2178eb319d7ba5d69bb093db87bd10c1fea4892004ebcd886dbf0baddfeb`.

`3609687.0` applied the source-confirmed `dwc3_dis_sleep_mode()` ordering delta: `--ss-dis-sleep-mode-before-gadget` cleared USB2 `ENBLSLPM` and `GUCTL1.L1_SUSP_THRLD_EN_FOR_HOST` immediately after DWC3 global-control setup and before gadget Run/Stop. Fastboot disconnected at `00:44:39`; Fullerene HS attach appeared at `00:44:49`; the descriptor timed out with `-110` at `00:44:55`; Android `18d1:4ee7` returned at `00:45:15`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `d66c07fe295759b49162989b61b85b56c835b11b1b6679b5d4f877ec74d27dc1`.

`3627425.0` applied the source-confirmed USB2 PHY resume ordering delta: `--ss-reassert-hs-phy-ref-after-gctl` reasserted the Bramble `ref_clk_src` RPMh 19.2 MHz vote after DWC3 global-control setup and before gadget Run/Stop. Fastboot disconnected at `00:57:29`; Fullerene HS attach appeared at `00:57:40`; the descriptor timed out with `-110` at `00:57:45`; Android `18d1:4ee7` returned at `00:58:07`; `boot-reason=watchdog`; no `1234:0001`. The official `phy-msm-snps-hs.c` implementation of `usb_phy_set_suspend(usb2, 0)` only re-enables `ref_clk_src`; this source-mapped delta is negative. Artifact SHA: `df60bda6cd93caaf9762e1d36eb54ceba3368b353041c05a56533ab8933a7f44`.

`3636600.0` applied the exact source-confirmed QMP autonomous-mode disable: `--ss-clear-qmp-autonomous-exact` changed the autonomous register write from a masked clear to literal `0`, matching `msm_ssusb_qmp_enable_autonomous(phy, 0)`. Fastboot disconnected at `01:03:30`; Fullerene HS attach appeared at `01:03:40`; the descriptor timed out with `-110` at `01:03:46`; Android `18d1:4ee7` appeared at `01:04:06` and briefly re-enumerated at `01:04:07`; `boot-reason=watchdog`; no `1234:0001`. The exact-write delta is negative. Artifact SHA: `b11f38d071a7167f911621bf78c5411d6c1062241900f4bccf98033c98f77ad8`.

`3642418.0` applied the source-confirmed QMP resume ordering delta: `--ss-qmp-resume-wmb` inserted arm64 `wmb()` (`dsb(st)`) after the connected-cable clamp/autonomous/LFPS writes. Fastboot disconnected at `01:07:16`; Fullerene HS attach appeared at `01:07:27`; the descriptor timed out with `-110` at `01:07:32`; Android `18d1:4ee7` returned at `01:07:53`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `8bab39c4d363b905587c4b1213080ed0aa299423d83ba3e6ed265f51f144e650`.

`3650453.0` applied the source-confirmed QMP LFPS IRQ-clear ordering delta: `--ss-qmp-lfps-clear-wmb` replaced the local `SeqCst` fence with arm64 `wmb()` (`dsb(st)`) between the official `1` and `0` writes. Fastboot disconnected at `01:12:49`; Fullerene HS attach appeared at `01:13:00`; the descriptor timed out with `-110` at `01:13:05`; Android `18d1:4ee7` returned at `01:13:26`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `4b2db168439c6252db5e34a95bc292d5609252326d0db858839e33fd455df51d`.

`3659086.0` re-tested the existing source-confirmed USB3 PIPE soft-reset release on the current fixed full-path baseline with `FULLERENE_AARCH64_USB_SS_PHY_RESET_RELEASE=1`. Fastboot disconnected at `01:18:24`; Fullerene HS attach appeared at `01:18:35`; the descriptor timed out with `-110` at `01:18:40`; Android `18d1:4ee7` returned at `01:19:00`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `19ceadf0c786b8efe5f09bb3245b189b4a98f75551814145fbe013ca4a205b01`.

`3675364.0` applied the official `dwc3_override_vbus_status(false)` clear after the QMP disconnect notifier: SS `LANE0_PWR_PRESENT`, HS `UTMI_OTG_VBUS_VALID`, and HS `SW_SESSVLD_SEL` were cleared before the local no-core QMP reset/init. Fastboot disconnected at `01:29:48`; Fullerene HS attach appeared at `01:29:58`; the descriptor timed out with `-110` at `01:30:04`; Android SuperSpeed `18d1:4ee7` returned at `01:30:24`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `4eaca5edf99940b11027733eab0261caad6f20f5888559cf5d87f8eef6279008`.

`3684012.0` applied the official hibernation-gated `DCTL.KEEP_CONNECT` clear on the old-session stop: when GHWPARAMS1 reported hibernation, the local DCTL write cleared `KEEP_CONNECT` together with `RUN_STOP`. Fastboot disconnected at `01:35:21`; Fullerene HS attach appeared at `01:35:32`; the descriptor timed out with `-110` at `01:35:37`; Android SuperSpeed `18d1:4ee7` returned at `01:35:59` and re-enumerated at `01:36:00`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `3a306b32d8f0a4f6245f94d81d621fe836499c0475d17d3203596d443629a3c6`.

`3689425.0` applied the official `dwc3_usb3_phy_suspend(dwc, false)` USB3 `SUSPHY` clear after QMP disconnect and VBUS/session override teardown, before the local source-ordered QMP reset/init SUSPHY write. Fastboot disconnected at `01:38:52`; Fullerene HS attach appeared at `01:39:02`; the descriptor timed out with `-110` at `01:39:08`; Android SuperSpeed `18d1:4ee7` returned at `01:39:29`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `8fd192c52502e3e30c831db48af7ccf229e251ea4f2613d0ab5cfac521d656f8`.

`3694965.0` applied the official DWC3 gadget IRQ-disable delta: `DEVTEN=0` was written before the old-session Run/Stop. Fastboot disconnected at `01:42:27`; Fullerene HS attach appeared at `01:42:38`; the descriptor timed out with `-110` at `01:42:43`; Android SuperSpeed `18d1:4ee7` returned at `01:43:04`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `c697e6d7c010bcb35179d29ae4659d9e9b68d990a255c3ed0a01c3eebd549156`.

`3700749.0` applied the hardware side of the official EP0 OUT/IN endpoint disable: `DALEPENA` bits 0/1 were cleared before the old-session Run/Stop. Fastboot disconnected at `01:45:58`; Fullerene HS attach appeared at `01:46:08`; no Fullerene descriptor completed before Android SuperSpeed `18d1:4ee7` returned at `01:46:35`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. No explicit host `-110` line was captured in this run's kernel log. The delta is negative. Artifact SHA: `e350b36f9525391d8ebd29dc44614b1c1214e7f953523134f9c459a2fc6f9df4`.

`3715381.0` applied the hardware portion of the official post-stop GSI cleanup: GSI event-buffer counts 1..3 were acknowledged, then Qualcomm GSI `BLOCK_WR_GO` was set and `GSI_EN` cleared after old-session DCTL.Run/Stop. Fastboot disconnected at `01:56:08`; Fullerene HS attach appeared at `01:56:18`; the descriptor timed out with `-110` at `01:56:24`; Android SuperSpeed `18d1:4ee7` returned at `01:56:44`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `be9beed42813b00f30a99f919e249b7bac00551af7be8fb5698610c6523fcf95`.

`3762693.0` applied the source-correct preserve-state A/B for the DWC3 reference-clock timing registers: `--ss-preserve-ref-clock-state` skipped the historical local `GUCTL.REFCLKPER`/extended `GFLADJ` writes because Bramble qpr1 `dwc3-msm.c` has no `dwc3_msm_update_ref_clk()` helper and the Bramble DT has no frame-length-adjustment property. Fastboot disconnected at `02:29:02`; Fullerene HS attach appeared at `02:29:13`; the descriptor timed out with `-110` at `02:29:19`; Android SuperSpeed `18d1:4ee7` returned at `02:29:39`; `boot-reason=watchdog`; `lsusb` showed only fallback `18d1:4ee7`; no `1234:0001`. The delta is negative. Artifact SHA: `5f122410b37dffd98460c1a9c16c8a5f89dd2121195799bce5e8fbd869c579b3`.

The official `dwc3_stop_active_transfers_to_halt()` audit is complete but not directly actionable in a fresh fastboot handoff: `dwc3_stop_active_transfer_noioc()` needs the Linux software `dwc3_ep.resource_index`, while Fullerene inherits neither that structure nor a hardware register exposing the index. No guessed ENDTRANSFER resource index was added.

`3665411.0` applied the official QMP disconnect-notifier delta: `--ss-qmp-notify-disconnect` wrote PCS `POWER_DOWN_CONTROL=0` and read it back before the local no-core QMP reset/init. Fastboot disconnected at `01:22:49`; Fullerene HS attach appeared at `01:23:00`; the descriptor timed out with `-110` at `01:23:05`; Android SuperSpeed `18d1:4ee7` returned at `01:23:25`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative. Artifact SHA: `abbc009cdb08689b7707a8c410987a9da7fe8e192c2bba840fd57566f935c4f6`.

`1721868.0` re-ran the documented fixed direct-USB2 baseline (`--direct-handoff --start-after-connect --no-smmu --hsphy-source-exact --refresh-hsphy-power --enum-timeout 30 --hold 30`) from host-side Fastboot with no new delta. `fastboot boot` was accepted; Fastboot disconnected at `04:50:32`, the Fullerene HS marker `usb 1-9` appeared at `04:50:42`, the Device Descriptor timed out with `-110` at `04:50:47`, and Android SuperSpeed `18d1:4ee7` returned at `04:51:09`; `boot-reason=watchdog`; no `1234:0001`. This is a clean reproduction of the known no-SETUP/no-USB2-RX boundary on the current build. Boot image SHA: `c32b0324ca3fff6b1f8333b579844ecaf589176a5eb5d0f8fa10dd68d033df6f`.

`1807705.0` applied the last uncovered lito-usb.dtsi Connect-Done property: `--android-lpm-errata` (requires `--android-hs-lpm`) sets `DCTL.LPM_ERRATA=0xf` from the DT `snps,has-lpm-erratum` quirk exactly where qpr1 `dwc3_gadget_conndone_interrupt()` sets it on revisions >= 240A (Bramble's DWC_usb31 passes through the DWC31 flag), with the qpr1 `core.c` default `lpm_nyet_threshold = 0xf`. This was the only DT-sourced register field not yet covered by an A/B; the DT audit (tmp clone of `android-msm-bramble-4.19-android11-qpr1`, the only bramble branch) also machine-verified the 146-entry QMP init table, the 18-entry reg-offset list, the GSI offsets, the PDC trigger types (including HEAD `5d23766e` EDGE_RISING for dp/dm), the six controller clocks, and the `<0x3>` GSI event-buffer count as already implemented. ADB was `device`; `adb reboot bootloader` and `fastboot boot: OKAY` succeeded. The host saw Fullerene HS attach at `05:47:54`, the descriptor timed out with `-110` at `05:47:59`, and Android `18d1:4ee7` returned at `05:48:20`; `boot-reason=watchdog`; no `1234:0001`. The delta is negative; the remaining DT properties (`tx-fifo-resize` IN-EP resize, `usb3-u1u2-disable` SET_FEATURE gate) are unreachable in a control-only fresh-gadget handoff. Artifact SHA: `3eb706858475e2a94a3e26796c9af2236519f75dbc3c588e0ecf8b6f648e5315`.

### No-hardware enumeration proof (host-side, Linux-usbcore-faithful)

With the handset unavailable for further probing, the enumeration protocol was
proven host-side instead. `fullerene-kernel/src/arch/aarch64/usb_linux_host_enum.rs`
implements the exact Linux v6.6 usbcore enumeration sequence (source-fetched
from git.kernel.org: `hub.c` `hub_port_init()`/`get_bMaxPacketSize0()`/
`hub_set_address()`, `message.c` `usb_get_descriptor()`/`usb_get_string()`/
`usb_get_device_descriptor()`, `config.c` `usb_get_configuration()`/
`usb_get_bos_descriptor()`) as a virtual host driving Fullerene's
`Ep0Simulator` through the same `ControlAction`/response-buffer protocol the
hardware wrapper uses. Four tests pass (`cargo test --bin fullerene-kernel`,
98 total): the new-scheme 64-byte-read → SET_ADDRESS(7) → 18-byte descriptor →
config → strings → SET_CONFIGURATION sequence reaches `idVendor=0x1234`,
`idProduct=0x0001`, a consistent `wTotalLength`, and a committed
configuration; the old scheme (SET_ADDRESS first, then the 8-byte
`bMaxPacketSize0` read) also completes; a full USB reset between
enumerations returns to `1234:0001`; and an exact-length config read
terminates cleanly into the next SETUP. The QEMU EP0 self-test
(`--qemu-usb-sim`) still passes unchanged. This closes every software-side
question the host can ask: descriptor content, EP0 ordering, retry handling,
and post-status rearm are all enumeration-correct. The Bramble failure is
therefore confirmed to be entirely below EP0 — the USB2 PHY RX path that
never delivers a SETUP token — and remains reachable only with physical
evidence (JTAG/secure-debug capture or a USB protocol analyzer), consistent
with the boundary snapshot in `boundary-state.md`.

### Reviewer timing/reset follow-up (2026-09-04, handset reconnected)

The reviewer's six-point audit was source-checked against the qpr1 clone and
the current `main` before running. Point 2 (external HS-PHY reset assert →
100–150 µs → deassert) is already implemented:
`platform/bramble/usb_reset.rs::pulse_usb2_phy_reset()` drives the DT's
`phy_reset` (`GCC_QUSB2PHY_PRIM_BCR`, resource index 1) with
`delay_us(100)` between assert and deassert, runs before
`init_hsphy*()` exactly like `msm_hsphy_reset()` → `msm_hsphy_init()`, and is
on the fixed baseline. Point 3 (vdd/vdda18/vdda33) is likewise covered by the
baseline `--refresh-hsphy-power` RPMh re-vote (`refresh_usb_power(false)`);
these rails are RPMh-managed with no MMIO readback, so a state table is not
available and the vote itself is the only observable. Points 1 (QMP PHYSTATUS
poll time base), 4 (poll timeout as wall time), 5 (post-POR settle), and 6
(MMIO completion) produced three new source-confirmed A/Bs:

- `10801.0` is the corrected real-time-poll control build with no extra
  delta (rejected before boot because Fastboot was not engaged); the first
  booted run with the new default — `init_qmp_phy()` now samples PHYSTATUS
  every 1 µs for at most 1000 iterations (`usleep_range(1,2)` ×
  `INIT_MAX_TIME_USEC` = 1000, exactly Android's time base), with the old
  nop-loop retained behind `FULLERENE_AARCH64_USB_QMP_POLL_NOP_LOOP` — is
  `11071.0`. Fastboot disconnected; HS attach at `18:49:44`; descriptor
  `-110` at `18:49:49`; Android `18d1:4ee7` at `18:50:10`;
  `boot-reason=watchdog`; no `1234:0001`. Negative. Boot image SHA
  `c32b0324ca3fff6b1f8333b579844ecaf589176a5eb5d0f8fa10dd68d033df6f`
  (identical to the `1721868.0` baseline because the USB2-only path never
  reaches the QMP poll).
- `12066.0` restored the 150 µs post-POR settle inside the source-exact HS
  path (`FULLERENE_AARCH64_USB_HSPHY_POR_DELAY_150=1`, artifact SHA
  `a2d53cc6e04566d04ff439dc44e24f8635b43cd7d27b2a17c5cf244a812d61d5`). HS
  attach at `18:51:01`; descriptor `-110` at `18:51:06`; Android at
  `18:51:27`; `boot-reason=watchdog`; no `1234:0001`. Negative.
- `15449.0` combined both deltas (POR delay + nop-loop QMP poll restored, so
  the run is the exact pre-change kernel behavior on both axes and the
  Android-timescale default is validated against it). HS attach at
  `18:53:36`; descriptor `-110` at `18:53:41`; Android at `18:54:02`;
  `boot-reason=watchdog`; no `1234:0001`. Negative. Artifact SHA
  `a2d53cc6e04566d04ff439dc44e24f8635b43cd7d27b2a17c5cf244a812d61d5`.

The direct USB2 handoff does not reach the QMP poll at all (that loop only
runs on the SuperSpeed path), so the reviewer's point 1 cannot explain the
USB2 no-SETUP boundary; it is kept as the new default because it matches the
qpr1 time base, and the full-SS A/B of the poll change remains pending until
the SS path is revisited. The post-POR settle (point 5) acts on the
attach-reaching USB2 path and is now a reproduced negative. The remaining
reviewer concern (point 3 analog rails) has no host-readable state without a
protocol analyzer; the vote-based evidence stands.

### Reviewer round 3: DT source correction and raw-cell readout (2026-09-04)

The reviewer correctly flagged that the previous `DT_TWO_ENTRY` conclusion
rested on an 11-second attach that sits inside the ±1–2 s jitter band, and
that the qpr1 source itself is three-entry. Source recheck: `lito-usb.dtsi`
and `lito-qrd.dtsi` both carry `qcom,param-override-seq` with **three**
entries (`0x63 0x6c`, `0x85 0x70`/`0xc8 0x70`, `0x17 0x74`); the old
`phy_tables.rs` comment claiming a two-entry Google override contradicted
the very source it cited. Three fixes and two readout runs followed:

- `HS_DT_PARAM_OVERRIDE_CELLS` now captures the six raw FDT cells (packed
  value<<8|offset) at install time, published per-cell through the
  pre-connect readout channel (`hsphy-dt-cell0`..`5`).
- `53319.0`'s 11-second attach is reclassified as jitter, not code 1.
- `69825.0` (`hsphy-dt-cell0`): disconnect `19:28:50` → attach `19:29:00`
  = 10 s ⇒ **cell0 = 0**: the handset DTB does not provide
  `qcom,param-override-seq` to Fullerene at all (property absent from the
  matched node). The earlier `DT_TWO_ENTRY` classification was wrong.
- `72382.0` (`hsphy-table`): disconnect `19:30:22` → attach `19:30:32`
  = 10 s ⇒ **classification 0 = compiled fallback**. Every hardware run to
  date, including all `-110`/`-71` A/Bs, ran the compiled fallback.
- Combined with the fallback history this closes the TUNE3 question
  symmetrically: the old three-entry fallback (TUNE3=0x17 written) failed in
  every prior run, and the new two-entry fallback (TUNE3 absent) fails
  identically in `72382.0` (descriptor `-110`, watchdog, no `1234:0001`;
  artifact SHA `4e8565dbc105b89629cdd7aab08e3a2d91e0aa6abcc3a3a0eaa023699bd7f7f0`
  covers the cell0 run; the classification run's boot image is identical
  apart from the readout env). Analog tuning is not the boundary.
- The reviewer's priority-1 DCFG experiment already exists as a documented
  negative: `810905.0` set `--dcfg-superspeed` on the attach-reaching USB2
  path and lost even the HS attach. `857076.0` covers the DCFG
  receive-policy A/B. The initial-EP0-MPS-512 hybrid-state concern is
  therefore bounded by an existing run and the EP0-contract proof in
  `usb_linux_host_enum.rs`.

Net for this historical source-version comparison: the handset-DTB-vs-source
divergence is real and measured (the bootloader hands Fullerene a DTB without
the HS override property), and the two-pair candidate failed. The exact-build
factory package and its three-pair fallback are authoritative for the current
default; the raw-cell channel makes future DT questions one run each. The
no-SETUP boundary remains; physical capture (analyzer/JTAG) is still outside
the available equipment.

### Reviewer round 4: observer bugs fixed, divergence confirmed both sides

The reviewer correctly identified three defects in the round-3 observer, and
all three are fixed:

1. `HS_DT_PARAM_OVERRIDE_CELLS` packed six raw cells into three slots
   (pair-combined) and collapsed "absent" into the same zero as an
   incomplete pair. Replaced by `HS_DT_PARAM_OVERRIDE = (present, len_bytes,
   [Option<u32>; 6])`, captured at install time in `main.rs` via the new
   `fdt::find_compatible_property_observation()` (same walk discipline, a
   presence bit independent of length).
2. The timing channel clamped raw values with `.min(15)`; a valid qpr1 pair
   (0x636c packed) would have saturated to 15 s and the ~17 s biter could
   kill a legitimately-nonzero property run. The channel now publishes only
   small categorical codes via `hsphy_prop_code()`:
   `hsphy-prop-present` (0/1), `hsphy-prop-len` (0=absent, 1/2/3 =
   8/16/24 bytes, 4=other), `hsphy-prop-pair0/1/2` (0=absent/incomplete,
   1 = exact qpr1 base value, 2 = the qrd alternate `0xc8/0x70`,
   3 = other).
3. The fallback comment ("production has exactly two entries") contradicted
   the qpr1 source, which is three-entry in both `lito-usb.dtsi` and
   `lito-qrd.dtsi`. The comment now states both forms are experiment
   boundaries rather than verified production matches, and
   `install_dt_phy_sequences()` accepts both.

Two hardware runs with the fixed observer:

- `113968.0` (gate `hsphy-prop-present`): FALSE branch (recovery at 37 s,
  matching the bite+1 s FALSE bucket), tentative presence = 0.
- `116370.0` (pre-connect ladder `hsphy-prop-present`): disconnect
  `20:03:04` → attach `20:03:14` = 10 s = baseline + 0 ⇒ **presence code 0,
  confirmed**. Artifact SHA `443abc777ea21d0e1c1d56fd445534ad241f68691a99b881f7ac78c5afc6a539`.
- `174974.0` (same-walk `hsphy-node-proof`): disconnect `20:48:36` → HS
  attach `20:48:47` (11 s; baseline timing jitter), descriptor `-110` at
  `20:48:52`, Android `18d1:4ee7` at `20:49:13`, watchdog recovery; no
  `1234:0001`. Artifact SHA
  `4e8565dbc105b89629cdd7aab08e3a2d91e0aa6abcc3a3a0eaa023699bd7f7f0`.
  The run is a negative identity-readout attempt; the pre-connect timing
  bucket is not independently decodable from whole-second host timestamps,
  so it does not claim a numeric proof code.

Android-side cross-check: the runtime DT
(`/sys/firmware/devicetree/base/soc/hsphy@88e3000/`) **lists** the
`qcom,param-override-seq` name on the same node path, but the file content
is mode-000 to shell on this production user build (no root/su/magisk), so
the bytes are unreadable. The combination is decisive in direction:
**Fullerene's fastboot-boot DTB (x0) lacks the property while the Android
runtime DT (post-DTBO) carries it** — the divergence the reviewer
hypothesized, now measured from both sides. All hardware runs therefore
used the compiled fallback, and the round-3 "DT_TWO_ENTRY" claim is
withdrawn; the TUNE3 symmetric closure (3-entry fallback failed in every
pre-2026-09-04 run, 2-entry fallback fails identically now) stands.

With the observer fixed, the property question is closed, and the remaining
open families are the ones the ledger already bounds (no SETUP token past
the PHY; SOF-less RX). A USB protocol analyzer on D+/D- remains the next
decisive step: it separates "host emits SETUP, device silent" from
"PHY/UTMI ingress drops it", which no software readout can reach.

## Next source-directed investigation

| Order | Check | Result | Why |
| --- | --- | --- | --- |
| 1 | Trace QMP PCS/link-training state and secure ownership with read-only stage-20 selectors | — | `3228530.0`/`3233224.0`/`3236181.0` prove all three QMP GCC branches are `1`; corrected runs `3326735.0` and `3332679.0` prove retained QMP common/PCS power `1`. Keep lane A, controller-domain-first reassertion, and the corrected offset fixed |
| 1a | Read QMP `PCS_STATUS2[3]` (`RX_EQUALIZATION_IN_PROGRESS`) at stage 20 | — | Complete: `3264118.0` measured the 0-bit bucket, so RX equalization was not in progress at the snapshot. Do not introduce the optional INSIG/link-training mutation on this evidence alone |
| 1b | Read the live USB31 DWC3 `LINKSTATE` from `DWC31_LINK_GDBGLTSSM=0xd050` with `ss-ltssm-bit0..3` at stage 21 | — | `3283449.0` and the four old bit runs were valid pre-Run/Stop samples only; Qualcomm gadget start initializes `SS_DIS` before production `DCTL.Run/Stop`. Corrected timing runs give bit0=`0` (`3383134.0`), bit2=`0` (`3395716.0`), bit3=`0` (`3401041.0`), and wide bit1=`0` (`3408278.0`), but retake `ss-domain-core` first because `3380073.0` used the wrong IP-prefix predicate |
| 1c | Split the corrected post-Run/Stop domain result into GDSC/core-branch/mock-UTMI branch selectors | — | Complete: `3422166.0`, `3426864.0`, `3429063.0`, and `3457638.0` all read `0`; the full post-Run/Stop CX/BCM, regulator, GDSC, RCG, and branch refresh in `3449266.0`/`3453087.0`/`3455402.0`/`3457638.0` also leaves each selected bit at `0`. This is a shared transition/ownership collapse, not a single missing branch |
| 2 | Apply the official USB31 LFPS exit-response timer delta before SS Run/Stop | — | Complete: `3544447.0` applied GEN2=`6` / GEN1=`5`; the full path still produced HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery with no `1234:0001` |
| 3 | Compare/apply the official `dwc3_phy_setup()` clear of `GUSB3PIPECTL.UX_EXIT_PX` | — | Complete: `3553220.0` applied the bit-27 clear; the full path still produced HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery with no `1234:0001` |
| 3a | Apply the official connected-cable QMP PHY resume/autonomous-mode clear | Complete: `3584703.0` applied the QMP autonomous-mode disable before gadget Run/Stop; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep the QMP power/clock/reset, DCTL, endpoint, and packet conditions fixed for the next discriminator |
| 3b | Apply the official connected-cable QMP clock enable after DWC3 global-control setup | Complete: `3600300.0` moved the QMP clock-branch reassertion after global-control setup; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep QMP power/reset, autonomous mode, DCTL, endpoint, and packet conditions fixed for the next discriminator |
| 3c | Move the official `dwc3_dis_sleep_mode()` clear before the SuperSpeed gadget start | Complete: `3609687.0` moved the `ENBLSLPM`/`GUCTL1.L1_SUSP_THRLD_EN_FOR_HOST` clear before gadget Run/Stop; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep USB3 PHY power/reset, DCTL, endpoint, and packet conditions fixed for the next discriminator |
| 3d | Reassert the official USB2 PHY `ref_clk_src` resume after DWC3 global-control setup | Complete: `3627425.0` reasserted the source-mapped RPMh 19.2 MHz vote; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | The legacy Bramble HS PHY's `usb_phy_set_suspend(usb2, 0)` has no extra analog write. Lito provides no generic `phys`/`phy-names`, and connected-cable QMP regulator resume is gated off by the official driver; avoid speculative generic-PHY or regulator mutations |
| 3e | Match the official connected-cable QMP autonomous-mode disable's literal-zero register write | Complete: `3636600.0` used the exact `write(..., 0)` form; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | The prior masked clear and the official literal-zero write are now separated. The next source audit is the official QMP resume `wmb()` boundary |
| 3f | Apply the official QMP resume `wmb()` after clamp/autonomous writes | Complete: `3642418.0` applied arm64 `wmb()` (`dsb(st)`) after the connected-cable QMP resume writes; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3g | Match the official QMP `wmb()` between the LFPS RXTERM IRQ clear `1` and `0` writes | Complete: `3650453.0` applied arm64 `wmb()` (`dsb(st)`) between the two QMP IRQ-clear writes; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3h | Re-test the existing official USB3 PIPE soft-reset release on the current fixed full-path baseline | Complete: `3659086.0` applied `GUSB3PIPECTL.PHYSOFTRST` release; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | The stage-13 and current full-path tests are both negative; do not repeat this delta without a new source distinction |
| 3i | Add the official QMP `usb_phy_notify_disconnect(ss_phy)` power-down write before the no-core QMP reset/init | Complete: `3665411.0` applied the PCS `POWER_DOWN_CONTROL=0` write/readback; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3j | Add the official `dwc3_override_vbus_status(false)` clear of USB2/USB3 VBUS and session-valid override bits before the no-core QMP reset/init | Complete: `3675364.0` applied the three QSCRATCH clears; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3k | Match the official hibernation-gated `DCTL.KEEP_CONNECT` clear on old-session stop | Complete: `3684012.0` applied the source-confirmed same-write `KEEP_CONNECT` clear; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3l | Match the official `dwc3_usb3_phy_suspend(dwc, false)` USB3 `SUSPHY` clear after peripheral-stop VBUS/PHY teardown | Complete: `3689425.0` applied the source-confirmed USB3 `SUSPHY` clear; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3m | Match the official DWC3 gadget IRQ disable before old-session stop | Complete: `3694965.0` applied the source-confirmed `DEVTEN=0` write; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3n | Match the official `dwc3_gadget_run_stop(false)` EP0 OUT/IN endpoint disable before old-session Run/Stop | Complete: `3700749.0` applied the source-confirmed `DALEPENA` bits 0/1 clear; the full path remained negative with HS attach, no completed Fullerene descriptor, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3o | Compare the official `dwc3_stop_active_transfers_to_halt()` / `dwc3_stop_active_transfer_noioc()` active-transfer halt with the fresh fastboot takeover | Complete: source audit found no exact handoff A/B | The official helper requires Linux `dwc3_ep.resource_index`; the fresh takeover has no inherited software resource indices and DALEPENA has no equivalent readout. Do not guess ENDTRANSFER parameters |
| 3p | Reproduce the hardware portion of the official post-stop GSI cleanup | Complete: `3715381.0` applied the GSI event-buffer count clear and Qualcomm block/disable sequence; the full path remained negative with HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery; no `1234:0001` | Keep DCTL, SMMU, endpoint, event-ring, and packet changes downstream of a host-visible SS attach |
| 3q | Complete the bounded read-only QMP/secure-ownership audit for a source-visible SS link transition | Complete: one non-Bramble controller-timing write was identified for a preserve-state A/B; no further safe delta identified | Bramble's legacy `usb-phy` DT binding excludes generic-PHY `power_on`; official QMP power/clock/reset/table/start and Qualcomm DWC3 VBUS/DBM/LFPS/sleep boundaries are covered by source comparison and existing A/Bs. Hibernation `KEEP_CONNECT` is tied to `GCTL.GBLHIBERNATIONEN` and scratch-buffer setup, while the official source says device-mode hibernation is not implemented; do not set the bit alone |
| 3r | Obtain new physical evidence at the secure-firmware/PHY-wire boundary | Pending external read-only evidence | The source-correct preserve-state A/B also remained negative without a host-visible SS identity. Next useful evidence is JTAG/secure-debug register capture or a USB protocol analyzer; do not add guessed ENDTRANSFER indices, UTMI/PHY register mutations, packet wrapping, or the destabilizing post-Run/Stop GCC reset |
| 3s | Test the source-correct preserve-state A/B for DWC3 reference-clock timing | Complete: `3762693.0` negative; no `1234:0001` | The Bramble qpr1 Qualcomm source has no `dwc3_msm_update_ref_clk()` helper, and the DT has no `snps,quirk-frame-length-adjustment`; skipping the historical local `REFCLKPER`/extended `GFLADJ` writes still produced HS attach, descriptor `-110`, Android `18d1:4ee7`, and watchdog recovery |
| 3t | Test the existing source-confirmed QMP common/PCS power-up reassertion after QMP init | Complete: `3787980.0` negative; no `1234:0001` | Adding `--ss-reassert-qmp-power` wrote the official QMP common and PCS power controls to `1` after QMP init. The run still produced HS attach, descriptor `-110`, Android SuperSpeed `18d1:4ee7`, and watchdog recovery; external secure-firmware/PHY-wire evidence remains the next step |
| 3u | Test the source-confirmed QMP power-up reassertion after DWC3 global-control setup | Complete: `3800747.0` negative; no `1234:0001` | Adding only `--ss-reassert-qmp-power-after-gctl` replayed the official post-global-control QMP power-consumer enable. The host still saw only a high-speed attach with no descriptor completion before Android SuperSpeed `18d1:4ee7` fallback and watchdog recovery; external secure-firmware/PHY-wire evidence remains the next step |
| 3v | Reproduce the official USB2 legacy-PHY reset/init boundary before the no-core SuperSpeed QMP reset/init | Complete: `4092211.0` physical A/B negative; no success | The qpr1 `dwc3_core_soft_reset()` order is now physically tested with `--ss-reinit-hs-phy`. Source audit corrected the mapping claim: qpr1's `usb_phy_reset()` callback is unset for this legacy PHY driver and is therefore a no-op; the effective reset is the `msm_hsphy_init()` reset. The opt-in placed the USB2/USB3 `SUSPHY` writes first, mapped that effective reset and the EUD gate, then the Bramble regulator-enable, 19.2 MHz `ref_clk_src`, GCC PHY-reset, and source-exact femto-PHY sequence. The official DTS has no HS-PHY `cfg_ahb_clk`. `fastboot boot` was accepted, but Fullerene never attached; Android `18d1:4ee7` returned after 34 s with `boot-reason=watchdog`, and no Fullerene descriptor or `1234:0001` appeared. Artifact SHA `0586ac287170a4c4202e0513fd50a24200550a7011799d864f9f4cada2685132` |
| 3w | Reproduce the official controller-side `dwc3_phy_setup()` writes before no-core QMP init | Complete: `4114953.0` physical A/B negative; no success | The opt-in cleared `UX_EXIT_PX`, selected UTMI-8/`USBTRDTIM=9`, and asserted USB2/USB3 `SUSPHY` before QMP reset/init. The host still saw HS attach, Device Descriptor `-110`, and Android `18d1:4ee7`; `boot-reason=watchdog`; no `1234:0001`. Artifact SHA `28c5fd8e6cefe70bed8c7a86d51e884b173e595cda498c841dd76f9f7dd9b09d` |
| 3x | Test the full Android-style EP0 restart at the direct USB2 Run/Stop boundary with the attach-reaching controls | Complete: `4178263.0` physical A/B negative; no `1234:0001` | QEMU, boot audit, and `fastboot boot` passed, but no Fullerene HS attach appeared; Android SuperSpeed `18d1:4ee7` returned after 38 s and `boot-reason=watchdog`. Repeating the complete resource/configuration/SETUP epoch after the existing handoff suppresses the known attach path; it is not an enumeration fix. Artifact SHA `f888b8178a36d05ec0bb0ed161b36f5d2b736cc6841c4beab6d331b70a7d9a37` |
| 3y | Reproduce Qualcomm `DWC3_CONTROLLER_NOTIFY_CLEAR_DB` immediately after device-core reset | Complete: `16669.0` physical A/B negative; no `1234:0001` | QEMU, boot audit, and `fastboot boot` passed. The host saw Fullerene HS attach at `08:21:20`, Device Descriptor `-110` at `08:21:25`, then Android SuperSpeed `18d1:4ee7` at `08:21:46`; `boot-reason=watchdog`. Blocking GSI doorbells and clearing `GSI_EN` immediately after the DWC3 reset matched the official Qualcomm notification but did not move the descriptor-timeout boundary. Artifact SHA `bc3a134bf773378665bdf2c81fafab9f13761a3fb07dc7452998829f1d8e8a35` |
| 3z | Reproduce the Bramble qpr1 HS-PHY EUD early-return and source-exact femto-PHY init on the direct USB2 handoff (`--direct-handoff --start-after-connect --no-smmu --hsphy-source-exact --refresh-hsphy-power --enum-timeout 30 --hold 30`) | Complete: `38983.0` physical A/B negative; no `1234:0001` | QEMU, boot audit, and `fastboot boot` passed. The host saw Fullerene HS attach at `08:36:16`, Device Descriptor `-110` at `08:36:21`, then Android SuperSpeed `18d1:4ee7` at `08:36:42`; `boot-reason=watchdog`. The direct path now checks the DT EUD resource before the optional rail refresh/reset/init, and uses the official register order with the DTS's three override pairs; this did not move the no-data descriptor boundary. Artifact SHA `a7a028ebf5442b871391dccbc9e26b10c6df7d62759164248e2689fb4cbb656d` |
| 3aa | Retake the historical 16-bit/`USBTRDTIM=5` branch with the current source-exact HS-PHY path | Complete: `69641.0` reproduced `-71`; no `1234:0001` | HS attach at `08:57:06`; four zero-data Device Descriptor `-71` completions followed by setup-address `-71` failures; Android SuperSpeed `18d1:4ee7` returned at `08:57:32`; `boot-reason=watchdog`. This confirms the user's historical branch, but not usable EP0 progress: no descriptor bytes were returned. Artifact SHA `505963020d018f25c8a78bc1042494656ccfdd38f6066d35de819ce3b260b430` |
| 3ab | Apply the newly wired direct-path HS-PHY-before-DWC3-reset ordering to the historical 16-bit/`USBTRDTIM=5` branch | Complete: `89051.0` negative; no `1234:0001` | HS attach at `09:09:45`; four zero-data Device Descriptor `-71` completions and setup-address `-71` failures; Android SuperSpeed `18d1:4ee7` returned at `09:10:12`; `boot-reason=watchdog`. The missing direct-path ordering was exercised, but it did not move the historical protocol-error boundary or return descriptor bytes. Artifact SHA `8e7b566d7486e397942eb1b2e0cee441774ab0ce815482157c478f32a223e37d` |
| 3ac | Apply direct-path HS-PHY-before-DWC3-reset to the correctly unset 8-bit/`USBTRDTIM=9` baseline | Complete: `92588.0` negative; no `1234:0001` | HS attach at `09:11:21`; Device Descriptor `-110` at `09:11:27`; Android SuperSpeed `18d1:4ee7` returned at `09:11:47`; `boot-reason=watchdog`. This corrects the earlier mistaken `PHYIF=16-bit=0` invocation and confirms the true 8-bit result. Artifact SHA `d9894435ea9e54be95e6f29734a3fef31ed2442b0c4817aac9b6412f724cd32d` |
| 3ad | Select the Bramble DT/Android HS performance core clock on the direct USB2 handoff | Complete: `96071.0` negative; no `1234:0001` | With true 8-bit/`USBTRDTIM=9`, source-exact HS-PHY, and rail refresh retained, `--usb-core-hs-clock` produced HS attach at `09:12:44`, descriptor `-110` at `09:12:49`, Android `18d1:4ee7` at `09:13:10`, and `boot-reason=watchdog`. The 66.666667 MHz HS clock did not move the no-data boundary. Artifact SHA `0c303788d4cddcd0fdba6a845f980c877e3b3353db083b0c34de743061f5a025` |
| 3ae | Extend the pre-Run/Stop clock-stable delay on the true 8-bit direct USB2 handoff | Complete: `101507.0` negative; no `1234:0001` | With source-exact HS-PHY and rail refresh retained, `--clock-stable-delay-us 20000` produced HS attach at `09:16:11`, descriptor `-110` at `09:16:17`, Android SuperSpeed `18d1:4ee7` at `09:16:39`, and `boot-reason=watchdog`. The extra 20ms did not move the no-data boundary. Artifact SHA `66fbfa03448ba6bf04949446bca66355c9e0693bec3bfb6af878ee3347069d91` |
| 3af | Combine HS core-clock vote with the 20ms clock-stable delay on the true 8-bit direct USB2 handoff | Complete: `112714.0` negative; no `1234:0001` | The combined `--usb-core-hs-clock --clock-stable-delay-us 20000` run produced HS attach at `09:24:36`, descriptor `-110` at `09:24:41`, Android SuperSpeed `18d1:4ee7` at `09:25:02`, and `boot-reason=watchdog`. Neither the combined timing/clock condition nor the prior individual A/Bs moved the no-data boundary. Artifact SHA `c91433570f9951e5b2153d83b50653f1adf8c4ac2ffb5de3a541d9578a67eede` |
| 3ag | Audit the historical SS/Type-C matrix for the missing explicit lane-B plus observer combination | Complete: `157537.0` reproduced `-62`; no `1234:0001` | With `--super-speed --qmp-lane b --no-core-reset --stop-after-stage 13 --no-smmu` and Type-C observation enabled, xHCI timed out setup-device at `09:53:12`/`09:53:17`, rejected address 18 with `-62`, then stock Android SuperSpeed `18d1:4ee7` returned at `09:53:33`. No Fullerene identity appeared; the `-62` event is not deeper enumeration because `New USB device found` belonged to Android. Artifact SHA: `d133d9ddf96e2a6f91aef4a5a7a850b2899e864b00941ce03678453e144875ed` |
| 3ah | Run the same explicit lane-B plus Type-C-observer condition without the stage-13 stop | Complete: `161709.0` negative; no `1234:0001` | With `--super-speed --qmp-lane b --no-core-reset --no-smmu` and Type-C observation enabled, xHCI timed out setup-device at `09:55:43`, reported `Device not responding to setup address`, then rejected address 23 with `-71` at `09:55:44`. The port power-cycled at `09:55:48`; stock Android SuperSpeed `18d1:4ee7` returned at `09:56:04`. No Fullerene identity appeared, so the unrestricted path did not turn the stage-13 `-62` condition into enumeration. Artifact SHA: `74885dc9a24e210043c92fc53594c4ffb63900987e6b560066dd52e209524fb3` |
| 3ai | Run the next untested full SuperSpeed matrix cell: explicit lane B with Type-C observer disabled | Complete: `166686.0` negative; no `1234:0001` | With `--super-speed --qmp-lane b --skip-typec-spmi --no-core-reset --no-smmu`, xHCI timed out setup-device at `09:59:05`, reported `Device not responding to setup address`, then rejected address 28 with `-71` at `09:59:06`. Stock Android SuperSpeed `18d1:4ee7` returned at `09:59:26`; no Fullerene identity appeared. Artifact SHA: `c338ccc9780830af709ae08249d6857add1263976a50dfd72d51111b7286bad1` |
| 3aj | Run the next untested full SuperSpeed matrix cell: explicit lane A with Type-C observer enabled | Complete: `170017.0` negative; no `1234:0001` | With `--super-speed --qmp-lane a --no-core-reset --no-smmu` and Type-C observation enabled, no Fullerene identity appeared. The host instead logged an unrelated high-speed device descriptor timeout on `usb 1-9` at `10:01:22`; stock Android SuperSpeed `18d1:4ee7` returned at `10:01:44`; `boot-reason=watchdog`. Artifact SHA: `cd6abfdc3323c5b13fb4cb4eb7d735d66583eb3cda5e58f5beb433ff9c317021` |
| 3ak | Run the final untested full SuperSpeed lane/observer matrix cell: explicit lane A with Type-C observer disabled | Complete: `173030.0` negative; no `1234:0001` | With `--super-speed --qmp-lane a --skip-typec-spmi --no-core-reset --no-smmu`, no Fullerene identity appeared. The host logged a high-speed `usb 1-9` descriptor timeout at `10:03:19`; stock Android SuperSpeed `18d1:4ee7` returned at `10:03:40`; `boot-reason=watchdog`. Artifact SHA: `644ee727a70a2e6bb4022d6e4dc4ab7d77d682ae682e77b5c42532d22a6e621f` |
| 3al | Exercise the Harness ADB-to-Fastboot transition on the source-exact direct USB2 baseline | Complete: `184240.0` transport validation; no `1234:0001` | `--adb-reboot-to-fastboot --direct-handoff --start-after-connect --no-smmu --hsphy-source-exact --refresh-hsphy-power --enum-timeout 30 --hold 30` recorded `adb-state-before-fastboot=device`, issued `adb reboot bootloader`, accepted `fastboot boot: OKAY`, then reproduced the Fullerene HS attach at `10:11:41`, descriptor `-110` at `10:11:46`, and Android SuperSpeed `18d1:4ee7` at `10:12:07`. Transport files: `transport-preflight.txt`, `adb-state-before-fastboot.txt`, `adb-reboot-bootloader.txt`. Artifact SHA: `a7a028ebf5442b871391dccbc9e26b10c6df7d62759164248e2689fb4cbb656d` |
| 3am | Test the retained SETUP-before-arm race on the new ADB-to-Fastboot transport path (`--signal-probe --signal-cmd-gate setup-first --observe-secs 30`) | Complete: `212614.0` negative diagnostic; no `1234:0001` | ADB was `device`, `adb reboot bootloader` and `fastboot boot: OKAY` succeeded. Fastboot disconnected at `10:26:59`; Fullerene HS attach appeared at `10:27:13`; descriptor read timed out with `-110` at `10:27:18`; Android SuperSpeed `18d1:4ee7` returned at `10:27:44`; `boot-reason=watchdog`. The `setup-first` gate did not fire, so the retained trace does not support the hypothesis that SETUP arrived before the EP0 arm. The boundary remains no SETUP/no USB2 RX data. Artifact SHA: `e34b309e9af5f18d4d06fd45b798b54cb3cfda5a15846dbd6425eb3bc0282a52` |
| 3an | Confirm that any SETUP reached the retained EP0 trace on the new ADB-to-Fastboot transport path (`--signal-probe --signal-cmd-gate setup --observe-secs 30`) | Complete: `225452.0` negative diagnostic; no `1234:0001` | ADB was `device`; `adb reboot bootloader` and `fastboot boot: OKAY` succeeded. Fastboot disconnected at `10:36:04`; Fullerene HS attach appeared at `10:36:18`; descriptor read timed out with `-110` at `10:36:23`; Android SuperSpeed `18d1:4ee7` returned at `10:36:44`; `boot-reason=watchdog`. The broad `setup` gate did not fire either, directly confirming no retained SETUP reached EP0 on this transport. Do not repeat this diagnostic on the equivalent profile. Artifact SHA: `ac6ed0d624e55b74ac98adb370e89bd5ef277221659f8a6a32ae38cf589705cc` |
| 3ao | Apply the last uncovered lito-usb.dtsi Connect-Done property: set `DCTL.LPM_ERRATA=0xf` from the DT `snps,has-lpm-erratum` quirk (`--android-hs-lpm --android-lpm-errata`) | Complete: `1807705.0` physical A/B negative; no `1234:0001` | Machine-verified DT audit of the only bramble branch (`android-msm-bramble-4.19-android11-qpr1`, same commit as d1): QMP init table (146 entries), reg-offset list, GSI offsets, PDC trigger types incl. HEAD `5d23766e` EDGE_RISING, six controller clocks, and `<0x3>` GSI event buffers are all already implemented; `snps,has-lpm-erratum` → `DCTL.LPM_ERRATA` was the only remaining DT-sourced register field, per qpr1 `dwc3_gadget_conndone_interrupt()` (`revision >= 240A`, core.c default `lpm_nyet_threshold=0xf`, DWC_usb31 passes via the DWC31 flag). ADB was `device`; `fastboot boot: OKAY`; HS attach at `05:47:54`, descriptor `-110` at `05:47:59`, Android `18d1:4ee7` at `05:48:20`; `boot-reason=watchdog`. The remaining DT properties (`tx-fifo-resize` IN-EP resize, `usb3-u1u2-disable` SET_FEATURE gate) are unreachable in a control-only fresh-gadget handoff; the DT-driven register surface is now exhausted. Artifact SHA `3eb706858475e2a94a3e26796c9af2236519f75dbc3c588e0ecf8b6f648e5315` |
| 4 | Only if SS link is present, inspect DWC3/SMMU/endpoint ownership | — | Then move to `DCFG`, `DEPSTARTCFG`, endpoint resources, event ring, and EP0. Until then, endpoint and packet-format changes are downstream |
| 5 | Only after a valid EP0 data stage, inspect descriptor bytes | — | Wrapping is downstream of the observed no-identity/no-payload boundary |

Latest classified hardware run: `1693171.0` retested the full SuperSpeed path with the current qpr1-derived QMP, DBM, LFPS, DWC3, and old-session cleanup controls. `fastboot boot` was accepted; the host kernel saw a Fullerene high-speed attach on `usb 1-9` at `17:17:40`, but no device descriptor completed before stock Android `18d1:4ee7` over SuperSpeed returned at `17:18:06`; `1234:0001` did not appear and the boot reason was `watchdog`. The latest SS corrections therefore still stop before a host-visible SS identity and do not produce EP0 data. Artifact SHA: `e7350ef61795a36a6755a51d4c33968a7fc18896d986fef8c30ee633fb1475f2`. This run had no separate usbmon file; the preceding `853186.0` usbmon capture remains the latest raw-request evidence and delivered no descriptor response bytes. The concrete USB2 boundary remains no EP0 response/no USB2 RX payload; `1234:0001` has not been observed.

### Final software audit follow-up: Full-Speed bypass and DWC3 internal debug queues

The reviewer identified two remaining software-only discriminators. The
published qpr1/DWC3 sources were re-read before testing: `DCFG.FULLSPEED` is
the standard DWC3 device-speed encoding, and Linux `debugfs.c` exposes
`GDBGFIFOSPACE`, `GDBGLSPMUX/GDBGLSP`, and `GDBGEPINFO0/1` as read-only
diagnostics. The FIFO sampler was implemented without acknowledging the event
ring or changing endpoint state. It samples the eight Linux queue types and
all 16 device-mode LSP selectors during the existing EP0 observation loop;
raw samples are retained as `TRACE_DWC3_DEBUG`, while the timing channel only
publishes bounded min/max/change categories.

`190834.0` exercised the already-wired `--dcfg-fullspeed` diagnostic on the
fixed direct USB2 baseline. The host explicitly reported:

```text
usb 1-9: new full-speed USB device number 15 using xhci_hcd
```

but no descriptor response arrived before Android fallback:

```text
disconnect 21:00:34 -> full-speed attach 21:00:45
Android 18d1:4ee7 21:01:11
boot-reason=watchdog
1234:0001: not reached
```

Artifact SHA:
`9b8b57af06b70ca3cd674c23f9bab929743a59e5ab25ade8303c997806f1d296`.
This is a strong negative for the narrow hypothesis "HS chirp/data mode alone
is the failure": forcing the host to FS removes HS attach, but the EP0
descriptor path still produces no response. It does not yet distinguish the
common FS/HS PHY RX path from the DWC3 device ingress.

The read-only internal-debug sampler is now part of the probe loop. A first
run (`199575.0`, selector `dwc3-debug-rxreq`) built and booted the new image,
reached the ordinary HS attach, and returned Android after 42 seconds; the
host still saw descriptor `-110` and no `1234:0001`. Artifact details are in
`tmp/fullerene-bramble-loop.199575.0/`; the timing bucket is not treated as a
numeric queue result because it is a whole-second host observation. The new
selector family is ready for calibrated one-bit runs (`dwc3-debug-rxreq`,
`dwc3-debug-rxinfo`, `dwc3-debug-eventq`, and `dwc3-debug-lsp-change`).

The boundary is therefore not closed to hardware evidence: FS also fails,
while the corrected DWC3 `SPACE_AVAILABLE` stage values remain undecoded from
coarse host timing. The selectors have now been measured as readout executions,
but their values must not be interpreted as queue occupancy. Keep the analyzer
deferred only until one separated free-space bucket or retained-trace decode is
attempted.

### Corrected DWC3 `SPACE_AVAILABLE` audit

The earlier interpretation of the `GDBGFIFOSPACE` results is withdrawn. Linux's
`dwc3_core_fifo_space()` writes the EP0/queue selector and returns
`DWC3_GDBGFIFOSPACE_SPACE_AVAILABLE(reg)`, namely bits 31:16. A reported zero
therefore means **zero free space (or an unsupported/debug-invalid readout)**;
it is not an occupancy/empty-queue value and does not prove that USB packets
never reached the controller.

The sampler is now explicitly labeled `SPACE_AVAILABLE` and retains five
stage vectors for every queue:

```text
0 entry / Fastboot-inherited
1 after DWC3 reset
2 after EP0 endpoint/resource publication
3 immediately before Run/Stop
4 immediately after Run/Stop
```

It also exposes per-queue stage-change selectors. The LSP local comparison was
renamed to `lsp_selector_diff_mask`; the live `LIVE_DWC3_DEBUG_LSP_CHANGE`
field remains the actual time-direction change mask.

Four new hardware runs used the rebuilt image and the same direct USB2 profile:

| Run | selector | host timeline | interpretation |
| --- | --- | --- | --- |
| `264920.0` | `dwc3-free-entry-rxreq-wide` | disconnect `21:51:37` → HS attach `21:51:57` → `-110` `21:52:03` → Android `21:52:23` | entry `SPACE_AVAILABLE` wide bucket exercised; value not decoded from whole-second timing |
| `267527.0` | `dwc3-free-entry-rxinfo` | disconnect `21:53:21` → HS attach `21:53:35` → `-110` `21:53:40` → Android `21:54:01` | entry `SPACE_AVAILABLE` selector exercised; value not decoded |
| `270353.0` | `dwc3-free-entry-eventq` | disconnect `21:55:01` → HS attach `21:55:15` → `-110` `21:55:20` → Android `21:55:41` | entry `SPACE_AVAILABLE` selector exercised; value not decoded |
| `272154.0` | `dwc3-free-rxreq-change` | disconnect `21:55:58` → HS attach `21:56:17` → `-110` `21:56:22` → Android `21:56:45` | stage free-space change selector exercised; value not decoded |

Artifacts:

```text
264920.0  cecde97c748783e5b63e91b349266df3ed51e72a0d89a6cacd06551330bd2649
267527.0  0b90233a55a3cfe5ca097b78de714341dd548ee38edcaa7f66c34977fcf17445
270353.0  e359e338711f9fc62870e08faf851d43cd72bf030e086f76d7d9f3cb34dbac30
272154.0  17fba1d30c2bdc0d8897931c86b360233fba387be2a0e1b3d6f3e88b9f2faab8
```

All four runs remained non-enumerating and recovered by the normal watchdog
path. The host timestamps prove the readout images reached the ordinary HS
attach boundary, but they do **not** decode the free-space categories. Thus the
DWC3 queue evidence is currently provisional, not a PHY/UTMI ingress proof.
The Full-Speed negative remains valid, but the next software step is to make
one wider/separated stage bucket independently decodable before escalating to
wire or secure-debug capture.

### 2026-09-05 historical qpr1 source correction and physical retakes

This historical qpr1-source experiment is superseded by the exact-build
factory-package extraction recorded below; its two-pair values remain useful
only as an explicit negative A/B control.

The local Google Bramble Android-11 qpr1 DTS was re-read as primary source for
that source-version experiment. Its `lito-bramble-usb.dtsi` node contains two
HS-PHY override pairs, `0x67@0x6c` and `0xc8@0x70`; these are not the exact
Android-14 `UP1A.231105.001.B2` factory-package values. The compiled fallback
temporarily used those two pairs when the bootloader-provided DTB had no
usable property; the exact-build stock extraction below later corrected the
default to three pairs.

Three same-day `fastboot boot` retakes were recorded. The corrected fallback
with source-exact HS-PHY-before-reset, rail refresh, USB2 `SUSPHY`, initial EP0
MPS 512, and delayed SETUP arm (`1207420.0`) did not produce a Fullerene USB
attach; Android SuperSpeed `18d1:4ee7` returned at `11:31:08`. Removing only
the delayed arm (`1209541.0`) likewise produced no Fullerene attach; Android
returned at `11:32:27`. Enabling the qpr1 gadget-start restart sequence just
before Run/Stop (`1211111.0`) also produced no Fullerene attach; Android
returned at `11:33:26`.

Artifact SHAs:

```text
1207420.0  38950320e4b38565a0eb7a290a79b100b50c04ef2184b2962f9c30514a9f69a6
1209541.0  38950320e4b38565a0eb7a290a79b100b50c04ef2184b2962f9c30514a9f69a6
1211111.0  6b4e23de38fdd1947ff416845b5ceebd4f79c0006b202f680722a0578d4c5aaa
```

These are negative transport results, not a closure of the USB2 source
question: no analyzer or secure-debug capture was used, and no flash or erase
operation was attempted.

### 2026-09-05 Exact ABL EP0-field retake

The next source-guided control combined the disassembly-confirmed ABL
`SETEPCONFIG` fields with ABL's selective `DEPCMDPAR` publication mask. It also
retained the corrected Bramble HS-PHY fallback (`0x67@0x6c`, `0xc8@0x70`),
source-exact HS-PHY initialization before DWC3 reset, and rail refresh:

```text
cargo run -q -p flasks --bin bramble-usb -- loop --template \
  .../fullerene-bramble-boot.img --adb-reboot-to-fastboot --direct-handoff \
  --start-after-connect --no-smmu --hsphy-source-exact --hsphy-before-reset \
  --refresh-hsphy-power --abl-ep-config --abl-command-params \
  --enum-timeout 30 --hold 5 --fastboot-wait 60
```

Run `1220660.0` accepted `fastboot boot`, reached the host high-speed attach
boundary (`usb 1-9` at `11:39:43`), but the first Device Descriptor request
timed out with `-110` and no descriptor response bytes. The handset returned
as Android SuperSpeed `18d1:4ee7` at `11:40:09`; `1234:0001` did not appear.
The artifact SHA is:

```text
1220660.0  88e7415bcea43704bbbf4dd85a1a90b79eb284f4b77d24ae51b2644e6a7f7308
```

This is a negative EP0 retake, not a reason to restore the historical PHY
values or to claim wire-level failure. No analyzer/secure-debug capture was
available, and no flash or erase operation was attempted.

### 2026-09-05 Android resource-order retake

The next A/B used the Android msm ordering proven in local qpr1 source:
`DEPSTARTCFG`, transfer resources for all 32 hardware endpoints, then the two
EP0 `SETEPCONFIG` commands. It retained the corrected two-pair Bramble PHY,
source-exact pre-reset initialization, rail refresh, and eager SETUP arm.

Run `1226628.0` accepted `fastboot boot` and reached Fullerene HS attach on
`usb 1-9` at `11:43:55`. The first `GET_DESCRIPTOR(Device)` timed out with
`-110` at `11:44:01`; Android SuperSpeed `18d1:4ee7` returned at `11:44:21`,
and `1234:0001` did not appear. Artifact SHA:

```text
1226628.0  2e531e14de7f88041e752e3c779320049f067cbf520b5b735bc5474a635f2778
```

This source-order change preserves the same host-visible failure boundary, so
the remaining EP0 investigation should not treat endpoint resource ordering
as the immediate fix. No flash/erase operation was attempted.

### 2026-09-05 qpr1 device-core-reset retake

The next source-guided A/B replaced the handoff's DWC3 device reset with the
local qpr1 implementation of `dwc3_device_core_soft_reset()`: verbatim DCTL
`CSFTRST`, ten 1-ms polls, the 50-ms PHY synchronization delay, and the
post-reset doorbell clear. It retained the corrected Bramble HS-PHY pairs,
source-exact PHY-before-reset ordering, rail refresh, eager SETUP arm, and
no-SMMU USB2 path.

Run `1235572.0` accepted `fastboot boot` and reached Fullerene HS attach on
`usb 1-9` at `11:50:36`. The first `GET_DESCRIPTOR(Device)` timed out with
`-110` at `11:50:41`; Android SuperSpeed `18d1:4ee7` returned at `11:51:01`,
and `1234:0001` did not appear. Artifact SHA:

```text
1235572.0  e3934e80f295a27d8b6719183cd080eb117477d9823f4edd530b2e06af426627
```

The source-exact device-reset cadence and DCTL write semantics therefore do
not move the observed USB2 EP0 boundary. No flash/erase operation was
attempted.

### 2026-09-05 qpr1 sleep-mode ordering retake

The next source-guided A/B moved qpr1 `dwc3_dis_sleep_mode()` to the early
USB2 gadget-start boundary. It clears both
`GUSB2PHYCFG.ENBLSLPM` and `GUCTL1.L1_SUSP_THRLD_EN_FOR_HOST` immediately
before the gadget is started, while retaining the corrected Bramble HS-PHY
pairs, source-exact PHY-before-reset and device-core reset, rail refresh,
eager SETUP arm, and no-SMMU path.

Run `1245531.0` accepted `fastboot boot` and reached Fullerene HS attach on
`usb 1-9` at `11:57:57`. The first `GET_DESCRIPTOR(Device)` timed out with
`-110` at `11:58:02`; Android SuperSpeed `18d1:4ee7` returned at `11:58:23`,
and `1234:0001` did not appear. Artifact SHA:

```text
1245531.0  8299f78263054f1f654cfecafe50acda8ff042464ce47f09e63c53f5a79285e5
```

The qpr1 sleep-mode write ordering therefore does not move the observed USB2
EP0 boundary. No flash/erase operation was attempted.

### 2026-09-05 qpr1 DBM reset/enable retake

The next source-guided A/B enabled the qpr1 `dwc3_msm_block_reset(false)`
sequence on USB2: DBM soft reset for 1 ms, release, set
`QSCRATCH_GENERAL_CFG.DBM_EN`, then enable the DBM FIFO address and size
masks. It retained the corrected Bramble HS-PHY pairs, qpr1 sleep-mode
ordering, source-exact PHY-before-reset and device-core reset, eager SETUP
arm, and no-SMMU path.

Run `1249959.0` accepted `fastboot boot` and reached Fullerene HS attach on
`usb 1-9` at `12:00:39`. The first `GET_DESCRIPTOR(Device)` timed out with
`-110` at `12:00:44`; Android SuperSpeed `18d1:4ee7` returned at `12:01:05`,
and `1234:0001` did not appear. Artifact SHA:

```text
1249959.0  9d7fbbeb2407732773eb91f4dedc98180745b75771ae99f5b5a9d228ba03a1fd
```

The qpr1 DBM reset/enable sequence therefore does not move the observed USB2
EP0 boundary. No flash/erase operation was attempted.

### 2026-09-05 qpr1 UTMI post-reset-only retake

The next A/B removed the extra pre-reset UTMI-as-PIPE selection and kept only
the qpr1 post-reset selection. The corrected Bramble PHY, qpr1 DBM and
sleep-mode controls, source-exact device reset, eager SETUP arm, and no-SMMU
path were retained.

Run `1252401.0` accepted `fastboot boot` and reached Fullerene HS attach on
`usb 1-9` at `12:02:16`. The first `GET_DESCRIPTOR(Device)` timed out with
`-110` at `12:02:21`; Android SuperSpeed `18d1:4ee7` returned at `12:02:41`,
and `1234:0001` did not appear. Artifact SHA:

```text
1252401.0  deb4a41334c45455c893bf37fe7a98ccf0710bbb5ea2d260e1b19a7ff38ae7a2
```

The qpr1 UTMI mux ordering therefore does not move the observed USB2 EP0
boundary. No flash/erase operation was attempted.

### 2026-09-05 qpr1 event-ring default correction retake

The qpr1 source defines `DWC3_EVENT_BUFFERS_SIZE=4096` for the Android DWC3
event buffer. The normal linker-owned direct handoff had incorrectly kept the
historical ABL-observed `0xf0` size as its default. The implementation now
uses 4096 for that path and preserves `0xf0` only when explicitly reusing a
Fastboot-owned event buffer.

Run `1295369.0` accepted `fastboot boot`. The Harness classification missed
the attach, but the host kernel log independently records Fullerene HS attach
on `usb 1-9` at `12:34:17`, followed by the first Device Descriptor timeout
with `-110` at `12:34:23`. Android SuperSpeed `18d1:4ee7` returned after 38
seconds, and `1234:0001` did not appear. Artifact SHA:

```text
1295369.0  ebca0ebdc566934de488190722a8d1863918024f3ed9198389065e64552ab8c6
```

The qpr1 event-ring size is therefore corrected in the source but does not,
by itself, restore the EP0 response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 event-ring correction without pre-reset PHY retake

The next A/B kept the 4096-byte direct-handoff event ring and all of the
source-exact USB2, rail, DBM, sleep-mode, UTMI, device-reset, eager-SETUP, and
no-SMMU controls, but removed only `--hsphy-before-reset`.

Run `1301278.0` accepted `fastboot boot`. The host kernel logged Fullerene HS
attach on `usb 1-9` at `12:38:24`; the first Device Descriptor request timed
out with `-110` at `12:38:29`. Android SuperSpeed `18d1:4ee7` returned after
38 seconds, and `1234:0001` did not appear. Artifact SHA:

```text
1301278.0  467b85e3b91da0de5cb40f46848626c6435d40f0a285a3f3760962ec7eed9e4f
```

Removing only the pre-reset PHY ordering does not move the observed USB2 EP0
boundary. No flash/erase operation was attempted.

### 2026-09-05 historical PHY-pair control retake

The qpr1 source-confirmed two-pair default and the historical pair were then
separated as a physical control. Run `1313609.0` forced `0x63@0x6c` and
`0x85@0x70` through the explicit `--hsphy-legacy-fallback` flag, while
retaining the 4096-byte event ring, source-exact handoff, rail refresh, qpr1
USB2 reset/sleep/DBM/UTMI controls, eager SETUP arm, and no-SMMU path.

The run accepted `fastboot boot`; the host kernel logged Fullerene HS attach
on `usb 1-9` at `12:47:27`, then Device Descriptor `-110` at `12:47:32`.
Android SuperSpeed `18d1:4ee7` returned after 40 seconds, and `1234:0001`
did not appear. Artifact SHA:

```text
1313609.0  46a960bfe9805ce118d153562e7ca1fadac7839ba6b4f04085905ade4527d66c
```

The historical PHY pair does not explain the missing EP0 response and is not
restored as the default. No flash/erase operation was attempted.

### 2026-09-05 Android gadget-start restart with corrected event ring

The next test combined the qpr1-corrected 4096-byte direct-handoff event ring
with an explicit Android `__dwc3_gadget_start()`-style restart immediately
before Run/Stop. Run `1320367.0` used `--gadget-restart-at-runstop` with the
same qpr1 USB2 reset/sleep/DBM/UTMI controls, corrected PHY source, rail
refresh, eager SETUP, and no-SMMU path.

`fastboot boot` was accepted, but the host kernel log showed no Fullerene HS
attach. Android SuperSpeed `18d1:4ee7` returned at `12:52:08`; `1234:0001`
did not appear. Artifact SHA:

```text
1320367.0  46536500a20e22fe2f41de89f629c69e2465d017fe3b76a5f7609a5f8a8a4baa
```

This restart ordering is negative and should not be treated as evidence that
the qpr1 endpoint/resource sequence is wrong in isolation, because it also
changes the attach boundary. No flash/erase operation was attempted.

### 2026-09-05 qpr1 device-core reset at Run/Stop retake

The next test applied qpr1's `dwc3_gadget_pullup(true)` boundary more directly:
Run `1328769.0` performed the qpr1-style device-core soft reset immediately
before the USB2 Run/Stop transition, then rebuilt the 4096-byte event ring,
endpoint resources, EP0 configuration, and initial SETUP state. The corrected
PHY, rail refresh, qpr1 USB2 reset/sleep/DBM/UTMI controls, eager SETUP, and
no-SMMU path remained enabled.

`fastboot boot` was accepted, but the host kernel showed no Fullerene HS
attach. Android SuperSpeed `18d1:4ee7` returned at `12:58:22`; `1234:0001`
did not appear. Artifact SHA:

```text
1328769.0  77daaea6d1582eb3b198406026b34fbc6b1af0686bdc94f8d5cc86538decc55a
```

The Run/Stop-boundary device reset is therefore negative on this unit; it
does not restore the USB2 pull-up. No flash/erase operation was attempted.

### 2026-09-05 qpr1 separate SETUP buffer control retake

The next physical A/B was a non-source DMA-layout control. Run `1331990.0`
used a separate eight-byte SETUP object via `--ss-separate-setup-buffer`; the corrected
4096-byte event ring, qpr1 USB2 reset/sleep/DBM/UTMI controls, exact PHY path,
rail refresh, eager SETUP, and no-SMMU path were unchanged.

The local qpr1 source was then checked directly: `dwc3_ep0_out_start()` passes
`dwc->ep0_trb_addr` both to the CONTROL_SETUP TRB buffer pointer and to
STARTTRANSFER. Therefore qpr1 does not establish a separate `ctrl_req` layout;
the separate-buffer run remains only a physical negative control.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:00:10`, then Device Descriptor `-110` at `13:00:15`.
Android SuperSpeed `18d1:4ee7` returned at `13:00:36`; `1234:0001` did not
appear. Artifact SHA:

```text
1331990.0  ef44fb5c43522ab225c22c0b3e18e4f5e3d3233637ea282d63e9e0b97d4284c
```

The non-source separate-buffer control did not move the observed USB2 EP0
boundary. No flash/erase operation was attempted.

### 2026-09-05 qpr1 DCFG=SuperSpeed corrected retake

The historical DCFG policy result was re-tested after the qpr1 event-ring and
PHY corrections. Run `1335551.0` enabled `--dcfg-superspeed` on the otherwise
attach-reaching USB2 profile: qpr1 source-exact PHY, rail refresh, device
reset, sleep-mode, DBM, and UTMI controls, 4096-byte event ring, eager SETUP,
and no-SMMU.

`fastboot boot` was accepted, but no Fullerene HS attach appeared in the host
kernel log. Android SuperSpeed `18d1:4ee7` returned at `13:03:01` and
`1234:0001` did not appear. Artifact SHA:

```text
1335551.0  b7572d9a31f41e1f0cb4fc5707e6d4c06ac8a0de05ee6cf68f5fe929b115ffe9
```

The corrected retake confirms that forcing `DCFG=SuperSpeed` suppresses the
previously reproducible USB2 HS attach on this unit. The default remains
High-Speed for the USB2 path. No flash/erase operation was attempted.

### 2026-09-05 qpr1 initial EP0 MPS=512 corrected retake

The qpr1 gadget-start path initializes both EP0 directions at a 512-byte
maximum packet size and later changes a USB2 connection to 64 bytes at Connect
Done. Run `1338560.0` enabled `--ep0-initial-512` on the corrected USB2
profile, retaining the 4096-byte event ring, exact PHY, rail refresh, qpr1
reset/sleep/DBM/UTMI controls, eager SETUP, and no-SMMU.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:04:37`, then Device Descriptor `-110` at `13:04:43`.
Android SuperSpeed `18d1:4ee7` returned at `13:05:03`; `1234:0001` did not
appear. Artifact SHA:

```text
1338560.0  ada52aa3dd196a34531517e2a7748a3d44509cfb9c20fb660e15de7846481ba6
```

The source-defined initial 512-byte EP0 context does not restore the USB2
descriptor response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 eager SETUP corrected retake

The qpr1 source arms the initial EP0 OUT SETUP transfer before Run/Stop. Run
`1341149.0` omitted `--start-after-connect` to exercise that eager timing on
the corrected profile, retaining the 4096-byte event ring, exact PHY, rail
refresh, qpr1 reset/sleep/DBM/UTMI controls, and no-SMMU.

`fastboot boot` was accepted, but no Fullerene HS attach appeared in the host
kernel log. Android SuperSpeed `18d1:4ee7` returned at `13:06:46`; `1234:0001`
did not appear. Artifact SHA:

```text
1341149.0  5863f51d10bc6bd4879f65317bf1165a3098f444df9993d549d17b9305d12139
```

On this unit the qpr1 eager arm suppresses the attach, while the deferred
profile reaches HS but does not answer the first descriptor. No flash/erase
operation was attempted.

### 2026-09-05 qpr1 gadget restart with deferred SETUP retake

The prior restart A/B re-ran qpr1's event-ring, all transfer-resource, EP0
configuration, and endpoint-enable sequence immediately before Run/Stop, but
also armed SETUP before Run/Stop and suppressed the attach. Run `1351460.0`
kept that restart sequence and combined it with `--start-after-connect`, so
only the restart helper's SETUP `STARTTRANSFER` was deferred to the same
post-Run/Stop U0 arm window used by the attach-reaching direct profile.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:14:14`, then Device Descriptor `-110` at `13:14:20`.
Android SuperSpeed `18d1:4ee7` returned at `13:14:41`; `1234:0001` did not
appear. Artifact SHA:

```text
1351460.0  536d1e264f3dc380fa3262c64e43d6cb1109bc06b6948c2a1cd10e18c45c5c7e
```

Deferring only SETUP within the qpr1-style restart does not restore the first
descriptor response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 SUSPHY-through-EP0-setup audit correction

Run `1362071.0` was recorded as testing qpr1's `GUSB2PHYCFG.SUSPHY` hold,
but source review found that `--usb2-source-susphy` was only wired into the
non-direct `init_with_super_speed()` path. The actual direct Fastboot-reuse
path used by that run cleared `SUSPHY` before endpoint/resource construction,
so its HS attach and descriptor timeout are not evidence for the claimed
SUSPHY-through-EP0 A/B.

The original run did accept `fastboot boot`, reached HS attach on `usb 1-9` at
`13:21:25`, timed out the Device Descriptor with `-110` at `13:21:30`, and
returned Android SuperSpeed `18d1:4ee7` at `13:21:51`. Artifact SHA:

```text
1362071.0  66581f60292d5f8fea8a7f4e47db8f5c9adc3d93f1c2e692d9d9a56c53e2ab01
```

The implementation has now been corrected so the option applies to the
direct handoff; a valid physical retake remains pending. No flash/erase
operation was attempted.

### 2026-09-05 qpr1 Bramble DT confirmation for USB2 SUSPHY

The missing qpr1 Bramble base-DT source was checked in Google's separate
`kernel/msm-extra/devicetree` repository. Its Bramble `qcom/lito-usb.dtsi`
defines the DWC3 node with `maximum-speed = "super-speed"` and does not set
`snps,dis_u2_susphy_quirk`. The qpr1 DWC3 `dwc3_phy_setup()` therefore sets
`GUSB2PHYCFG.SUSPHY` for this revision, and the endpoint-command helper
temporarily clears and restores it around commands. This confirms that the
new direct-path `--usb2-source-susphy` wiring is source-compatible with
Bramble; it is not the same as the earlier invalid run, which never enabled
the option on the direct path.

The same source confirms the Bramble overlay's two HS-PHY override pairs
(`0x67@0x6c`, `0xc8@0x70`) used by the current source-exact PHY path. The
physical SUSPHY retake remains pending until the handset is physically
reconnected. Source: [qpr1 Bramble lito-usb.dtsi](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/heads/android-msm-bramble-4.19-android11-qpr1/qcom/lito-usb.dtsi).

### 2026-09-05 qpr1 DCTL-only USB2 Run/Stop control

Run `1367586.0` selected `--usb2-source-exact-runstop` and changed only the
DCTL `RUN_STOP` bit, avoiding the generic HIRD/APPL1RES/TRGTULST policy. The
qpr1 audit subsequently showed that `dwc3_gadget_run_stop(true)` also does not
use the current helper's USB2 SUSPHY/ENBLSLPM guard, so this earlier control
was not fully source-exact.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:24:38`, then Device Descriptor `-110` at `13:24:43`.
Android SuperSpeed `18d1:4ee7` returned at `13:25:04`; `1234:0001` did not
appear. Artifact SHA:

```text
1367586.0  d6ef00f7e09e3faafa7173eb3f21d270cfd5bbde6dcff30bc0f6afcf657efbe1
```

The DCTL-only control does not restore the first descriptor response. No
flash/erase operation was attempted; a true source-exact retake remains
separate.

### 2026-09-05 qpr1 source-exact USB2 Run/Stop retake

Run `1374991.0` retook the qpr1 `dwc3_gadget_run_stop(true)` boundary. Unlike
the earlier DCTL-only control, the USB2 path now omitted the helper's
`SUSPHY/ENBLSLPM` low-power guard and wrote only the DCTL `RUN_STOP` bit, while
retaining the attach-reaching qpr1 restart/deferred-SETUP profile.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:30:04`, then Device Descriptor `-110` at `13:30:09`.
Android SuperSpeed `18d1:4ee7` returned at `13:30:30`; `1234:0001` did not
appear. Artifact SHA:

```text
1374991.0  00d93af8db3b9eea8fa47052a3725ae74bdd8d2f68e911c5bd9e40984432b909
```

The qpr1 source-exact USB2 Run/Stop control does not restore the first
descriptor response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 USB2 SETUP signal-probe retake

Run `1380102.0` repeated the attach-reaching qpr1 USB2 profile with
`--signal-probe --signal-early-drop 3`. The host kernel logged Fullerene HS
attach on `usb 1-9` at `13:33:37`, then Device Descriptor `-110` at
`13:33:42`. Android SuperSpeed `18d1:4ee7` returned at `13:34:04`; no
`1234:0001` appeared, and no successful SETUP signal/drop was recorded.

```text
1380102.0  26158c06507e7a7d571b31b590bc9c4b6ed011c51c20a7f2b88c1dfc1bf4c517
```

The probe does not show SETUP reaching the software-visible signal boundary;
the first descriptor response remains absent. No flash/erase operation was
attempted.

### 2026-09-05 qpr1 core-reset + source-exact Run/Stop retake

Run `1387158.0` combined qpr1 `dwc3_gadget_pullup(true)`'s device-core soft
reset with the guard-free USB2 `RUN_STOP` write, retaining the attach-reaching
restart/deferred-SETUP profile.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:37:38`, then Device Descriptor `-110` at `13:37:44`.
Android SuperSpeed `18d1:4ee7` returned at `13:38:05`; `1234:0001` did not
appear. Artifact SHA:

```text
1387158.0  ab93d123c25938a0a24c04bdeea1bfb086255bcfe4d3803f0870ec927a26da0d
```

The qpr1 reset plus source-exact Run/Stop sequence does not restore the first
descriptor response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 source DMA-map retake

Run `1393139.0` repeated the same corrected qpr1 USB2 profile without
`--no-smmu`, using the existing verified Apps-SMMU identity map. The host
kernel logged Fullerene HS attach on `usb 1-9` at `13:41:42`, then Device
Descriptor `-110` at `13:41:47`. Android SuperSpeed `18d1:4ee7` returned at
`13:42:08`; `1234:0001` did not appear.

```text
1393139.0  1f6308cd26d1caba853ee854fc6b26ded9616bab983f79402bb429ebac4f4236
```

The DMA mapping mode does not restore the first descriptor response. No
flash/erase operation was attempted.

### 2026-09-05 qpr1 single gadget-start epoch retake

Run `1400851.0` skipped the initial EP0/resource construction and performed
the qpr1 gadget-start sequence only at the final Run/Stop boundary. It also
included qpr1's device-core reset and source-exact USB2 Run/Stop.

`fastboot boot` was accepted. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:47:18`, then Device Descriptor `-110` at `13:47:24`.
Android SuperSpeed `18d1:4ee7` returned at `13:47:44`; `1234:0001` did not
appear. Artifact SHA:

```text
1400851.0  373e1ae212c03f29c05ce8b5910dc129e9f3ace71d1000aa19567d430056590a
```

Avoiding the duplicate pre-Run/Stop EP0 epoch does not restore the first
descriptor response. No flash/erase operation was attempted.

### 2026-09-05 qpr1 single gadget-start eager SETUP retake

Run `1404254.0` used the single Run/Stop-boundary gadget-start epoch without
`--start-after-connect`, retaining qpr1's immediate EP0 SETUP arm. Fastboot
boot was accepted, but no Fullerene HS attach appeared; Android SuperSpeed
`18d1:4ee7` returned at `13:49:54`, and `1234:0001` did not appear.

```text
1404254.0  b1c9f052ec7a3369f786f834c78fbc50a5491b5d0290fde2f212d44079d0dca
```

The eager single-epoch start suppresses the attach on this device. No
flash/erase operation was attempted.

### 2026-09-05 qpr1 post-Run/Stop ungated SETUP retake

Run `1406791.0` retained the attach-reaching pre-Run/Stop setup and bypassed
the stale `DSTS.DEVCTRLHLT` check when arming deferred EP0 SETUP. The host
kernel logged Fullerene HS attach on `usb 1-9` at `13:51:13`, then Device
Descriptor `-110` at `13:51:18`. Android SuperSpeed `18d1:4ee7` returned at
`13:51:38`; `1234:0001` did not appear.

```text
1406791.0  eec8fc622e9c86f7403e1ac7ad35afe14a553dfa3c4aa2e447a42c111fabb6e0
```

Bypassing the stale halt gate does not restore the first descriptor response.
No flash/erase operation was attempted.

### 2026-09-05 qpr1 EP0 SETUP-TRB retire probe

Run `1409298.0` used `--signal-early-drop 2` on the attach-reaching
source-exact USB2 profile. The host kernel logged Fullerene HS attach on
`usb 1-9` at `13:52:56`, then Device Descriptor `-110` at `13:53:02`.
Android SuperSpeed `18d1:4ee7` returned at `13:53:22`; no `1234:0001` appeared,
and no TRB-retire signal/drop was observed.

```text
1409298.0  abec5129e95047acc35b70d6d488d9f4c2c80d1b6a3e3bff7447cb7f3b034a4f
```

The TRB-retire probe does not show the armed EP0 SETUP TRB being consumed.
No flash/erase operation was attempted.

### 2026-09-05 qpr1 EP0 STARTTRANSFER armstat readout

Run `1416857.0` repeated the attach-reaching source-exact USB2 profile with
`--signal-cmd-gate armstat`, attempting to time-encode whether the retained
EP0 SETUP `STARTTRANSFER` completed. The ADB-to-Fastboot transport was
validated (`device` → bootloader), and `fastboot boot` was accepted. Fullerene
HS attach appeared on `usb 1-9` at `13:58:38`; the Device Descriptor timed out
with `-110` at `13:58:43`; Android SuperSpeed `18d1:4ee7` returned at
`13:59:04`; `1234:0001` did not appear. No host-visible diagnostic boundary
separated the arm status, so this run records the unchanged pre-response
failure without claiming a raw internal status.

```text
1416857.0  2573ee7ec058f423f3f95d490b996953e648e162bbc60944c37b30144ef7724f
```

No analyzer, secure-debug capture, flash, or erase operation was used.

### 2026-09-05 qpr1 USB2 HS-PHY ref-clock-after-Run/Stop A/B

Run `1437348.0` added one source-guided differential after the final USB2
Run/Stop: re-enable Bramble's HS-PHY reference clock, matching qpr1's
`usb_phy_set_suspend(usb2, 0)` clock-resume operation. The ADB-to-Fastboot
transport was validated and `fastboot boot` was accepted. Fullerene HS attach
appeared on `usb 1-9` at `14:13:38`; the Device Descriptor timed out with
`-110` at `14:13:43`; Android SuperSpeed `18d1:4ee7` returned at `14:14:03`.
`1234:0001` did not appear.

```text
1437348.0  6a3723249d78dd14ae913372dd5d94a0f1a1e10750b0533ef20f3ddee47bf736
```

The qpr1-derived HS-PHY ref-clock re-enable does not restore the first
descriptor response. No analyzer, secure-debug capture, flash, or erase
operation was used.

### 2026-09-05 qpr1 USB2 HS-PHY ref-clock-after-GCTL A/B

Run `1449479.0` moved the same source-guided HS-PHY ref-clock re-enable to
the qpr1 `dwc3_core_init()` position immediately after DWC3 global-control
setup, before endpoint construction. The ADB-to-Fastboot transport was
validated and `fastboot boot` was accepted. Fullerene HS attach appeared on
`usb 1-9` at `14:22:21`; the Device Descriptor timed out with `-110` at
`14:22:26`; Android SuperSpeed `18d1:4ee7` returned at `14:22:47`.
`1234:0001` did not appear.

```text
1449479.0  55d82aef5058102c01a6cebdf7f1011d34b5bfde33ce061b29e58828e2cb3593
```

The source-order correction, closer to qpr1 than the prior after-Run/Stop
trial, still does not restore the first descriptor response. No analyzer,
secure-debug capture, flash, or erase operation was used.

### 2026-09-05 qpr1 DWC31 KEEP_CONNECT-clear Run/Stop A/B

Run `1462231.0` corrected the USB2 source-exact Run/Stop write for Bramble's
DWC_usb31 (`0x3331`) core: qpr1 clears `DCTL.KEEP_CONNECT` on the start-side
revision-gated path before asserting `DCTL.RUN_STOP`. The ADB-to-Fastboot
transport was validated and `fastboot boot` was accepted. Fullerene HS attach
appeared on `usb 1-9` at `14:31:24`; the Device Descriptor timed out with
`-110` at `14:31:30`; Android SuperSpeed `18d1:4ee7` returned at `14:31:51`.
`1234:0001` did not appear.

```text
1462231.0  d33332f6fa990707c0f5ead6629de0af1977dfaba144fe581ce1712ae579bee6
```

The qpr1 DWC31 `KEEP_CONNECT` correction does not restore the first
descriptor response. No analyzer, secure-debug capture, flash, or erase
operation was used.

### 2026-09-05 qpr1 gadget-start DCFG-speed-after-restart A/B

Run `1468317.0` kept the initial EP0/resource epoch but moved the final
`DCFG.SPEED` write from before the qpr1-style gadget restart to after
endpoint/resource/SETUP publication, matching qpr1's `__dwc3_gadget_start()`
ordering. The ADB-to-Fastboot transport was validated and `fastboot boot` was
accepted. Fullerene HS attach appeared on `usb 1-9` at `14:35:03`; the Device
Descriptor timed out with `-110` at `14:35:09`; Android SuperSpeed
`18d1:4ee7` returned at `14:35:30`. `1234:0001` did not appear.

```text
1468317.0  31e4c05e96911ae73e25dc096242bc76544b65b88774dde02c33e768a9142271
```

The qpr1 gadget-start speed-write ordering does not restore the first
descriptor response. No analyzer, secure-debug capture, flash, or erase
operation was used.

### 2026-09-05 qpr1 DWC31 KEEP_CONNECT-clear SuperSpeed A/B

Run `1475255.0` applied the DWC31 qpr1 start-side `DCTL.KEEP_CONNECT` clear
to the full SuperSpeed handoff with the source-exact `DCTL.RUN_STOP` write.
The ADB-to-Fastboot transport was validated and `fastboot boot` was accepted.
Fullerene's USB2 companion appeared at `14:40:00`, but no Fullerene
descriptor completed; Android SuperSpeed `18d1:4ee7` returned at `14:40:26`.
`1234:0001` did not appear.

```text
1475255.0  b2cd7b8a87ac8e49b95a8bbd6a5f8ddc7f0d54319551464e3ecaa660a4bf36e0
```

The same qpr1 DWC31 correction is negative on the SuperSpeed path as well.
No analyzer, secure-debug capture, flash, or erase operation was used.

### 2026-09-05 qpr1 device-core-reset + DWC31 KEEP_CONNECT-clear retake

Run `1478398.0` retook the qpr1-like USB2 pull-up boundary with the device
core soft reset immediately before the final gadget restart, now including
the corrected DWC31 `DCTL.KEEP_CONNECT` clear before `DCTL.RUN_STOP`. The
ADB-to-Fastboot transport was validated and `fastboot boot` was accepted.
Fullerene HS attach appeared on `usb 1-9` at `14:42:16`; the Device Descriptor
timed out with `-110` at `14:42:21`; Android SuperSpeed `18d1:4ee7` returned
at `14:42:42`. `1234:0001` did not appear.

```text
1478398.0  8dcf320b6bf0f956defdb1595a144902ee44c7d41f9b45f9aa5865f6e83aaa63
```

Adding qpr1's device-core reset to the corrected DWC31 Run/Stop path does not
restore the first descriptor response. No analyzer, secure-debug capture,
flash, or erase operation was used.

### 2026-09-05 qpr1 single gadget-start epoch + DWC31 KEEP_CONNECT-clear retake

Run `1482934.0` removed the duplicate initial EP0/resource epoch and retained
only the qpr1-style gadget-start sequence at the final USB2 Run/Stop boundary,
with the corrected DWC31 `DCTL.KEEP_CONNECT` clear. The ADB-to-Fastboot
transport was validated and `fastboot boot` was accepted. Fullerene HS attach
appeared on `usb 1-9` at `14:45:35`; the Device Descriptor timed out with
`-110` at `14:45:41`; Android SuperSpeed `18d1:4ee7` returned at `14:46:01`.
`1234:0001` did not appear.

```text
1482934.0  8cd3934cbb91e5e801212fe69b7aa4342ae4124c3536bdc2e6ab468c58511ef1
```

The single-epoch qpr1 start does not restore the first descriptor response.
No analyzer, secure-debug capture, flash, or erase operation was used.

### 2026-09-05 qpr1 DCFG=SuperSpeed with single gadget-start epoch retake

Run `1501319.0` combined the qpr1-style single gadget-start epoch, device-core
reset at the final USB2 Run/Stop boundary, DWC31 `DCTL.KEEP_CONNECT` clear,
and qpr1's Bramble maximum-speed `DCFG=SuperSpeed` value. ADB-to-Fastboot
transport, QEMU preflight, boot-image audit, and `fastboot boot` all passed.
After Fastboot disconnected at `14:59:03`, neither Fullerene HS attach nor
`1234:0001` appeared; the host saw no Android/Fastboot USB recovery during the
150-second wait and the harness timed out. The phone is currently absent from
both `adb devices` and `fastboot devices`, so this is a stronger negative
failure mode than the prior HS descriptor timeout. No flash, erase, analyzer,
or secure-debug operation was used.

```text
1501319.0  68b2fa3bd96b3274f59d146db3f0236412dc5b043eeb64aaec9b829aef8bd3fa
```

The maximum-speed `DCFG` retake does not enumerate and can suppress even the
previous HS attach; do not treat it as a candidate default.

### 2026-09-05 qpr1 DEVTEN source-exact A/B prepared

The official qpr1 `dwc3_gadget_enable_irq()` enables a narrower device-event
mask than the current Fullerene polling/debug mask: vendor, overflow,
command-complete, erratic-error, wakeup, connect-done, USB reset, and
disconnect, with link-status-change added only for cores older than 2.30a.
The local direct Fastboot-reuse path now exposes this as
`--usb2-source-exact-devten`, at both the initial DEVTEN publication and the
Run/Stop-boundary gadget restart. The default mask and the separate observed
Factory ABL `--abl-devten` differential are unchanged. Baseline and
source-exact kernel builds, the `bramble-usb` CLI build, and help-text
propagation pass. No physical result is claimed yet because the Pixel remains
absent from `adb`, `fastboot`, and `lsusb`; physical retake is pending.
The generated preflight artifact is
`tmp/fullerene-bramble-source-devten-check.img` with SHA-256
`33d72f14b4850a0f78ff49a34c0759973edb749715f6d9b8b1a73ac5a44d21d2`.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 physical USB availability recheck after DEVTEN A/B preparation

The host was rechecked after the source-exact DEVTEN preflight. `adb devices -l`
and `fastboot devices` remain empty, and `lsusb` shows only the root hubs,
webcam, receiver, and Bluetooth adapter; neither Pixel `18d1:*` nor Fullerene
`1234:0001` is present. No loop was launched and no new hardware result is
claimed. The preflight image remains ready at
`tmp/fullerene-bramble-source-devten-check.img`; when the phone reappears, the
next run will omit the negative `--dcfg-superspeed` control and add both
`--usb2-source-susphy` and `--usb2-source-exact-devten` to the last
HS-attach-reaching profile. No flash, erase, analyzer, or secure-debug
operation was used.

### 2026-09-05 HS-attach profile with source-exact DEVTEN candidate prepared

The full next physical profile was built and passed the QEMU Bramble
preflight and boot audit. It retains the last HS-attach-reaching USB2 shape,
adds the now-correct direct-path `--usb2-source-susphy` and
`--usb2-source-exact-devten`, and includes the qpr1 device-reset, sleep-mode,
DBM, source-exact Run/Stop, single gadget-start, and Run/Stop core-reset
differentials. The negative `--dcfg-superspeed` control is intentionally
omitted. Candidate image:
`tmp/fullerene-bramble-source-devten-hs-check.img`, SHA-256
`7eaf48ebfbfc7e87ab7a47784206b441e40c7140454249568284aeeb1c93b660`.
Kernel tests, `bramble-usb` build, and `git diff --check` pass. The Pixel is
still absent, so no `fastboot boot` or hardware result is claimed.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 USB2 per-command PHY guard correction

The qpr1 `dwc3_send_gadget_ep_cmd()` source decides whether to clear
`GUSB2PHYCFG.SUSPHY` and `ENBLSLPM` from the gadget's pre-connect speed state
(`UNKNOWN`/USB2), not from the inherited `DSTS.CONNECTSPD` field. A Fastboot
handoff can leave that field reporting the old SuperSpeed session, which could
skip the required guard in the direct USB2 reuse path. The existing internal
cmd-guard cfg is now exposed as `--usb2-source-exact-cmd-guard` and is limited
to the direct Bramble USB2 probe. The source-exact candidate is rebuilt and
ready at `tmp/fullerene-bramble-source-devten-hs-guard-check.img`, SHA-256
`e88a9e536fd4c35c8504a74dd34f0e8362fa8e4103d34ea960c5eae76114c8ce`.
The qpr1 source reference remains the local `tmp/qpr1-msm` checkout and
Google's Bramble device-tree source. No physical result is claimed while the
Pixel is absent.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 physical USB availability recheck after cmd-guard candidate

At `15:54 JST`, the host still reported no Pixel in `adb devices -l` or
`fastboot devices`, and `lsusb` contained neither the Android `18d1:*` device
nor Fullerene `1234:0001`. The qpr1 source-exact cmd-guard candidate remains
ready at `tmp/fullerene-bramble-source-devten-hs-guard-check.img` with SHA-256
`e88a9e536fd4c35c8504a74dd34f0e8362fa8e4103d34ea960c5eae76114c8ce`. The
software monitor was kept running and now accepts either the expected ADB
state or the expected Fastboot serial as the next safe trigger; no physical
boot attempt is claimed. No flash, erase, analyzer, or secure-debug operation
was used.

### 2026-09-05 qpr1 reset-timing audit correction

An initial comparison appeared to find a source-exact reset timing mismatch.
Reading the distinct qpr1 functions resolves it: `gadget.c`'s
`dwc3_device_core_soft_reset()` uses `retries = 10` with
`usleep_range(1000, 1100)`, followed by the DWC_usb31 50-ms PHY-sync delay;
the current source-exact path intentionally matches that. The separate
`core.c` `dwc3_core_soft_reset()` uses `retries = 1000` with `udelay(1)`, but
that is the probe-time PHY/core reset and is not the gadget pull-up reset used
by this direct handoff. No code change was made from the false alarm. The
source distinction is recorded here so the docs do not preserve the mistaken
claim. No physical trial was run because the Pixel remains absent.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 EP0 configuration/resource-order audit

The qpr1 `dwc3_gadget_start_config()` / `dwc3_gadget_set_ep_config()` comparison
found no new defect in the active physical candidate's Run/Stop restart path:
it performs `DEPSTARTCFG`, allocates one transfer resource for each endpoint
object in its 32-slot `dwc->eps[]` array, then configures EP0 OUT/IN with
control type, the 512-byte initial packet state, transfer-complete/not-ready
notifications, and physical endpoint numbers. Although `dwc3_core_num_eps()`
temporarily derives a count from `GHWPARAMS3`, qpr1's `dwc3_gadget_init()` then
sets `dwc->num_eps = DWC3_ENDPOINTS_NUM` and allocates all 32 objects; the
gadget-start loop skips only null objects. The EP0 TRB,
`STARTTRANSFER` PAR0/PAR1 mapping, and qpr1 event-ring reset also match. The
generic legacy direct path still has a historical per-endpoint
config-then-resource A/B, but the candidate explicitly uses
`--gadget-start-only-at-runstop --gadget-restart-at-runstop`, so that path is
not used for this retake; the prior Android resource-order retake `1226628.0`
was already negative. The DWC3 simulation (3 tests) and USB protocol tests
(12 tests) passed. The Pixel remains absent, so no hardware result and no
`1234:0001` claim is made.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 endpoint-count correction and candidate rebuild

The EP0/resource-order audit found one implementation mismatch after reading
qpr1's complete endpoint initialization path: `dwc3_core_num_eps()` derives a
temporary `dwc->num_eps` from `GHWPARAMS3`, but `dwc3_gadget_init()` then sets
`dwc->num_eps = DWC3_ENDPOINTS_NUM` and allocates all 32 endpoint objects.
`dwc3_gadget_start_config()` therefore walks all 32 slots and skips only null
objects. The direct handoff now mirrors that actual qpr1 gadget-start range;
the earlier GHWPARAMS3-only implementation was not source-correct.

The source-exact kernel check, DWC3 simulation (3 tests), USB protocol tests
(12 tests), QEMU Bramble preflight, Bramble boot audit, and `git diff --check`
passed after the correction. The regenerated candidate is
`tmp/fullerene-bramble-source-devten-hs-guard-check.img` with SHA-256
`22514441e2977e8e8e0a43fd7568bca08c924d3e8f175d6db0cca1c521107d16`.
The Pixel remains absent, so no physical result and no `1234:0001` claim is
made. No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DALEPENA publication-order correction

The qpr1 `__dwc3_gadget_ep_enable()` comparison found that each EP0
`SETEPCONFIG` is followed immediately by publication of that physical
endpoint's `DALEPENA` bit before the opposite EP0 direction is configured.
The Run/Stop restart path had configured both directions and published
`DALEPENA=0b11` only afterward. It now publishes EP0-OUT bit 0, configures
EP0-IN, and publishes bit 1 in the qpr1 order. No claim about physical
enumeration is made: the Pixel remains absent and the candidate must be
regenerated and tested when it returns.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DALEPENA correction candidate rebuild

After the DALEPENA publication-order correction, the source-exact kernel
check, QEMU Bramble preflight, Bramble boot audit, and `git diff --check`
passed again. The regenerated candidate is
`tmp/fullerene-bramble-source-devten-hs-guard-check.img` with SHA-256
`e592e1feaeb52b152a03e0e458dd7164395b7e4b3303ccf7e4997281692ad509`.
The Pixel is still absent, so no physical boot and no `1234:0001` result is
claimed. The software monitor remains active for ADB or Fastboot return.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DISSCRAMBLE global-control correction

The qpr1 `dwc3_core_setup_global_control()` path clears both `SCALEDOWN` and
`DISSCRAMBLE` while selecting device mode and honoring Bramble's
`snps,disable-clk-gating` policy. The active direct Fastboot-reuse block was
already clearing `SCALEDOWN` and setting `DSBLCLKGTNG`, but it left the
bootloader's `DISSCRAMBLE` state untouched. The active candidate now clears
`DISSCRAMBLE` at the same post-reset global-control boundary. This is a
source-based correction; the Pixel is absent, so no physical enumeration
result is claimed until a retake is possible.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 MMIO ordering correction for initial USB2 handoff

The active direct path's initial bare-handoff prefix still used raw
`write_volatile` for GUSB2PHYCFG, GUSB3PIPECTL, GCTL, DCFG, and DALEPENA,
whereas the qpr1 Qualcomm/DWC3 code reaches those device registers through
Linux `writel()` and readback boundaries. The candidate now uses the common
MMIO write helper (which includes the AArch64 store barrier) and an immediate
readback for each of those controller writes; the QSCRATCH general-config
write now uses the existing readback helper as well. This correction is
limited to the active initial USB2 handoff prefix and does not claim a
physical result while the Pixel is absent.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DBM iowrite ordering correction

The qpr1 DBM implementation reaches soft-reset, QSCRATCH DBM enable, and
FIFO-enable registers through `iowrite32()`. On AArch64 that primitive applies
`__iowmb()` before each store. The active candidate's DBM helper had used raw
`write_volatile` for those reset/enable stores; it now adds the equivalent
`dsb st` boundary after each store. This is a source-derived correction only;
no physical result is claimed while the Pixel is absent.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DBM-order candidate rebuild

After the DBM `iowrite32()` ordering correction, the physical candidate was
rebuilt as `tmp/fullerene-bramble-source-devten-hs-guard-check.img`, SHA-256
`edd0ed5525a198014166d46436b0c54f110ab86e6ae4e45a1fb4601837cea2fa`.
Kernel checks, DWC3 simulation (3 tests), USB protocol tests (12 tests), QEMU
Bramble preflight, boot audit, and `git diff --check` passed.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 physical USB availability recheck after DBM-order rebuild

At `16:47:18 JST`, ADB and Fastboot were empty and host `lsusb` showed neither
the Pixel nor Fullerene `1234:0001`; no physical boot was possible. Monitor
session `63426` remains active for reconnection.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DBM-order physical retake (Run 1656178.0)

The reconnect monitor detected Fastboot and launched the DBM-order candidate
with the source-exact DEVTEN/cmd-guard, USB2 SUSPHY, qpr1 Run/Stop, gadget
restart, and Run/Stop core-reset controls. `fastboot boot` was accepted for
artifact `tmp/fullerene-bramble-loop.1656178.0/fullerene-bramble-boot.img`,
SHA-256
`a3c1a4d575596e987d43e30fce266a455decde9415b777df85797de3a207cad6`.

The probe did not enumerate `1234:0001`. Host kernel evidence shows the
Fastboot device disconnecting at `16:49:49 JST`, a new high-speed device on
`1-9` at `16:50:04`, and `device descriptor read/64, error -110` at
`16:50:09`. The handset returned after 41 seconds and reappeared as stock
Android `18d1:4ee7` over SuperSpeed at `16:50:30`; the boot reason was
`watchdog`. This retake confirms HS attach but no EP0 descriptor response; it
does not distinguish a secure/electrical fault without an analyzer or
secure-side capture.

The loop exited with the expected enumeration-timeout failure after recording
the Android fallback. No flash, erase, analyzer, or secure-debug operation was
used.

### 2026-09-05 SuperSpeed full-path physical retake (Run 1693171.0)

The full SuperSpeed path was retested on lane A with the current source-derived
QMP clock/power/autonomous controls, DBM reset/enable ordering, LFPS and
`UX_EXIT_PX` controls, USB2 sleep/PHY controls, old-session VBUS/endpoint/GSI
cleanup, and source-exact DWC3 Run/Stop. `fastboot boot` was accepted for
artifact `tmp/fullerene-bramble-loop.1693171.0/fullerene-bramble-boot.img`,
SHA-256 `e7350ef61795a36a6755a51d4c33968a7fc18896d986fef8c30ee633fb1475f2`.

The host kernel saw a new high-speed USB device on `usb 1-9` at `17:17:40 JST`,
but no Fullerene descriptor completed during the observation window. Stock
Android `18d1:4ee7` over SuperSpeed returned at `17:18:06`; `boot-reason.txt`
was `watchdog`. No Fullerene `1234:0001` appeared. This retake did not turn
the existing HS fallback into a host-visible SuperSpeed identity or EP0 data.

The loop exited with the expected enumeration-timeout failure after recording
the Android fallback. No flash, erase, analyzer, or secure-debug operation was
used.

### 2026-09-05 qpr1 eager SETUP retake (Run 1665839.0)

The qpr1-style eager SETUP control was repeated with the latest DBM/MMIO/
DEVTEN corrections and with `--start-after-connect` omitted. The candidate
was built and accepted by `fastboot boot` as
`tmp/fullerene-bramble-loop.1665839.0/fullerene-bramble-boot.img`, SHA-256
`ebd6a168a9a368a6b026e67ce0762a43a14c0c85ab779c4a570f39d4ed5dcc9b`.

No Fullerene HS attach or `1234:0001` appeared. The handset returned as stock
Android `18d1:4ee7` over SuperSpeed at `16:57:59`; `boot-reason.txt` was
`watchdog`. This confirms the current unit's split: eager pre-Run/Stop SETUP
arming suppresses the HS attach, while the attach-reaching deferred-arm path
still reaches HS but receives no EP0 descriptor response. This run did not
include the separate `--hsphy-before-reset` ordering control.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 HS-PHY-before-reset physical retake (Run 1672355.0)

The attach-reaching deferred-arm profile was repeated with
`--hsphy-before-reset`, retaining the source-exact HS-PHY, rail-refresh, USB2
SUSPHY/cmd-guard/device-reset/sleep-mode/DBM controls, source-exact Run/Stop
and DEVTEN controls, gadget restart, and Run/Stop core-reset controls.
`fastboot boot` was accepted for artifact
`tmp/fullerene-bramble-loop.1672355.0/fullerene-bramble-boot.img`, SHA-256
`40f30b543f308a0f179c9b2964602a7f817a6ae72b74059520e2c3073266a613`.

The host kernel saw Fullerene high-speed attach on `usb 1-9` at `17:01:49 JST`
and `device descriptor read/64, error -110` at `17:01:55`. Stock Android
`18d1:4ee7` over SuperSpeed returned at `17:02:16`; `boot-reason.txt` was
`watchdog`. No Fullerene `1234:0001` appeared. Moving the source-exact HS-PHY
reset/init before the DWC3 device-core reset did not move the boundary.

The loop exited with the expected enumeration-timeout failure after recording
the Android fallback. No flash, erase, analyzer, or secure-debug operation was
used.

### 2026-09-05 qpr1 MMIO-order candidate rebuild

The initial USB2 handoff MMIO-order correction was rebuilt as
`tmp/fullerene-bramble-source-devten-hs-guard-check.img`, SHA-256
`d293777b013bc6da63108b7e5252ffd6a2dba2ca44e6342f1600d35efebcdbf9`.
Kernel checks, DWC3 simulation (3 tests), USB protocol tests (12 tests),
QEMU Bramble preflight, boot audit, and `git diff --check` passed.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 Full-Speed bypass physical retake (Run 1687095.0)

The attach-reaching deferred-arm profile was repeated with `--dcfg-fullspeed`,
retaining the current qpr1-derived PHY, DBM, endpoint, DEVTEN, Run/Stop, and
MMIO controls. `fastboot boot` was accepted for artifact
`tmp/fullerene-bramble-loop.1687095.0/fullerene-bramble-boot.img`, SHA-256
`d331efffa9dabb4fad1cc2e48c43219311c7891ffb7b5f623f60c0369464c5e4`.

The host kernel saw a new full-speed USB device on `usb 1-9` at `17:13:04 JST`,
but no device descriptor completed during the observation window. Stock Android
`18d1:4ee7` over SuperSpeed returned at `17:13:30`; `boot-reason.txt` was
`watchdog`. No Fullerene `1234:0001` appeared. The speed override therefore
changed the host-observed attach from the prior high-speed result, but did not
move the no-EP0-data boundary.

The loop exited with the expected enumeration-timeout failure after recording
the Android fallback. No flash, erase, analyzer, or secure-debug operation was
used.

### 2026-09-05 physical USB availability recheck after MMIO-order rebuild

At `16:43:55 JST`, ADB and Fastboot were empty and host `lsusb` showed neither
the Pixel nor Fullerene `1234:0001`. No physical boot was possible. Monitor
session `63426` remains active and will use the rebuilt candidate after
reconnection.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 qpr1 DISSCRAMBLE correction candidate rebuild

The direct-path qpr1 global-control correction was rebuilt as
`tmp/fullerene-bramble-source-devten-hs-guard-check.img`, SHA-256
`9f7ab3932870b4560957a1e82fc2035982c3cbc48748818254d6deb945735791`.
The source-exact kernel check, DWC3 simulation (3 tests), USB protocol tests
(12 tests), QEMU Bramble preflight, Bramble boot audit, and `git diff --check`
passed. This remains a software/preflight result until the Pixel returns.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 physical USB availability recheck after DISSCRAMBLE rebuild

At `16:34:35 JST`, `adb devices -l`, `fastboot devices -l`, and host `lsusb`
showed neither the Pixel nor Fullerene `1234:0001`. No physical boot was run.
Monitor session `63426` remains active and will launch the rebuilt candidate
when the expected ADB/Fastboot transport returns.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 final physical USB availability recheck for current candidate

At `16:39:40 JST`, the host still had no ADB state, no Fastboot device, and no
USB device matching either the Pixel vendor ID or Fullerene `1234:0001`. This
is an availability check, not a hardware trial; no `fastboot boot` was
possible. Monitor session `63426` remains active for reconnection.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 physical USB availability recheck after DALEPENA rebuild

At `16:23:16 JST`, `adb devices -l` and `fastboot devices -l` were empty, and
host `lsusb` contained neither the Pixel Android vendor device nor Fullerene
`1234:0001`. No physical boot was run. The active monitor session remains
alive and will trigger the corrected candidate
`tmp/fullerene-bramble-source-devten-hs-guard-check.img` (SHA-256
`e592e1feaeb52b152a03e0e458dd7164395b7e4b3303ccf7e4997281692ad509`) when
the expected ADB/Fastboot transport returns.

No flash, erase, analyzer, or secure-debug operation was used.

### 2026-09-05 USB2 full DWC3 core-reset physical A/B (Run 1730678.0)

The latest safe `fastboot boot` retake added a broader USB2 DWC3 reset sequence:
the qpr1 device-core reset followed by DCTL `CSFTRST`, GCTL
`CORESOFTRESET`, and the USB2 PHY-facing soft reset. Build, QEMU/preflight,
boot audit, and transport recovery all passed. The accepted artifact was
`tmp/fullerene-bramble-loop.1730678.0/fullerene-bramble-boot.img` with
SHA-256
`92e082e8250e5fe067ed545effe7afdf730b4841855fd9ff9f6a71df53ecc772`.

On the physical Bramble, host USB reached high-speed attach at `17:44:49 JST`
(`usb 1-9`, device 75), but the first device-descriptor request timed out at
`17:44:55` with `error -110`. Stock Android `18d1:4ee7` returned at
`17:45:16`; `boot-reason=watchdog`. Host `lsusb`/ADB/Fastboot never showed
Fullerene `1234:0001`, so the goal remains unreached. The broader core reset
did not move the established HS-attach/no-descriptor boundary.

The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-full-core-reset.1u.bin`, SHA-256
`f3e2d3e7059cbdb6fa255b266d22ed3223636a261f502ffe04a244d72afabbc8`.
This is software-side host evidence only; no analyzer, flash, erase, or
secure-debug operation was used.

### 2026-09-05 qpr1 RAMCLK reset-value physical A/B (Run 1754069.0)

The next source-guided retake corrected a handoff policy that conflicted with
qpr1: `dwc3_gadget_conndone_interrupt()` documents that `GCTL.RAMCLKSEL` is
reset to zero after USB reset and that the driver intentionally uses that reset
value. The direct path now leaves the field untouched by default; the old
captured-Fastboot-value write is retained only as a named diagnostic cfg.
Build, QEMU/preflight, boot audit, and transport recovery all passed. The
accepted artifact was
`tmp/fullerene-bramble-loop.1754069.0/fullerene-bramble-boot.img` with
SHA-256
`8eb0ad609a45e9f95d92e1afde954628e13e3f14ad74da3c3cd3b8c64ce74705`.

On the physical Bramble, host USB reached high-speed attach at `18:02:12 JST`
(`usb 1-9`, device 76), but the first device-descriptor request timed out at
`18:02:17` with `error -110`. Stock Android `18d1:4ee7` returned at
`18:02:38`; `boot-reason=watchdog`. Host `lsusb`/ADB/Fastboot never showed
Fullerene `1234:0001`, so the goal remains unreached. This source-correct
RAMCLK change did not move the established HS-attach/no-descriptor boundary.

The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-ramclk-reset-default.1u.bin`, SHA-256
`f485e7e17b9497563400b5295022ec251af822d918ccd618ca174d4a594d74fe`.
This is software-side host evidence only; no analyzer, flash, erase, or
secure-debug operation was used.

The next A/B is to move qpr1's `__dwc3_gadget_start()` endpoint/event
publication into the final Run/Stop boundary while retaining the source-correct
RAMCLK behavior. This remains software-only and does not assume an analyzer.

### 2026-09-05 qpr1 gadget-start-only-at-Run/Stop physical A/B (Run 1761853.0)

This A/B moved the qpr1 `__dwc3_gadget_start()` work to the final Run/Stop
boundary and used the corrected 32-slot transfer-resource range. The accepted
artifact was
`tmp/fullerene-bramble-loop.1761853.0/fullerene-bramble-boot.img` with
SHA-256
`0d153b65296a91b23d38fe39de892c547570b0ddc882665f19027256228800fc`.
Build, QEMU/preflight, boot audit, and `fastboot boot` acceptance passed.

The physical Bramble's Fastboot session disconnected at `18:07:52 JST`, but
the host saw no Fullerene `usb 1-9` attach and submitted no Fullerene device
descriptor request. Stock Android `18d1:4ee7` returned at `18:08:33`, with
`boot-reason=watchdog`; no `1234:0001` appeared. This source-order placement
removes the previously reachable HS attach and is negative for this handoff
boundary. The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-gadget-start-runstop.1u.bin`, SHA-256
`ff6adfcef5396382a5680f7a2c38872f235f5b10719cf97f927f595a8c5414cf`.
No analyzer, flash, erase, or secure-debug operation was used.

The qpr1 endpoint audit is corrected in this document: the gadget init path
allocates all 32 endpoint objects even though the earlier core-probe count is
derived from `GHWPARAMS3`; the gadget-start resource loop therefore covers 32
slots. The next hardware A/B returns to the HS-attach-reaching path and tests
only that corrected resource range.

### 2026-09-05 qpr1 32-slot resource-range physical A/B (Run 1770789.0)

The corrected direct path returned to the HS-attach-reaching profile and
changed the transfer-resource loop to qpr1's 32-slot gadget-start range. The
source-correct RAMCLK reset behavior, full USB2 core reset, qpr1 USB2 source
guards, and no-SMMU profile were retained. Build, QEMU/preflight, boot audit,
and `fastboot boot` acceptance passed. The accepted artifact was
`tmp/fullerene-bramble-loop.1770789.0/fullerene-bramble-boot.img` with
SHA-256
`a4ea3ce2a0c9e7e210816cdc4a3f5dbcf03fdaf1e730357a9a8ec631d655f5ff`.

On the physical Bramble, host USB again reached high-speed attach at
`18:15:03 JST` (`usb 1-9`, device 77), but the first device-descriptor request
timed out at `18:15:09` with `error -110`. Stock Android `18d1:4ee7` returned
after the watchdog recovery; no Fullerene `1234:0001` appeared. Thus the
32-slot correction restores HS attach but does not cross the EP0 descriptor
boundary. The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-gadget-resource-32.1u.bin`, SHA-256
`0953d609c25cac80d99f61a98b3502a81dc7cde57452ec21acc6dd99754cbdc6`.
This is software-side host evidence only; no analyzer, flash, erase, or
secure-debug operation was used.

The next source audit targets the qpr1 `dma_alloc_coherent()` mapping contract:
on this non-coherent arm64 path Linux remaps coherent allocations as
write-combine/Normal-NC memory. The standalone bare probe does not enable the
normal Fullerene MMU path, so its `.usb_dma` accesses use the inherited
bootloader regime; this is an explicit physical A/B, not a claim that the
memory type is already proven to be the root cause.

### 2026-09-05 DMA mapping A/B attempt audit (Run 1783141.0)

The attempted Normal-NC change was not exercised by the physical artifact.
The source change was made in the normal Fullerene bootstrap's `mmu.rs`, but
the independently linked USB probe intentionally does not include or enable
that MMU path. The built Bramble artifact therefore remained byte-for-byte
identical to Run 1770789.0 (SHA-256
`a4ea3ce2a0c9e7e210816cdc4a3f5dbcf03fdaf1e730357a9a8ec631d655f5ff`). This
is recorded as an invalid DMA-memory A/B, not as evidence for or against
Normal-NC.

The physical attempt itself reproduced the prior boundary: HS attach at
`18:24:30 JST` (`usb 1-9`, device 78), descriptor timeout at `18:24:35` with
`error -110`, and stock Android `18d1:4ee7` at `18:24:56`; no `1234:0001`.
The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-dma-normal-nc.1u.bin`, SHA-256
`334f0fe675f4a999c861606af835c050de47a6068c6102958fb03084f0d4bd81`.
No analyzer, flash, erase, or secure-debug operation was used.

The next A/B forces cache clean/invalidate operations for the standalone
probe's `.usb_dma` window, directly exercising the path that the previous
Normal-NC attempt did not reach.

### 2026-09-05 standalone-probe cache-maintenance physical A/B (Run 1805518.0)

This A/B forced `dc cvac`/`dc ivac` cache clean/invalidate operations for the
standalone probe's `.usb_dma` window while retaining the qpr1 32-slot,
HS-attach-reaching profile. The accepted artifact was
`tmp/fullerene-bramble-loop.1805518.0/fullerene-bramble-boot.img`, SHA-256
`27ed8f05045ce11b1d1da5fc5847417e8fef0be758cb517fb0ce41f9c0804e44`.

The physical Bramble again reached HS attach at `18:41:53 JST` (`usb 1-9`,
device 79), then timed out on the first device-descriptor request at
`18:41:58` with `error -110`. Stock Android `18d1:4ee7` returned at
`18:42:19`; no Fullerene `1234:0001` appeared. This A/B does not move the
EP0 descriptor boundary. The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-dma-cache-maintenance.1u.bin`, SHA-256
`b51235555c0d4312c18d3cd8300332107499d0ec811f3b4d5da1b6fa5bcba8c6`.
No analyzer, flash, erase, or secure-debug operation was used.

The next source correction targets qpr1's Connect Done transition: on DWC3
revisions >= 2.30a it adds the EOPF/suspend event bit to `DEVTEN` only after
Connect Done, rather than including it in the initial gadget-start mask.

### 2026-09-05 qpr1 Connect Done EOPF transition physical A/B (Run 1813781.0)

This A/B added qpr1's EOPF/suspend event bit to `DEVTEN` after Connect Done
for revisions >= 2.30a, while retaining the qpr1 32-slot resource range and
the standalone-probe cache-maintenance policy. The accepted artifact was
`tmp/fullerene-bramble-loop.1813781.0/fullerene-bramble-boot.img`, SHA-256
`476a704cc5d038e2152514eca3657ad2425cc65820d2765c0b88cf91d0049689`.

The physical Bramble reached HS attach at `18:48:00 JST` (`usb 1-9`, device
80), then timed out on the first device-descriptor request at `18:48:06` with
`error -110`. Stock Android `18d1:4ee7` returned at `18:48:27`; no Fullerene
`1234:0001` appeared. The post-Connect Done EOPF transition did not move the
EP0 descriptor boundary. The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-qpr1-conndone-eopf.1u.bin`, SHA-256
`a878cfe3fb0436fadab152cf43f910752d22c9d1768742440f07fa21c1ac00ec`.
No analyzer, flash, erase, or secure-debug operation was used.

The next A/B keeps the qpr1 32-slot/cache profile and arms the initial EP0
SETUP before Run/Stop, matching qpr1's `dwc3_ep0_out_start()` timing; it
removes only the current `--start-after-connect` delay.

### 2026-09-05 qpr1 initial-SETUP-before-Run/Stop physical A/B (Run 1819771.0)

This A/B removed only `--start-after-connect`, so the initial qpr1
`dwc3_ep0_out_start()`/`STARTTRANSFER` timing was used before Run/Stop. The
qpr1 32-slot resource, cache-maintenance, Connect Done EOPF, HS-PHY, reset,
and no-SMMU settings were retained. The accepted artifact was
`tmp/fullerene-bramble-loop.1819771.0/fullerene-bramble-boot.img`, SHA-256
`1ed0cc455a50aaaa99e02690c603913965f63685653ba8d7250e737f8c6a433b`.

The physical Bramble did not produce a Fullerene HS attach or descriptor
request before watchdog recovery. Stock Android `18d1:4ee7` returned at
`18:53:16`; no Fullerene `1234:0001` appeared. This source-order placement is
negative on the handoff: it removes the previously reachable HS-attach
boundary. The raw `/dev/usbmon1` capture is archived at
`tmp/fullerene-usbmon-qpr1-setup-before-runstop.1u.bin`, SHA-256
`cff3c41e59756dae2bee7bc794ed7e308eaa8f5920d3c371a004e08b25c45a6f`.
No analyzer, flash, erase, or secure-debug operation was used.

The active HS-attach-reaching control therefore retains `--start-after-connect`.
The next source audit targets the qpr1 endpoint command engine's initial
`SETEPCONFIG` notification fields and the already-observed descriptor timeout.

### 2026-09-05 qpr1 endpoint-command timeout physical A/B (Run 1856221.0)

The direct path's endpoint-command polling budget was corrected from the
unverified 50,000-read extension to qpr1's `dwc3_send_gadget_ep_cmd()` value
of 3,000 reads. The active HS-attach-reaching profile was otherwise retained.
The accepted artifact was
`tmp/fullerene-bramble-loop.1856221.0/fullerene-bramble-boot.img`, SHA-256
`82d83a4c3fe6fe21745381788fd5d3e50e443ebfd9b848a8a490e28642182ad7`.

The physical Bramble reached HS attach at `19:20:42 JST` (`usb 1-9`, device
83), then timed out on the first device-descriptor request at `19:20:47` with
`error -110`. Stock Android `18d1:4ee7` returned at `19:21:08`; no Fullerene
`1234:0001` appeared. In usbmon, the first descriptor submit was at
`19:20:42.329895`, its completion was `-2` with zero captured data at
`19:20:47.347929`, and the retries completed `-71` with zero data at
`19:20:47.964706`, `.964850`, and `.964976`. The timeout-budget correction
did not move the no-response boundary. The raw capture is
`tmp/fullerene-usbmon-epcmd-timeout-3000.1u.bin`, SHA-256
`f29a56b76248406d584b997776ed02013cef0f92421deeabcbdc44af229fc7cd`.
No analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-05 retained SETUP gate physical readout (Run 1846144.0)

This run retained the active qpr1/HS-attach-reaching profile and added
`--signal-cmd-gate setup`, with `/dev/usbmon1` captured in parallel. The
accepted artifact was
`tmp/fullerene-bramble-loop.1846144.0/fullerene-bramble-boot.img`, SHA-256
`e5e96a91babd2ecccfa13c1bc2e67eafbf61def1fe105ad6b186eabbd1bfe463`.

The physical Bramble reached HS attach at `19:13:51 JST` (`usb 1-9`, device
82), then timed out on the first device-descriptor request at `19:13:56` with
`error -110`. Stock Android `18d1:4ee7` returned at `19:14:18`; no Fullerene
`1234:0001` appeared. The `setup` gate produced no separate host-visible
boundary, so this run does not promote the internal latch to stronger evidence;
it reproduces the same attach/descriptor boundary. The raw capture is
`tmp/fullerene-usbmon-signal-setup.1u.bin`, SHA-256
`b6828bcc9926abb77ff5546bf8fc181a9b7c14d164cfd5c167bd3e3b606a5733`.
No analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-05 Bramble backup/device-tree availability check

A read-only host/workspace check found no saved Android backup, raw Bramble
partition dump, or Bramble DTB/DTBO artifact. The connected Android device does
expose `/sys/firmware/devicetree/base`, but ordinary production `adb shell`
cannot read its properties (`Permission denied`); `/proc/device-tree` is the
same tree. The device's `/dev/block/by-name` listing confirms the candidate
source partitions `boot_a/b`, `vendor_boot_a/b`, and `dtbo_a/b`. This is
partition-presence evidence only: no partition was read, written, flashed, or
erased, and no Android user-data backup was created. Bootloader `fetch` was
attempted for `dtbo_b` but this device rejected it because its fastboot lacks
`max-fetch-size`; an Android `adb pull` of the same partition was rejected by
production adbd with `Permission denied`. A future DT extraction must use a
read-only, permitted bootloader/image path or a matching official stock image,
rather than treating the existing Fullerene boot artifact as the stock device
tree. This was the state of the workspace before the factory-image extraction
recorded immediately below; it is not a claim that the current Downloads
directory lacks a matching stock DTB/DTBO.

### 2026-09-05 exact-build stock image extraction and fallback A/B

The requested backup check found no existing Android user-data backup, raw
partition dump, or saved Bramble DT artifact. Instead, a read-only HTTP Range
extraction reconstructed only the three relevant compressed members from the
official [Google factory-images index](https://developers.google.com/android/images)
package
[`bramble-up1a.231105.001.b2-factory-46a218d9.zip`](https://dl.google.com/dl/android/aosp/bramble-up1a.231105.001.b2-factory-46a218d9.zip).
The package matches the connected device's
`google/bramble/bramble:14/UP1A.231105.001.B2/11260668:user/release-keys`
fingerprint. The outer ZIP is about 2.43 GB, but it was not downloaded in full;
the extracted `boot.img`, `vendor_boot.img`, and `dtbo.img` members are retained
under `tmp/` for reproducibility.

On 2026-09-06 the actual Downloads copy was rechecked at
`/home/placeless/ダウンロード/bramble-up1a.231105.001.b2-factory-46a218d9.zip`:
size `2430380082` bytes, SHA-256
`46a218d9dc2bf1802a584fa4e7e1b4d609a62189bd87a474201fb6d84e1c3e1c`. Its outer
ZIP lists the stored inner archive
`bramble-up1a.231105.001.b2/image-bramble-up1a.231105.001.b2.zip` (size
`2350546036` bytes), plus the matching bootloader, radio, and flash scripts.
The inner archive contains the `boot.img`, `vendor_boot.img`, and `dtbo.img`
members used for the retained extractions. This is the exact file in Downloads,
not a separate user-data backup or an unverified similarly named artifact; the
connected device again reported the same fingerprint and incremental `11260668`.

For Android boot image v3, the board DTB is in `vendor_boot.img` and the
compiled device-tree overlays are in `dtbo.img`. The retained exact-build
artifacts are therefore the usable stock tree/overlay source:
[`tmp/bramble-stock-vendor_boot.img`](../tmp/bramble-stock-vendor_boot.img),
[`tmp/bramble-stock-vendor_boot.dtb`](../tmp/bramble-stock-vendor_boot.dtb),
and [`tmp/bramble-stock-dtbo.img`](../tmp/bramble-stock-dtbo.img). The DTBO
table has 36 entries. This confirms the factory image does contain the device
tree; it does not mean that a live Android user-data backup was taken.

The stock artifacts are:

- `tmp/bramble-stock-boot.img`, SHA-256
  `e1c1e38a20ab06101d3bd61cb3fb96e5176dac0696270811051b4b582d2c2d7f`
- `tmp/bramble-stock-vendor_boot.img`, SHA-256
  `8af68ba6199cb6947fdb1e1f49cb2052537d284ef38e1ea1beb3c4a2ca7bc135`
- `tmp/bramble-stock-dtbo.img`, SHA-256
  `b15bd3ab1447c477ab2037f48e78cb818cd97f018da0a2a1eea94e619b3353c2`
- extracted base DTB `tmp/bramble-stock-vendor_boot.dtb`, SHA-256
  `6ba711544adafd979ba9bd5e251c00ebe76220c2f593b5fab6371e8bbf0b59ff`;
  FDT offset in `vendor_boot.img` `30150656`, size `405231` bytes.

The exact stock DTB confirms `qcom,iommu-dma=atomic`, DMA pool
`0x90000000/0x60000000`, controller core/HS clocks `133333333/66666667`,
three GSI event buffers, `qcom,dwc-usb3-msm-tx-fifo-size=0x6c30`, DWC3 HIRD
`0x10`, and
`qcom,param-override-seq = <0x63 0x6c 0x85 0x70 0x17 0x74>`. The earlier
two-pair qpr1 fallback in the working tree was a source/version mismatch for
this exact Android 14 factory package; the compiled fallback was corrected to
the three stock pairs. The older DTB SHA recorded above this section was stale
and is superseded by the extracted file's SHA.

The same exact outer ZIP also contains
`bootloader-bramble-b5-0.6-10489838.img` (8,972,680 bytes), SHA-256
`63df7c9a2957ffb6f648ea0973f1b2f3ada427a10bd1ac29ac8741a3c8ff3272`.
Read-only inspection found the Qualcomm `FBPK/FBPT` container and extracted
the `xbl_a` payload at FBPK offset `9928`, size `0x380000`, as
`tmp/bramble-factory-xbl_a.elf`, SHA-256
`7748d80644542c5972b469472ff86ba9130b6d3f580a6b5d810b2a7d0f931dce`.
The public `linux-msm/xbltools` splitter produced these AArch64 ELF
components:

- `tmp/bramble-factory-xbl-sbl1.elf`, SHA-256
  `41fe85b8d32d73cd2b00e1512ece165b68d389efa934aacc331b201664dbeda6`
- `tmp/bramble-factory-xbl-core.elf`, SHA-256
  `52146ad38fae5baa9bf18121d0f800b5842dc808ebd3b0c5cd7cdead94090322`
- `tmp/bramble-factory-xbl-sec.mbn`, SHA-256
  `43c8f792f89fa300675d52629437fcc4591d1f383240d2776fc968d763af862b`

The Qualcomm FBPK unpacker also exposes the packed `abl` payload, correcting
the earlier shorthand that said there was no standalone ABL image. The exact
artifact is retained as `tmp/bramble-factory-abl.elf`, size `1048576` bytes,
SHA-256 `1a14a6a3cb65fcdab9c6121d81c81f50084283d206a75865c71c2382fd9cfefe`.
It is an ARM ELF whose embedded firmware volume begins at ELF file offset
`0x3000` and has size `0xfd000`. UEFI parsing of that volume found the
`LinuxLoader` application, `FastbootTransportUsbDxe` driver, and
`B1c1FastbootApp` application. The retained nested PE artifacts are
`tmp/bramble-factory-abl-fastboot-transport-usb.pe` (SHA-256
`13cf166d7f96ca0e78da38510b859b73c92c85da046a84463e316ed6fed5350a`) and
`tmp/bramble-factory-abl-b1c1-fastboot.pe` (SHA-256
`c7c29304afdec988d578b4655199d33a38870ef02f8878ba2429fa6e76baf313`).
The USB driver strings include `FastbootTransportUsbDxe`, connection and
transfer-completion states, and the generic `Protocol Error`/`CRC Error`
names; the ABL UEFI layer is therefore real same-build evidence, but the
strings alone are not a live DWC3 register trace.

The extracted XBL strings expose `USB30_PRIM = 0x0a600000`, `USB_RUMI`,
`USB30_SEC`, and `UsbFnIoRevNum = 0x00010001`. The XBL/SBL-side USB strings
also include `endxfer EP0 OUT`, `endxfer EP0 IN`, `usb_shared_hs_phy_init`,
and `usb_shared_ss_phy_init`. These are same-build bootloader observations,
not a live register trace or proof that each string belongs to the production
path; no bootloader partition was flashed or booted.

Run `1878964.0` used that corrected fallback with the direct USB2 handoff.
`fastboot boot` accepted
`tmp/fullerene-bramble-loop.1878964.0/fullerene-bramble-boot.img`, SHA-256
`6d921dc33782a5de2dd05e5b5aab42a14219955ef9b8a0e9a1d87248047b4ffc`. The host
reached Fullerene high-speed attach, but the first Device Descriptor timed out
with `-110`; Android `18d1:4ee7` returned with `ro.boot.bootreason=watchdog`.
No `1234:0001` appeared. Passive `/dev/usbmon1` capture is retained as
`tmp/fullerene-usbmon-stock-hsphy-fallback.1u.bin`, SHA-256
`d6d8f32a791f3945101c4252ff4dddc1e5aa5129d61f3ca537875c334fcb2367`.
This exact-build PHY fallback did not move the attach-to-descriptor boundary.
No partition was read or written, no user-data backup was made, and no
analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-05 exact-build fallback + qpr1 USB2 SUSPHY/DEVTEN retake

Run `1896337.0` retook the last high-speed-attach-reaching profile using the
exact-match stock `vendor_boot` PHY fallback and added the two source-directed
USB2 controls that had previously only been preflighted:
`--usb2-source-susphy --usb2-source-exact-devten`. The complete invocation was

```text
cargo run -q -p flasks --bin bramble-usb -- loop \
  --template tmp/bramble-stock-boot.img --adb-reboot-to-fastboot \
  --direct-handoff --start-after-connect --no-smmu \
  --hsphy-source-exact --refresh-hsphy-power \
  --usb2-source-susphy --usb2-source-exact-devten \
  --enum-timeout 30 --hold 5 --fastboot-wait 60
```

`fastboot boot` accepted the artifact
`tmp/fullerene-bramble-loop.1896337.0/fullerene-bramble-boot.img`, SHA-256
`5019538685cd5d65230aa7c8815ffee03516bb0c2b4ceb9947e581473498acb3`.
The host saw Fullerene high-speed attach on `usb 1-9` at `19:49:48 JST`, but
the first `GET_DESCRIPTOR(Device)` timed out with `-110` at `19:49:53`.
Stock Android `18d1:4ee7` returned over SuperSpeed at `19:50:14`, with
`ro.boot.bootreason=watchdog`; no `1234:0001` appeared. The added qpr1
SUSPHY-through-EP0 and exact device-event mask did not move the boundary.
The passive `/dev/usbmon1` capture is
`tmp/fullerene-usbmon-stock-susphy-devten.1u.bin`, SHA-256
`3d88ae97f4633091c4fc8c271bdd1b4b2e27af2e7cbcfce3f900cb17ebc543a2`.
The phone is back in Android and `adb devices -l` sees serial
`26191JECB00076`. No partition was read or written, no user-data backup was
made, and no analyzer, flash, erase, or secure-debug operation was used.

Run `1907651.0` repeated the same exact-build profile with the additional
`--usb2-source-exact-cmd-guard` control. This forces the qpr1 USB2
`SUSPHY/ENBLSLPM` clear-and-restore around every endpoint command even if the
inherited Fastboot `DSTS.CONNECTSPD` value looks like SuperSpeed. The artifact
`tmp/fullerene-bramble-loop.1907651.0/fullerene-bramble-boot.img` has SHA-256
`a0243d70d61fb6c241698fd529ddb7ad67e10532a10b4acf0eb4b0c6d2e9661a`.
The host again saw Fullerene high-speed attach on `usb 1-9` at `19:57:27 JST`,
then `device descriptor read/64, error -110` at `19:57:32`; Android
`18d1:4ee7` returned, with no `1234:0001`. The passive capture
`tmp/fullerene-usbmon-stock-susphy-devten-cmdguard.1u.bin` has SHA-256
`16f092371adf8f8d729aa194ae20b9a0c2b17196ca08d357aea4c54bb89fb2ce`.
This A/B did not move the boundary. The phone returned to Android; no
partition/user-data backup/analyzer/flash/erase/secure-debug operation was
used.

Run `1915263.0` then changed only the final USB2 Run/Stop policy to the qpr1
source-exact DCTL write (`--usb2-source-exact-runstop`), while retaining the
exact-build DT fallback, qpr1 USB2 SUSPHY/DEVTEN controls, and the attach-
reaching direct profile:

```text
cargo run -q -p flasks --bin bramble-usb -- loop \
  --template tmp/bramble-stock-boot.img --adb-reboot-to-fastboot \
  --direct-handoff --start-after-connect --no-smmu \
  --hsphy-source-exact --refresh-hsphy-power \
  --usb2-source-susphy --usb2-source-exact-devten \
  --usb2-source-exact-runstop \
  --enum-timeout 30 --hold 5 --fastboot-wait 60
```

`fastboot boot` accepted the artifact
`tmp/fullerene-bramble-loop.1915263.0/fullerene-bramble-boot.img`, SHA-256
`1a18fb17719ed8c6793c9969289d8fd3e363571a6b61eb71b3f1a8eeb67fb9ff`.
The host saw Fullerene high-speed attach on `usb 1-9` at `20:02:59 JST`, but
the first `GET_DESCRIPTOR(Device)` timed out with `-110` at `20:03:05`.
Stock Android `18d1:4ee7` returned at `20:03:26`, with
`ro.boot.bootreason=watchdog`; no `1234:0001` appeared. No separate raw
usbmon archive was produced for this run; the per-run host kernel capture is
`tmp/fullerene-bramble-loop.1915263.0/kernel-final.log`. The source-exact
Run/Stop delta did not move the attach-to-descriptor boundary. The phone is
back in Android. No partition was read or written, no user-data backup was
made, and no analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 current-HEAD SuperSpeed full-path retake (Run 18290.0)

The current HEAD was retested against the exact matching stock `boot.img` with
the full lane-A SuperSpeed profile. This includes the post-`1693171.0`
source-order corrections for DISSCRAMBLE, MMIO/DBM barriers, per-direction
DALEPENA publication, and DWC31 KEEP_CONNECT/Run-Stop handling. The complete
invocation was

```text
cargo run -q -p flasks --bin bramble-usb -- loop \
  --template tmp/bramble-stock-boot.img \
  --adb-reboot-to-fastboot --super-speed --qmp-lane a \
  --ss-pre-qmp-phy-setup \
  --ss-reassert-qmp-power-after-gctl \
  --ss-reassert-qmp-clocks \
  --ss-reassert-qmp-clocks-after-gctl \
  --ss-reassert-hs-phy-ref-after-gctl \
  --ss-dis-sleep-mode-before-gadget \
  --ss-clear-qmp-autonomous \
  --ss-clear-qmp-autonomous-exact \
  --ss-qmp-resume-wmb \
  --ss-qmp-lfps-clear-wmb \
  --ss-qmp-notify-disconnect \
  --ss-clear-vbus-override-before-qmp \
  --ss-clear-keep-connect-before-stop \
  --ss-clear-usb3-susphy-before-qmp \
  --ss-disable-gadget-irq-before-stop \
  --ss-disable-ep0-before-stop \
  --ss-clear-gsi-stop-state \
  --ss-preserve-ref-clock-state \
  --ss-reassert-core-clocks \
  --ss-android-dbm-reset \
  --ss-lfps-timer \
  --ss-clear-ux-exit-px \
  --source-exact-runstop \
  --no-core-reset --no-smmu \
  --enum-timeout 30 --hold 30 --fastboot-wait 60 --observe-secs 10
```

Build audit, QEMU preflight, and the Fastboot image audit passed. `fastboot
boot` accepted `tmp/fullerene-bramble-loop.18290.0/fullerene-bramble-boot.img`
with SHA-256
`d2e982efd5ab280483d7bfecd3757c5a65580ea1fe46faea531798bd3f11aaee`.
The host saw the old Android `18d1:4ee0` disconnect at `09:06:36 JST`, then
Fullerene's High-Speed attach on `usb 1-9` at `09:06:47`; the first
`GET_DESCRIPTOR(Device)` failed with `device descriptor read/64, error -110`
at `09:06:52`. No `1234:0001` device or completed Fullerene descriptor was
observed. Stock Android `18d1:4ee7` returned over SuperSpeed at `09:07:13`
with serial `26191JECB00076`; the harness recorded
`ro.boot.bootreason=watchdog` and returned exit status 1 for the enumeration
timeout. This current-HEAD SuperSpeed retake therefore did not move the
attach-to-EP0-response boundary. The per-run host captures are
`tmp/fullerene-bramble-loop.18290.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.18290.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD USB2 source-exact device-reset A/B (Run 29473.0)

The latest known HS-attach-reaching USB2 profile was retested on current HEAD
with the qpr1 source-exact device-core reset added. This is a current-code
retake of the older `1235572.0` device-reset experiment, after the later
MMIO/DBM barrier, endpoint publication-order, DISSCRAMBLE, and DWC31
KEEP_CONNECT/Run-Stop corrections. The complete invocation was

```text
cargo run -q -p flasks --bin bramble-usb -- loop \
  --template tmp/bramble-stock-boot.img --adb-reboot-to-fastboot \
  --direct-handoff --start-after-connect --no-smmu \
  --hsphy-source-exact --refresh-hsphy-power \
  --usb2-source-susphy --usb2-source-exact-devten \
  --usb2-source-exact-cmd-guard \
  --usb2-source-exact-runstop \
  --usb2-source-exact-device-reset \
  --enum-timeout 30 --hold 5 --fastboot-wait 60
```

QEMU preflight, image audit, and `fastboot boot` acceptance passed. The
accepted artifact is
`tmp/fullerene-bramble-loop.29473.0/fullerene-bramble-boot.img`, SHA-256
`2a8040f3d5128d9f60d6020926e9b27db02fb1a2590d72d7bcbb0def5aabf416`.
The host saw the old Android USB disconnect at `09:13:34 JST`, Fullerene
High-Speed attach on `usb 1-9` at `09:13:44`, and
`device descriptor read/64, error -110` at `09:13:50`. No `1234:0001`
device or completed Fullerene descriptor appeared. Stock Android
`18d1:4ee7` returned over SuperSpeed at `09:14:11`, with serial
`26191JECB00076`; `ro.boot.bootreason=watchdog`. The source-exact device-reset
delta therefore did not move the current HS attach-to-EP0-response boundary.
The per-run host captures are
`tmp/fullerene-bramble-loop.29473.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.29473.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed USB2-pull-up-after-Run/Stop A/B (Run 38747.0)

The source audit found an existing but unreachable cfg branch in the current
SuperSpeed path: `fullerene_aarch64_usb_usb2_susphy_after_runstop`. It repeats
the USB2 `SUSPHY` write immediately after the production DWC3 Run/Stop
transition, before the SuperSpeed state snapshot. `bramble-usb` had no public
flag or environment forwarding for it, so the harness was extended with
`--usb2-susphy-after-runstop`, requiring `--super-speed` and setting
`FULLERENE_AARCH64_USB_USB2_SUSPHY_AFTER_RUNSTOP`. The kernel CLI was not given
an unknown argument; the existing build-time cfg is the only control.

The current-HEAD lane-A SuperSpeed profile was then rerun with that A/B:

```text
cargo run -q -p flasks --bin bramble-usb -- loop \
  --template tmp/bramble-stock-boot.img --adb-reboot-to-fastboot \
  --super-speed --qmp-lane a \
  --ss-pre-qmp-phy-setup \
  --ss-reassert-qmp-power-after-gctl \
  --ss-reassert-qmp-clocks --ss-reassert-qmp-clocks-after-gctl \
  --ss-reassert-hs-phy-ref-after-gctl \
  --ss-dis-sleep-mode-before-gadget \
  --ss-clear-qmp-autonomous --ss-clear-qmp-autonomous-exact \
  --ss-qmp-resume-wmb --ss-qmp-lfps-clear-wmb \
  --ss-qmp-notify-disconnect --ss-clear-vbus-override-before-qmp \
  --ss-clear-keep-connect-before-stop --ss-clear-usb3-susphy-before-qmp \
  --ss-disable-gadget-irq-before-stop --ss-disable-ep0-before-stop \
  --ss-clear-gsi-stop-state --ss-preserve-ref-clock-state \
  --ss-reassert-core-clocks --ss-android-dbm-reset --ss-lfps-timer \
  --ss-clear-ux-exit-px --source-exact-runstop \
  --usb2-susphy-after-runstop --no-core-reset --no-smmu \
  --enum-timeout 30 --hold 30 --fastboot-wait 60 --observe-secs 10
```

The build audit, QEMU preflight, and image audit passed. `fastboot boot`
accepted `tmp/fullerene-bramble-loop.38747.0/fullerene-bramble-boot.img`,
SHA-256
`f54a0ced4f68adb9288c0a8a62d950e23da6d1b6b691ceaea6bf799f80cf3da4`.
The host saw the old Android `18d1:4ee7` disconnect at `09:20:13 JST`, then
Fullerene High-Speed attach on `usb 1-9` at `09:20:24`; the first Device
Descriptor failed with `device descriptor read/64, error -110` at `09:20:29`.
No `1234:0001` appeared. Stock Android `18d1:4ee7` returned over SuperSpeed
at `09:20:50` with serial `26191JECB00076`; the harness recorded
`ro.boot.bootreason=watchdog`. The A/B therefore did not move the
attach-to-EP0-response boundary. Evidence is in
`tmp/fullerene-bramble-loop.38747.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.38747.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed USB3-SUSPHY-after-Run/Stop A/B (Run 43733.0)

The existing source-aligned `--ss-clear-usb3-susphy-after-runstop` branch was
tested as the next single-delta SuperSpeed experiment, retaining the newly
reachable `--usb2-susphy-after-runstop` control and the full lane-A profile.
The USB3 suspend bit was cleared immediately after DWC3 Run/Stop, before the
post-boundary state snapshot; no endpoint or packet behavior was changed.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.43733.0/fullerene-bramble-boot.img`, SHA-256
`7924eadd433d1a1d2a0755807e7fe1b01707cbaa43bf51d18d61fe2d5706086c`.
The host saw the old Android USB disconnect at `09:23:54 JST`, Fullerene
High-Speed attach on `usb 1-9` at `09:24:04`, and
`device descriptor read/64, error -110` at `09:24:09`. No `1234:0001`
appeared. Stock Android `18d1:4ee7` returned over SuperSpeed at `09:24:31`
with serial `26191JECB00076`; `ro.boot.bootreason=watchdog`. The USB3
post-Run/Stop SUSPHY delta did not move the attach-to-EP0-response boundary.
Evidence is in `tmp/fullerene-bramble-loop.43733.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.43733.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed preserve-Fastboot-Run/Stop retake (Run 47845.0)

The older preserve-Run/Stop result was not treated as a specification. The
current-HEAD full lane-A SuperSpeed profile was retested with
`--direct-handoff --preserve-fastboot-runstop --no-core-reset`, retaining the
source-aligned PHY and DWC3 controls. This skips only the old-session
Run/Stop=0 write before the handoff; the later gadget boundary remains active.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.47845.0/fullerene-bramble-boot.img`, SHA-256
`50cd84b0baeec6e9f2f359fdcdfbfad741b15db6a056fca27c4fcb6587f59841`.
The host saw the old Android `18d1:4ee0` disconnect at `09:26:38 JST`, then
Fullerene High-Speed attach on `usb 1-9` at `09:26:49`; the first Device
Descriptor failed with `device descriptor read/64, error -110` at `09:26:54`.
No `1234:0001` appeared. Stock Android `18d1:4ee7` returned over SuperSpeed
at `09:27:15` with serial `26191JECB00076`; `ro.boot.bootreason=watchdog`.
The current retake therefore does not recover the SS path or move the
attach-to-EP0-response boundary. Evidence is in
`tmp/fullerene-bramble-loop.47845.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.47845.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed preserve-phy retake (Run 50706.0)

The current-HEAD full lane-A SuperSpeed profile was rerun with
`--ss-preserve-phy-state`, retaining Fastboot's already-trained QMP/USB3 PHY
and rebuilding only the DWC3 gadget/EP0 side. This is a source-distinct
retake of the older preserve-phy records, after the current DWC31 and EP0
publication-order corrections.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.50706.0/fullerene-bramble-boot.img`, SHA-256
`0b4141d455473e3ead1df1a3c3f55b8f5a202b3e4e9d8ba62cdf44039b874613`.
The host saw the old Android USB disconnect at `09:28:37 JST`. No Fullerene
HS attach appeared. Instead, the host reached the SuperSpeed setup path and
reported `device not accepting address 14, error -62` at `09:28:59`, then
power-cycled the port at `09:29:03`. Stock Android `18d1:4ee7` returned over
SuperSpeed at `09:29:13` with serial `26191JECB00076`; `ro.boot.bootreason=watchdog`.
No `1234:0001` appeared. Preserving the trained PHY suppresses the HS
fallback but still does not produce a completed Fullerene descriptor.
Evidence is in `tmp/fullerene-bramble-loop.50706.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.50706.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed preserve-phy + retry-setup A/B (Run 53664.0)

The preserve-phy path was retested with `--ss-retry-setup`, extending the
post-Run/Stop EP0 OUT `STARTTRANSFER` arm window from 100 ms to 5 seconds.
This was intended to distinguish SuperSpeed training latency from the EP0
ownership boundary without changing the PHY or descriptor data.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.53664.0/fullerene-bramble-boot.img`, SHA-256
`cb74dedb9afc8874aa288ec0e15a6c707f981fc9e9c6b4fa13f22ab62dc9f188`.
The host saw the old Android `18d1:4ee0` disconnect at `09:30:29 JST`, then
SuperSpeed setup reached `device not accepting address 19, error -62` at
`09:30:51`; xHCI power-cycled the port at `09:30:55`. Stock Android
`18d1:4ee7` returned over SuperSpeed at `09:31:06` with serial
`26191JECB00076`; `ro.boot.bootreason=watchdog`. No `1234:0001` appeared.
The longer setup-arm window did not move the SS address/EP0 boundary. Evidence
is in `tmp/fullerene-bramble-loop.53664.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.53664.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 current-HEAD SuperSpeed preserve-phy + separate-SETUP A/B (Run 56664.0)

The current preserve-phy SuperSpeed path was retested with the non-source
diagnostic `--ss-separate-setup-buffer`, replacing qpr1's EP0 TRB alias for
the eight-byte SETUP DMA object while leaving the PHY, endpoint, and
descriptor paths otherwise unchanged.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.56664.0/fullerene-bramble-boot.img`, SHA-256
`148e4ee3b548231be90c40215bf3844f852756899e2eb71e6f230bbee8469dfc`.
The host saw the old Android USB disconnect at `09:32:27 JST`, then the same
SuperSpeed setup-command timeouts and `device not accepting address 24,
error -62` at `09:32:49`; xHCI power-cycled the port at `09:32:53`. Stock
Android `18d1:4ee7` returned over SuperSpeed at `09:33:04` with serial
`26191JECB00076`; `ro.boot.bootreason=watchdog`. No `1234:0001` appeared.
Changing the SETUP buffer ownership did not move the SS address/EP0 boundary.
Evidence is in `tmp/fullerene-bramble-loop.56664.0/kernel-final.log` and
`tmp/fullerene-bramble-loop.56664.0/android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 qpr1 SuperSpeed eager-SETUP A/B harness correction (Run 66527.0)

The first attempt to exercise the new source-directed `--ss-eager-setup`
candidate stopped before `fastboot boot`: `bramble-usb` forwarded the new
kernel option, but the outer `flasks` argument/build-option layer did not yet
accept it. The run directory is
`tmp/fullerene-bramble-loop.66527.0`; its `build.log` records
`unexpected argument '--usb-gadget-handoff-ss-eager-setup'` and no boot
artifact was produced.

This is a harness/build-wiring failure, not a device result. The phone remained
in Fastboot as `18d1:4ee0`; no Fullerene attach, `1234:0001`, or
`fastboot boot` result is claimed. The missing outer parser/build-option/env
wiring was then added. No partition was read or written, no user-data backup
was created, and no analyzer, flash, erase, or secure-debug operation was
used.

### 2026-09-06 qpr1 SuperSpeed eager-SETUP physical A/B (Run 73831.0)

After correcting the outer parser/build-option/env wiring, the new
`--ss-eager-setup` path was exercised on the connected exact-build Bramble.
The path arms EP0 SETUP before the production Run/Stop write, matching qpr1
`__dwc3_gadget_start()` ordering. `fastboot boot` accepted
`tmp/fullerene-bramble-loop.73831.0/fullerene-bramble-boot.img`, SHA-256
`774244fb06c0614f4a5ee0ca1886a1c993c10374eef3fab1a5719b962a7b1011`.

The host did not observe `1234:0001`. Fullerene produced no host-visible
identity; after the 38-second recovery window, stock Android returned as
SuperSpeed `18d1:4ee7` (serial `26191JECB00076`) with
`ro.boot.bootreason=watchdog`. The host kernel log records only the Android
device attach at `09:45:33 JST`; evidence is in
`tmp/fullerene-bramble-loop.73831.0/kernel-final.log` and
`android-fallback-usb.txt`. This A/B does not validate the eager-SETUP
hypothesis, although it confirms the corrected build wiring reaches physical
`fastboot boot`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 qpr1 SS SUSPHY/resource-order A/B harness correction (Run 81942.0)

The next source-directed attempt combined qpr1's SS `SUSPHY`-through-gadget
start policy (`--ss-source-susphy`) with qpr1's endpoint resource ordering
(`--android-resource-order`) and the corrected eager-SETUP wiring. The outer
`flasks` validation rejected the latter because it was incorrectly restricted
to the USB2 probe, so the run stopped before image creation and before
`fastboot boot`; no physical result or `1234:0001` claim is made. The
validation was corrected to allow the shared resource-order implementation on
the SuperSpeed probe as well.

No partition was read or written, no user-data backup was created, and no
analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 qpr1 SS SUSPHY/resource-order validation retake (Run 83393.0)

The first validation correction changed only the diagnostic text; the actual
condition still required the USB2 probe boolean, so the same SS invocation
was rejected before image creation with
`--usb-gadget-handoff-android-resource-order requires the Bramble gadget
handoff probe on AArch64 build/run/debug`. The condition was then corrected
to accept either the USB2 or SuperSpeed probe. This is another harness result,
not a physical device result.

No partition was read or written, no user-data backup was created, and no
analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 qpr1 SS SUSPHY/resource-order compile correction (Run 84582.0)

With the validation fixed, the SS source-directed attempt reached the build
stage but failed before `fastboot boot`: Rust rejected an expression-level
`#[cfg]` used in the new USB3 SUSPHY branch (`E0658`). No boot artifact was
executed and no physical USB result or `1234:0001` claim is made. The branch
was rewritten as cfg-selected blocks. The phone was left in Fastboot by the
ADB-to-Fastboot preflight and is restored with `fastboot reboot` before the
next attempt.

No partition was read or written, no user-data backup was created, and no
analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 qpr1 SS SUSPHY + resource-order + eager-SETUP physical A/B (Run 86658.0)

The corrected source-directed profile was finally booted on the exact-build
Bramble. It combined `--ss-source-susphy` (retain USB3 PIPE SUSPHY through
endpoint construction), `--android-resource-order` (preallocate endpoint
resources before EP0 configuration), and `--ss-eager-setup`, with the same
factory-image template and lane-A SS profile.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.86658.0/fullerene-bramble-boot.img`, SHA-256
`f8c067ea73a93cb45fdef2cf3fb6341fe0c8137cb3a5229702b612625486539d`.
The host did not observe `1234:0001` or a Fullerene identity. After the
recovery window, Android returned over SuperSpeed as `18d1:4ee7` (serial
`26191JECB00076`) at `09:53:39 JST`; `ro.boot.bootreason=watchdog`.
Evidence is in `tmp/fullerene-bramble-loop.86658.0/kernel-final.log` and
`android-fallback-usb.txt`. This source-aligned SS A/B is negative.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 qpr1 SS SUSPHY + resource-order without eager-SETUP physical A/B (Run 90289.0)

This control removed only `--ss-eager-setup` from the previous source-directed
profile, retaining `--ss-source-susphy`, `--android-resource-order`, the exact
factory-image template, and the lane-A SuperSpeed controls.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.90289.0/fullerene-bramble-boot.img`, SHA-256
`2c104d6c6734395984683bb72d42a64e6df488d04065f0610961a21c251d7402`.
The host saw a Fullerene high-speed attach at `09:55:37 JST`, but the first
Device Descriptor timed out with `-110` at `09:55:42`; no `1234:0001` appeared.
Stock Android then returned as SuperSpeed `18d1:4ee7` at `09:56:02`, serial
`26191JECB00076`, with `ro.boot.bootreason=watchdog`. Evidence is in
`tmp/fullerene-bramble-loop.90289.0/kernel-final.log` and
`android-fallback-usb.txt`. The SUSPHY/resource-order delta alone is negative;
removing eager SETUP restores the ordinary HS-attach boundary but not EP0
descriptor response.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 factory-XBL EP0 IN NORMAL-TRB A/B (Run 95984.0)

The exact factory package's XBL observation records `TRBCTL=1` (`NORMAL`) for
the EP0 IN data response. This run enabled only that XBL-derived data-phase
TRB differential (`--xbl-ep0-in-data`) on the previously attach-reaching
ADB-to-Fastboot USB2 profile; EP0 SETUP/status TRBs and the PHY/resource
controls were unchanged.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.95984.0/fullerene-bramble-boot.img`, SHA-256
`3836f77c5bbb8f8718c750cfa147a5970bd5752b25b219fc067a5c77fad64d15`.
The host reached Fullerene high-speed attach at `09:59:38 JST`, but the first
Device Descriptor timed out with `-110` at `09:59:43`; no `1234:0001` or
Fullerene descriptor appeared. Stock Android returned as SuperSpeed
`18d1:4ee7` at `10:00:04`, serial `26191JECB00076`, with
`ro.boot.bootreason=watchdog`. Evidence is in
`tmp/fullerene-bramble-loop.95984.0/kernel-final.log` and
`android-fallback-usb.txt`. The factory-XBL NORMAL data-TRB A/B did not move
the host-visible attach-to-descriptor boundary.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 factory-XBL EP0 SETEPCONFIG P1 A/B (Run 116759.0)

The next isolated factory-XBL candidate enabled only the EP0 endpoint
configuration in-progress bit (`--xbl-ep0-config`), corresponding to XBL's
`SETEPCONFIG` P1 value `0x300` for the control endpoints. It retained the
known HS-attach-reaching ADB-to-Fastboot direct USB2 profile and did not enable
the already-negative XBL NORMAL data-TRB differential.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.116759.0/fullerene-bramble-boot.img`, SHA-256
`1457816ca2e64c036532060ef2a7f00a03a4cc251c495413af493e7b53f65b53`.
The host saw Fullerene high-speed attach at `10:13:33`, but the first Device
Descriptor timed out with `-110` at `10:13:38`; no `1234:0001` or Fullerene
descriptor appeared. Stock Android returned as SuperSpeed `18d1:4ee7` at
`10:13:59`, serial `26191JECB00076`, with `ro.boot.bootreason=watchdog`.
Evidence is in `tmp/fullerene-bramble-loop.116759.0/kernel-final.log` and
`android-fallback-usb.txt`. The XBL P1 in-progress-enable bit did not move the
attach-to-descriptor boundary.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 exact Factory XBL QMP table source audit

The exact same-build factory package in `Downloads` was inspected through its
packed `xbl_config`. Its effective `ss_phy_cfg_addr`/`ss_phy_cfg_val` sequence
has 146 QMP pairs. The current DT/Linux-derived table matches all entries
except the TXB block (indices 86..95): Factory XBL writes `0x16a4` first and
writes address `0x1684` twice, while the Linux labels place those writes in a
different address/order sequence. The new `--xbl-qmp-table` flag selects the
Factory sequence only for the SuperSpeed A/B; it does not alter the normal
fallback. This is static factory-image evidence, not a live MMIO trace.

### 2026-09-06 exact Factory XBL QMP table A/B (Run 154049.0)

The first physical A/B used the exact factory `xbl_config` QMP table with lane
A: `--super-speed --qmp-lane a --xbl-qmp-table --no-core-reset --no-smmu
--skip-typec-spmi`. QEMU preflight, image audit, ADB-to-Fastboot transition,
and `fastboot boot` all passed. The accepted artifact was
`tmp/fullerene-bramble-loop.154049.0/fullerene-bramble-boot.img`, SHA-256
`8fb93b083d39f108dc85588a89370f48ebfe5636b663382617604669a50e7f33`.

The host saw Fastboot disconnect at `10:42:04 JST`, Fullerene high-speed
attach at `10:42:15`, and the first Device Descriptor timeout (`-110`) at
`10:42:20`. No `1234:0001` or Fullerene descriptor appeared. Android returned
as stock SuperSpeed `18d1:4ee7` at `10:42:41`, serial `26191JECB00076`, with
watchdog recovery. Evidence is in
`tmp/fullerene-bramble-loop.154049.0/kernel-final.log` and
`android-fallback-usb.txt`. The exact Factory XBL QMP table did not move the
attach-to-descriptor boundary.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 exact Factory XBL HS-PHY table source audit

The exact `xbl_config` `hs_phy_cfg_addr` array contains valid Bramble entries
`0x6c, 0x70, 0x74, 0x78`. The MTP value bytes are `0x63, 0x85, 0x17, 0x03`,
and the hardware selector byte is `1`, so the XBL path selects MTP rather than
QRD. The stock DT-derived fallback previously applied only the first three
pairs. `--xbl-hs-phy-table` adds only the fourth write `0x78 <- 0x03`; this is
static factory-image evidence, not a live MMIO trace.

### 2026-09-06 exact Factory XBL HS-PHY fourth override A/B (Run 162215.0)

The next physical A/B added only the fourth Factory XBL HS-PHY override using
`--super-speed --qmp-lane a --xbl-hs-phy-table --no-core-reset --no-smmu
--skip-typec-spmi`. QEMU preflight, image audit, ADB-to-Fastboot transition,
and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.162215.0/fullerene-bramble-boot.img`, SHA-256
`53f3c86b7ef6789cfa767fc76a06d4529f6d287779b80c4856aadab7a02b6611`.

The host saw Fastboot disconnect at `10:48:25 JST`, Fullerene high-speed
attach at `10:48:36`, and Device Descriptor timeout `-110` at `10:48:41`.
No `1234:0001` or Fullerene descriptor appeared. Android returned as stock
SuperSpeed `18d1:4ee7` at `10:49:02`, serial `26191JECB00076`, with watchdog
recovery. Evidence is in
`tmp/fullerene-bramble-loop.162215.0/kernel-final.log` and
`android-fallback-usb.txt`. The fourth XBL HS-PHY override did not move the
attach-to-descriptor boundary.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 exact factory `vendor_boot` Linux USB driver audit

The `Downloads` package is the exact same-build Google factory image, not a
user-data backup. Its outer ZIP is
`/home/placeless/ダウンロード/bramble-up1a.231105.001.b2-factory-46a218d9.zip`
(SHA-256
`46a218d9dc2bf1802a584fa4e7e1b4d609a62189bd87a474201fb6d84e1c3e1c`), and
the extracted `vendor_boot.img` is the stock artifact already used here (SHA-256
`8af68ba6199cb6947fdb1e1f49cb2052537d284ef38e1ea1beb3c4a2ca7bc135`). The
vendor ramdisk contains the unstripped AArch64 modules `dwc3.ko`,
`dwc3-qcom.ko`, `usb-dwc3-msm.ko`, and `phy-msm-ssusb-qmp.ko`, with full
symbols. This is the closest available software evidence to the production
USB path; it does not require an analyzer and does not prove a live register
value by itself.

The matching qpr1 Linux source shows that `dwc3_gadget_run_stop(true)` first
sets up event buffers and runs `__dwc3_gadget_start` (EP0 descriptors,
resource window, DALEPENA, initial SETUP TRB, and DEVTEN), then writes the
maximum-speed field and DCTL RUN/STOP. The previous Run 167195 used an
ABL-oriented `--start-after-connect` order and therefore did not exercise this
source-aligned final restart boundary. The next A/B is explicitly scoped to
this ordering. No factory bootloader or partition is flashed.

### 2026-09-06 full Factory XBL/ABL USB2 profile A/B (Run 167195.0)

The exact factory-image-derived candidates were then combined: the XBL fourth
HS-PHY override plus the full ABL-derived USB2 handoff controls for shared
HS-PHY cleanup, endpoint configuration, command parameters, TRB flags, event
consumption, setup-TRB buffer, source-exact HS-PHY, and refreshed HS-PHY power.
The run used `--direct-handoff --no-smmu --start-after-connect --start-ungated
--android-resource-order --event-ring-size-4096 --abl-shared-hs-phy
--abl-ep-config --abl-command-params --abl-trb-flags --abl-event-consume
--abl-setup-trb-buffer --hsphy-source-exact --refresh-hsphy-power
--xbl-hs-phy-table --skip-typec-spmi`.

`fastboot boot` accepted
`tmp/fullerene-bramble-loop.167195.0/fullerene-bramble-boot.img`, SHA-256
`67508d312387251095111d9926eb48d59a1c7b7c6a55c05d7dca10b0a9648e4b`.
Fastboot disconnected at `10:51:57 JST`; Fullerene high-speed attach appeared
at `10:52:07`, but the first Device Descriptor timed out with `-110` at
`10:52:13`. No `1234:0001` or Fullerene descriptor appeared. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `10:52:33`, serial
`26191JECB00076`, with watchdog recovery. The complete static factory-derived
ABL/XBL profile therefore did not move the attach-to-descriptor boundary.
Evidence is in `tmp/fullerene-bramble-loop.167195.0/kernel-final.log` and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 source-aligned Linux USB2 final-restart A/B (Run 206207.0)

The exact-factory `vendor_boot`/qpr1 source audit was exercised with
`--direct-handoff --no-smmu --gadget-restart-at-runstop
--gadget-start-only-at-runstop --event-ring-size-4096
--android-resource-order --usb2-source-exact-runstop
--usb2-source-exact-devten --usb2-source-exact-cmd-guard
--usb2-dis-sleep-mode --usb2-android-dbm-reset --ep0-initial-512
--android-hs-lpm --android-lpm-errata --hsphy-source-exact
--xbl-hs-phy-table --skip-typec-spmi`. QEMU preflight, image audit, and
`fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.206207.0/fullerene-bramble-boot.img`, SHA-256
`a0bb4df7de12e54f4dfd8525db9bda532a9d01f4df868d3aca6a9ce90d6ae451`.

This profile produced no Fullerene USB attach after Fastboot disconnected at
`11:19:58 JST`; the host saw neither `1234:0001` nor the previous Fullerene
high-speed attach. Stock Android returned over SuperSpeed as `18d1:4ee7` at
`11:20:34 JST`, serial `26191JECB00076`, with watchdog recovery. This is a
negative result and points to the `gadget-start-only-at-runstop` ordering (or
its interaction with the restart path) as a new connection-boundary regression;
it does not disprove the Linux source sequence. Evidence is in
`tmp/fullerene-bramble-loop.206207.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 source-aligned Linux USB2 restart-only A/B (Run 210043.0)

The follow-up removed only `--gadget-start-only-at-runstop` while retaining
`--gadget-restart-at-runstop` and the same source/factory controls as Run
206207. QEMU preflight, image audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.210043.0/fullerene-bramble-boot.img`, SHA-256
`ff2c223d51963ca6c1e02ba339a4c1444b029a4fa9c8ec021eb276aeead22997`.

This also produced no Fullerene USB attach after Fastboot disconnected at
`11:22:22 JST`; the host saw neither `1234:0001` nor the prior Fullerene
high-speed attach. Stock Android returned over SuperSpeed as `18d1:4ee7` at
`11:22:58 JST`, serial `26191JECB00076`, with watchdog recovery. Removing
`--gadget-start-only-at-runstop` did not restore the connection boundary, so
the next baseline removes `--gadget-restart-at-runstop` as well. Evidence is
in `tmp/fullerene-bramble-loop.210043.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 source-control USB2 baseline without final restart (Run 213239.0)

The next control removed `--gadget-restart-at-runstop` as well, retaining the
source/factory controls from Runs 206207 and 210043. QEMU preflight, image
audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.213239.0/fullerene-bramble-boot.img`, SHA-256
`36aaf1fd08a2da1288ed61ad2c84d3e7eb00122d63edd0f76997d96c7766ff34`.

The host saw Fastboot disconnect at `11:24:10 JST`, but no Fullerene USB
attach, no `1234:0001`, and no Device Descriptor attempt. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `11:24:46 JST`, serial
`26191JECB00076`, with watchdog recovery. Thus the source-control set itself
does not reproduce the older attach-reaching profile; the next A/B restores
the known `--start-after-connect --start-ungated` timing controls before
changing another source-derived variable. Evidence is in
`tmp/fullerene-bramble-loop.213239.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 source-control timing retake (Run 217026.0)

The source-control set was retested with the known timing controls
`--start-after-connect --start-ungated`, while retaining the exact-factory
HS-PHY and qpr1-derived USB2 controls. QEMU preflight, image audit, and
`fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.217026.0/fullerene-bramble-boot.img`, SHA-256
`(recorded after run; see artifact.sha256)`.

Fastboot disconnected at `11:26:24 JST`; the host then saw a Fullerene
high-speed attach on `usb 1-9` followed by Device Descriptor `-110` at
`11:26:40 JST`. No `1234:0001` descriptor arrived before stock Android
`18d1:4ee7` fallback at `11:27:00 JST`. The timing controls therefore restore
the HS attach boundary, but not EP0 descriptor response; its ABL-derived
endpoint/TRB/event/setup controls remain a necessary unisolated part of the
attach-reaching baseline. Evidence is in
`tmp/fullerene-bramble-loop.217026.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 qpr1 USB2 device-core-reset-at-Run/Stop A/B (Run 220831.0)

The qpr1 `dwc3_gadget_pullup(true)` boundary was tested by adding
`--usb2-core-reset-at-runstop` to the source/factory USB2 controls. QEMU
preflight, image audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.220831.0/fullerene-bramble-boot.img`, SHA-256
`d2d95be013316c211b2ac2e0e2ae23d262772e2345b21f7fa2310a3c18faf47d`.

The host saw Fastboot disconnect at `11:29:06 JST`, but no Fullerene attach,
no `1234:0001`, and no descriptor attempt. Stock Android returned over
SuperSpeed as `18d1:4ee7` at `11:29:43 JST`, serial `26191JECB00076`, with
watchdog recovery. The reset-at-Run/Stop differential regressed before the
HS-attach boundary and is not retained as the next baseline. Evidence is in
`tmp/fullerene-bramble-loop.220831.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL endpoint/TRB profile without deferred initial SETUP (Run 223715.0)

The historical attach-reaching ABL-derived controls were retained
(`--start-ungated --android-resource-order --event-ring-size-4096
--abl-shared-hs-phy --abl-ep-config --abl-command-params --abl-trb-flags
--abl-event-consume --abl-setup-trb-buffer --hsphy-source-exact
--refresh-hsphy-power --xbl-hs-phy-table`), while removing
`--start-after-connect` so the initial SETUP could be armed before the final
RUN/STOP boundary. QEMU preflight, image audit, and `fastboot boot` passed.
Artifact: `tmp/fullerene-bramble-loop.223715.0/fullerene-bramble-boot.img`,
SHA-256 `5a64b82ebe9bd0b044069d1d85fee95129a90ccffeebc2927ca09fd14d530166`.

The host saw Fastboot disconnect at `11:30:54 JST`, but no Fullerene attach,
no `1234:0001`, and no descriptor attempt. Stock Android returned over
SuperSpeed as `18d1:4ee7` at `11:31:31 JST`, serial `26191JECB00076`, with
watchdog recovery. Arming the initial SETUP before this handoff's RUN/STOP
boundary does not work with the current ABL-derived endpoint profile; the
attach-reaching deferred-SETUP profile remains the physical baseline. Evidence
is in `tmp/fullerene-bramble-loop.223715.0/kernel-final.log`, `kernel.log`,
and `android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL profile with Connect Done-gated SETUP (Run 229817.0)

The attach-reaching Factory ABL endpoint/TRB/event profile was retained, while
the initial SETUP arm was moved to the Connect Done path with
`--start-at-connect-done --start-ungated`. The complete command was
`--direct-handoff --no-smmu --start-at-connect-done --start-ungated
--android-resource-order --event-ring-size-4096 --abl-shared-hs-phy
--abl-ep-config --abl-command-params --abl-trb-flags --abl-event-consume
--abl-setup-trb-buffer --hsphy-source-exact --refresh-hsphy-power
--xbl-hs-phy-table --skip-typec-spmi`. QEMU preflight, image audit, and
`fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.229817.0/fullerene-bramble-boot.img`, SHA-256
`61e088d7262d685a20ccba5fd81d2773037b64fee4c1425bdd66652ff63ee0b2`.

Fastboot disconnected at `11:34:57 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `11:35:08`, but the first Device Descriptor timed out
with `-110` at `11:35:13`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `11:35:34`, serial
`26191JECB00076`, with watchdog recovery. Connect Done-gated SETUP therefore
restores the HS attach boundary but not EP0 descriptor response. Evidence is
in `tmp/fullerene-bramble-loop.229817.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Corrected Factory ABL Connect Done-only SETUP A/B (Run 255300.0)

The source was corrected so `--start-at-connect-done` also suppresses the
bounded post-Run/Stop arm window; the first EP0 SETUP `STARTTRANSFER` can now
only be issued from the DWC3 Connect Done handler. The same Factory ABL/XBL
profile as Run 229817.0 was rebuilt from the exact factory `boot.img` with
`--adb-reboot-to-fastboot` used only to enter Fastboot. QEMU preflight, image
audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.255300.0/fullerene-bramble-boot.img`, SHA-256
`9915757e5bd2cbe8b8e963fa0bd91282a75c1ed9efed13866b58296be37c4f75`.

Fastboot disconnected at `11:53:06 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `11:53:19`, but the first Device Descriptor timed out
with `-110` at `11:53:24`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `11:53:44`, serial
`26191JECB00076`, with watchdog recovery. Strict Connect Done-only arm timing
therefore still leaves the HS attach-to-EP0-response boundary unchanged.
Evidence is in `tmp/fullerene-bramble-loop.255300.0/kernel-final.log`,
`kernel.log`, and `android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory-DTB `tx-fifo-resize` A/B (Run 260701.0)

The corrected Connect Done-only profile was retained and the exact factory
DTB's `tx-fifo-resize` behavior was exercised through
`--ep0-txfifo-fix`, which raises a degenerate EP0 IN FIFO to depth 32 while
preserving its start address. The exact factory `boot.img` was used;
`--adb-reboot-to-fastboot` only entered Fastboot. QEMU preflight, image audit,
and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.260701.0/fullerene-bramble-boot.img`, SHA-256
`c2e880b0a53d85ea9595aeaef8933a84ee67167be6abb5a89628d5ec28d88e0e`.

Fastboot disconnected at `11:56:43 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `11:56:54`, but the first Device Descriptor timed out
with `-110` at `11:56:59`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `11:57:20`, serial
`26191JECB00076`, with watchdog recovery. Correcting the possible EP0 IN FIFO
depth did not move the attach-to-EP0-response boundary. Evidence is in
`tmp/fullerene-bramble-loop.260701.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL profile plus exact qpr1 device-core reset (Run 265196.0)

The attach-reaching Factory ABL/XBL endpoint profile was retained and
`--usb2-core-reset-at-runstop --usb2-source-exact-device-reset` was added.
This applies the qpr1 raw `DCTL.CSFTRST`, 1-ms polling, and post-reset doorbell
clear at the final USB2 boundary, while the earlier handoff reset also uses
the source-exact helper. The exact factory `boot.img` was used;
`--adb-reboot-to-fastboot` only entered Fastboot. QEMU preflight, image audit,
and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.265196.0/fullerene-bramble-boot.img`, SHA-256
`956e636db2f01938d20e72aaa3514e3d46d70841fe4d8ec73406dc03c3706e86`.

Fastboot disconnected at `11:59:47 JST`; no Fullerene HS attach or descriptor
attempt appeared. Stock Android returned over SuperSpeed as `18d1:4ee7` at
`12:00:23`, serial `26191JECB00076`, with watchdog recovery. The exact qpr1
device-core reset sequence regressed before the known HS attach boundary in
this ABL combination. Evidence is in
`tmp/fullerene-bramble-loop.265196.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL profile plus exact XBL EP0 DMA objects (Run 268780.0)

The Factory ABL endpoint/TRB/event profile was retained and the XBL-observed
fixed DMA objects were selected with `--xbl-event-dma --xbl-stock-ep0-dma`:
the event ring at `0x0a6fc010` and the stock EP0 setup/TRB locations. The
initial SETUP remained strictly Connect Done-gated. The exact factory
`boot.img` was used; `--adb-reboot-to-fastboot` only entered Fastboot. QEMU
preflight, image audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.268780.0/fullerene-bramble-boot.img`, SHA-256
`ec40edddaf788b3f63c128da8d7ee3c9e3f28acd6dcd0332ef327500255c7a84`.

Fastboot disconnected at `12:02:13 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `12:02:23`, but the first Device Descriptor timed out
with `-110` at `12:02:29`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `12:02:49`, serial
`26191JECB00076`, with watchdog recovery. The exact XBL DMA-object addresses
did not move the attach-to-EP0-response boundary. Evidence is in
`tmp/fullerene-bramble-loop.268780.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL profile with normal Apps-SMMU path (Run 271311.0)

The exact factory `boot.img` and the corrected Factory ABL endpoint/TRB/event
profile were retained, including strict Connect Done-gated initial SETUP. This
run removed the diagnostic `--no-smmu` bypass and exercised the normal DT/SMMU
path. `--adb-reboot-to-fastboot` only entered Fastboot. QEMU preflight, image
audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.271311.0/fullerene-bramble-boot.img`, SHA-256
`ee29b0c16b5032c2d002ff0dce6dacdf5bb600de63b5ee2082a0f5edcc1a86ee`.

Fastboot disconnected at `12:03:54 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `12:04:04`, but the first Device Descriptor timed out
with `-110` at `12:04:09`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `12:04:30`, serial
`26191JECB00076`, with watchdog recovery. The normal Apps-SMMU path did not
move the attach-to-EP0-response boundary; this also agrees with the earlier
non-Factory-ABL SMMU control (`1393139.0`). Evidence is in
`tmp/fullerene-bramble-loop.271311.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 Factory ABL profile plus adopted live Apps-SMMU page (Run 276377.0)

The exact factory `boot.img`, Factory ABL endpoint/TRB/event profile, and
strict Connect Done-gated initial SETUP were retained. The normal SMMU path was
kept enabled, while `--dma-adopt-smmu` read the live Fastboot Apps-SMMU
translation context and relocated Fullerene's EP0 DMA objects into a page
already mapped by that context. `--adb-reboot-to-fastboot` only entered
Fastboot. QEMU preflight, image audit, and `fastboot boot` passed. Artifact:
`tmp/fullerene-bramble-loop.276377.0/fullerene-bramble-boot.img`, SHA-256
`b1b0db92a68187effc11766f81c9cbccdc2fa11ae6cfe6d96bb8e5f0bcff8dbd`.

Fastboot disconnected at `12:07:34 JST`; the host saw Fullerene high-speed
attach on `usb 1-9` at `12:07:44`, but the first Device Descriptor timed out
with `-110` at `12:07:49`. No `1234:0001` descriptor arrived. Stock Android
returned over SuperSpeed as `18d1:4ee7` at `12:08:10`, serial
`26191JECB00076`, with watchdog recovery. Adopting a live mapped SMMU page did
not move the attach-to-EP0-response boundary. Evidence is in
`tmp/fullerene-bramble-loop.276377.0/kernel-final.log`, `kernel.log`, and
`android-fallback-usb.txt`.

The run used `fastboot boot` only. No partition was read or written, no
user-data backup was created, and no analyzer, flash, erase, or secure-debug
operation was used.

### 2026-09-06 — exact Factory image EUD-gate override A/B (Run `294061.0`)

- The exact same-build Factory ABL/XBL USB2 profile was rerun with only `FULLERENE_AARCH64_USB_HSPHY_IGNORE_EUD=1`, overriding the DT `eud_enable_reg` early-return gate. The command retained `--direct-handoff --start-at-connect-done --start-ungated --no-smmu --android-resource-order --event-ring-size-4096 --abl-shared-hs-phy --abl-ep-config --abl-command-params --abl-trb-flags --abl-event-consume --abl-setup-trb-buffer --hsphy-source-exact --xbl-hs-phy-table --skip-typec-spmi`.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.294061.0/fullerene-bramble-boot.img`; artifact SHA-256 `413456ab1b81c92ca69639933ff45d1a248c880ec7bdf4260173979cc3fb1f0a`.
- Host result: Fastboot disconnected at `12:19:40 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `12:19:50`; Device Descriptor timed out with `-110` at `12:19:56`; stock Android SuperSpeed `18d1:4ee7` returned at `12:20:16`, serial `26191JECB00076`; watchdog recovery. No `1234:0001`.
- Conclusion: overriding the factory DT EUD ownership gate and forcing the source-exact femto-HS-PHY init did not move the attach-to-EP0-response boundary. This does not support EUD ownership as the sole blocker.
- Evidence: `tmp/fullerene-bramble-loop.294061.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, and `boot-reason.txt`. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — exact Factory image DT/Type-C path A/B (Run `298642.0`)

- The exact same-build Factory ABL/XBL USB2 profile was rerun with `--skip-typec-spmi` removed, allowing the direct handoff to use its DT-selected Type-C/SPMI path. All other Factory ABL/XBL, Connect Done, source-exact HS-PHY, and no-SMMU controls were held constant.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.298642.0/fullerene-bramble-boot.img`; artifact SHA-256 `d69e1531193cc2d52366c8e490ce5d3fadf0fdd4aa24c5b5a7a556db5b521543`.
- Host result: Fastboot disconnected at `12:22:47 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `12:22:57`; Device Descriptor timed out with `-110` at `12:23:02`; stock Android SuperSpeed `18d1:4ee7` returned at `12:23:23`, serial `26191JECB00076`; watchdog recovery. No `1234:0001`.
- Conclusion: enabling the DT/Type-C/SPMI route did not move the attach-to-EP0-response boundary. The negative result does not support `--skip-typec-spmi` as the sole cause.
- Evidence: `tmp/fullerene-bramble-loop.298642.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, and `boot-reason.txt`. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — exact Factory image DMA cache-maintenance A/B (Run `302105.0`)

- The exact same-build Factory ABL/XBL USB2 profile was rerun with `--dma-cache-maintenance`, forcing `dc cvac`/`dc ivac` for the DT-selected USB DMA objects even though the Factory DT declares `qcom,gsi-disable-io-coherency`. All other Connect Done, endpoint/TRB/event, source-exact HS-PHY, XBL HS-PHY, no-SMMU, and Type-C-skip controls were held constant.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.302105.0/fullerene-bramble-boot.img`; artifact SHA-256 `f59788d70bec3a87bfa59cf591ef89bdf464d2ab99819cfb8b3fcf7542002228`.
- Host result: the prior Android device disconnected at `12:25:00 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `12:25:10`; Device Descriptor timed out with `-110` at `12:25:16`; stock Android SuperSpeed `18d1:4ee7` returned at `12:25:36`, serial `26191JECB00076`; watchdog recovery. No `1234:0001`.
- Conclusion: explicit cache maintenance did not move the attach-to-EP0-response boundary. The exact Factory DT DMA coherency contract is not sufficient by itself to reach descriptor enumeration.
- Evidence: `tmp/fullerene-bramble-loop.302105.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, and `boot-reason.txt`. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — exact runtime DTBO selection and HS-PHY readout (Run `321947.0`)

- Android read-only `getprop` on the connected handset reported `ro.boot.dtbo_idx=17`, `ro.boot.hardware=bramble`, `ro.boot.hardware.revision=MP1.0`, and the exact Factory fingerprint `google/bramble/bramble:14/UP1A.231105.001.B2/11260668:user/release-keys`. Factory `dtbo.img` entry 17 is `Google Inc. MSM sm7250 v2 Bramble PVT` and overlays `qcom,param-override-seq = <0x67 0x6c 0xc8 0x70>`.
- The exact Factory ABL/XBL USB2 profile was rerun with the read-only `FULLERENE_AARCH64_USB_UTMI_PRECONNECT_READOUT=hsphy-table` timing readout. `fastboot boot` accepted `tmp/fullerene-bramble-loop.321947.0/fullerene-bramble-boot.img`; artifact SHA-256 `6e80d8360804777887f19c68bc90e4c273923c45bf19c320baa37df2bdd2000d`.
- Host result: Fastboot disconnected at `12:37:58 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `12:38:09`; Device Descriptor timed out with `-110` at `12:38:14`; stock Android SuperSpeed `18d1:4ee7` returned at `12:38:35`, serial `26191JECB00076`; watchdog recovery. No `1234:0001`.
- Conclusion: the factory image and the handset agree on DTBO index 17, but the diagnostic readout did not move the attach-to-EP0-response boundary. The coarse host timestamp is not used as a stronger claim about the retained readout code than the direct `dtbo_idx=17` evidence.
- Evidence: `tmp/fullerene-bramble-loop.321947.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, the exact `dtbo.img`, and the read-only `adb shell getprop` result. No partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — explicit Factory `dtbo_idx=17` HS-PHY overlay A/B (Run `326975.0`)

- Added an opt-in build flag, `FULLERENE_AARCH64_USB_HSPHY_DTBO_BRAMBLE_PVT=1`, which forces exactly the Factory DTBO entry 17 two-pair override `<0x67 0x6c 0xc8 0x70>` and removes the base DTB TUNE3 pair. This makes the overlay a direct physical differential even if a boot path supplies only the base DTB.
- The same exact Factory ABL/XBL USB2 profile was run with that flag. `fastboot boot` accepted `tmp/fullerene-bramble-loop.326975.0/fullerene-bramble-boot.img`; artifact SHA-256 `103e522e3fb4dad2f2fb579bd6556a31ff734f4b1b02f01c95a0121213f0d2c1`.
- Host result: Fastboot disconnected at `12:41:17 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `12:41:28`; Device Descriptor timed out with `-110` at `12:41:33`; stock Android SuperSpeed `18d1:4ee7` returned at `12:41:53`, serial `26191JECB00076`; watchdog recovery. No `1234:0001`.
- Conclusion: forcing the handset-selected Factory PVT overlay values does not move the attach-to-EP0-response boundary. The factory DTBO overlay is therefore not the missing single change for descriptor enumeration; keep the flag as a reproducible control, not as the default.
- Evidence: `tmp/fullerene-bramble-loop.326975.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, and `boot-reason.txt`. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — Factory DTBO plus Type-C/SPMI retake with passive usbmon (Run `340760.0`)

- The exact Factory ABL/XBL USB2 profile was rerun with the handset-selected Bramble PVT pair set forced by `FULLERENE_AARCH64_USB_HSPHY_DTBO_BRAMBLE_PVT=1`. This time `--skip-typec-spmi` was removed so the DT-selected Type-C/SPMI path was also active; `/dev/usbmon1` was captured passively in parallel.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.340760.0/fullerene-bramble-boot.img`; artifact SHA-256 `f87a0e3a63576176c76fafe49cd0a1464f49bd3377c6813019570623c36cc435`.
- Host result: Fullerene high-speed attach appeared on `usb 1-9` at `12:51:24 JST`; Device Descriptor timed out with `-110` at `12:51:29`; stock Android returned after watchdog recovery. No `1234:0001`.
- Passive capture `/tmp/fullerene-usbmon-run-20260906-typec-pvt.1u.bin` is 6475 bytes with SHA-256 `827290ae18d02cc09ec8eb112b9f7df77bc9a22b06523252419a4444340dd49d`. This is host software observation only, not an external analyzer or a wire-level decode.
- Conclusion: enabling Type-C/SPMI while forcing the exact Factory PVT overlay pair set did not move the attach-to-EP0-response boundary.
- Evidence: `tmp/fullerene-bramble-loop.340760.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. No partition/user-data backup, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe bootloader-DTB install correction and A/B (Run `348905.0`)

- Source audit corrected a material assumption: the physical binary is the separate `fullerene-kernel-aarch64-usb-probe` (`usb_probe.rs`), not the normal AArch64 kernel path. Before this change it had no DTB parser/installer, so the factory DTBO property found in `main.rs` was not automatically reaching the hardware probe.
- The probe now receives the bootloader-preserved AArch64 `x0` DTB address, parses `qcom,usb-hsphy-snps-femto/qcom,param-override-seq`, and installs that HS-PHY sequence before USB initialization. Build check passed for the Bramble USB-probe target; the implementation was intentionally limited to the DT-selected HS-PHY property and did not claim a full DT contract.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.348905.0/fullerene-bramble-boot.img`; artifact SHA-256 `991e151025682deb98c0897f82f44df64a88441806e072050e02a97ba07edb2a`.
- Host result: Fullerene high-speed attach appeared on `usb 1-9` at `12:57:04 JST`; `device descriptor read/64, error -110` occurred at `12:57:09`; stock Android fallback followed watchdog recovery. Passive capture `/tmp/fullerene-usbmon-run-20260906-dtb-probe.1u.bin` is 6376 bytes with SHA-256 `6f911e2e18df12cdb3b6537069257b792f8728fcdbfca5832c072f6b95927fe7`. No `1234:0001`.
- Conclusion: correcting the missing probe-side DTB install path did not yet make the device enumerate. The factory image remains useful evidence, but its DTBO HS-PHY property is not the sole missing change at the descriptor boundary.
- Evidence: `tmp/fullerene-bramble-loop.348905.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe x0/x2 DTB selection and HS-PHY readout A/B (Run `357179.0`)

- The probe assembly was tightened to preserve both Android boot arguments through relocation and pass them to Rust. Its DTB installer now selects the first valid FDT from `x0`, then `x2`, matching the normal kernel's guarded fallback order. The build also enabled the read-only `FULLERENE_AARCH64_USB_UTMI_PRECONNECT_READOUT=hsphy-table` selector.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.357179.0/fullerene-bramble-boot.img`; artifact SHA-256 `7be5c05a0b61865f0d97b588f8258050f3ceead803bcee89fabee9d1ae3631ac`.
- Host result: Fullerene high-speed attach appeared on `usb 1-9` at `13:03:26 JST`; `device descriptor read/64, error -110` occurred at `13:03:31`; stock Android fallback followed watchdog recovery. No `1234:0001`.
- Passive capture `/tmp/fullerene-usbmon-run-20260906-dtb-x2-readout-2.1u.bin` is 6475 bytes with SHA-256 `e02bf62287b08e8c7150a401e9e4665f39cf620124a28e18182a26c3b8c3f740`.
- Conclusion: the x0/x2 DTB selection and HS-PHY source readout did not move the attach-to-EP0-response boundary. The remaining static audit must compare the full Factory DT contract used by `main.rs` against the independently linked probe path.
- Evidence: `tmp/fullerene-bramble-loop.357179.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe full Factory DT contract A/B (Run `365586.0`)

- The probe-side DT installer was extended beyond HS-PHY: it now collects the Factory DT's DWC3/PHY/SMMU/PDC regions, DMA pool and stream ID, QMP register offsets/init sequence, GSI layout/coherency flag, IRQ tuples, Type-C/SPMI resources, clock rates, bus dimensions/vectors, voltage properties, GDSC, and EUD resource, then calls the same `install_usb_resource_contract()` used by the normal kernel path.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.365586.0/fullerene-bramble-boot.img`; artifact SHA-256 `17ad71ed024bbeff68944d42e2811a097aeab599d701460cca33f61d0c4208d3`.
- Host result: Fastboot disconnected at `13:09:22 JST`; no Fullerene `usb 1-9` attach or descriptor request appeared; stock Android SuperSpeed `18d1:4ee7` returned at `13:10:48`, serial `26191JECB00076`. No `1234:0001`.
- `/tmp/fullerene-usbmon-run-20260906-full-dt-contract.1u.bin` is empty because the device never entered the bus-1 HS attach state. This is a host software capture, not an analyzer result.
- Conclusion: the full-contract implementation regresses before the previously stable HS attach. The next A/B must isolate QMP-table installation from address/resource installation; the full bundle is not retained as a success candidate.
- Evidence: `tmp/fullerene-bramble-loop.365586.0/kernel.log`, `boot.log`, `fastboot-usb-tree.txt`, and the empty passive capture. The run was `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe DT_QMP-only A/B (Run `371918.0`)

- The probe-side Factory DT contract was split into two opt-in pieces so QMP initialization could be tested independently from address/resource overrides. This run enabled only `FULLERENE_AARCH64_USB_PROBE_DT_QMP=1`; the DT-derived resource bundle remained disabled, while the existing bootloader-DTB HS-PHY install and exact Factory ABL/XBL USB2 profile stayed enabled.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.371918.0/fullerene-bramble-boot.img`; artifact SHA-256 `2d3be6e384d12843061e44279f9419b1548b560dfa867ef6b962728d8e911134`.
- Host result: Fastboot disconnected at `13:13:40 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `13:14:18`; `device descriptor read/64, error -110` occurred at `13:14:24`; stock Android fallback followed watchdog recovery. No `1234:0001`.
- Passive `/dev/usbmon1` capture `/tmp/fullerene-usbmon-run-20260906-dt-qmp-only.1u.bin` is 6376 bytes with SHA-256 `22829922af1bb3c10cb8fdd71092eb383fd7567d6a24f63e105937f602f710c0`. This is host software observation only, not an external analyzer or wire-level decode.
- Conclusion: installing the DT QMP init sequence alone did not remove the stable HS-attach/EP0-descriptor-timeout boundary. The next isolation run should enable only `FULLERENE_AARCH64_USB_PROBE_DT_RESOURCES=1`.
- Evidence: `tmp/fullerene-bramble-loop.371918.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. The run was `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe DT_RESOURCES-only A/B (Run `380175.0`)

- The complementary isolation run enabled only `FULLERENE_AARCH64_USB_PROBE_DT_RESOURCES=1`; the probe installed the Factory DT address/resource contract but retained the compiled QMP init table. The bootloader-DTB HS-PHY install and exact Factory ABL/XBL USB2 profile were unchanged.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.380175.0/fullerene-bramble-boot.img`; artifact SHA-256 `1ac0a5812c0eabc39bc3f7df05303f5c39457b3f65cf86f0bab97873c0683d86`.
- Host result: Fastboot disconnected at `13:20:04 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `13:20:51`; `device descriptor read/64, error -110` occurred at `13:20:56`; stock Android fallback followed watchdog recovery. No `1234:0001`.
- Passive `/dev/usbmon1` capture `/tmp/fullerene-usbmon-run-20260906-dt-resources-only.1u.bin` is 6376 bytes with SHA-256 `b11dc03bf782310c4c673555c2abb9a5ec4a7a29bbe6803d0927308f549155c8`. This is host software observation only, not an external analyzer or wire-level decode.
- Conclusion: the DT resource/address contract alone also preserves the stable HS-attach/EP0-descriptor-timeout boundary. Together with Run `371918.0`, this isolates the regression in the interaction between DT-derived resources and DT-derived QMP sequence when both are installed, not in either component alone.
- Evidence: `tmp/fullerene-bramble-loop.380175.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. The run was `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — standalone probe full DT contract split-flag repeat (Run `389938.0`)

- Repeated the full combination after the two one-component controls: both `FULLERENE_AARCH64_USB_PROBE_DT_QMP=1` and `FULLERENE_AARCH64_USB_PROBE_DT_RESOURCES=1` were enabled, with the same exact Factory ABL/XBL USB2 profile. This checks whether the earlier no-attach result was reproducible or a warm-boot timing fluctuation.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.389938.0/fullerene-bramble-boot.img`; artifact SHA-256 `6f601dd6d5d8662f00c08655f950f75f0c500dddd434a4f5d7ba0ecee2508e96`.
- Host result: Fastboot disconnected at `13:27:13 JST`; Fullerene high-speed attach appeared on `usb 1-9` at `13:27:59`; `device descriptor read/64, error -110` occurred at `13:28:05`; stock Android fallback followed watchdog recovery. No `1234:0001`.
- Passive `/dev/usbmon1` capture `/tmp/fullerene-usbmon-run-20260906-dt-full-split-repeat.1u.bin` is 6376 bytes with SHA-256 `e840a9b20647505f0694d49f9140512e2ec5f248b99c0ddf93fef9e56e9f6a88`. This is host software observation only, not an external analyzer or wire-level decode.
- Conclusion: the full combination is not deterministically an attach regression; this repeat restored the usual HS-attach/EP0-descriptor-timeout boundary. The earlier no-attach full-contract result remains a separate negative warm-boot outcome, not a proven DT-value mismatch.
- Evidence: `tmp/fullerene-bramble-loop.389938.0/kernel-final.log`, `kernel.log`, `android-fallback-usb.txt`, `boot-reason.txt`, and the passive usbmon capture. The run was `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — normal AArch64 Factory DT path retake (Run `399346.0`)

- Booted the exact extracted Factory `boot.img` through the normal AArch64 entry (`--normal`) to compare the current `main.rs` DT discovery/install path with the standalone probe. The probe-only `FULLERENE_AARCH64_USB_PROBE_DT_QMP` and `FULLERENE_AARCH64_USB_PROBE_DT_RESOURCES` flags were not set.
- `fastboot boot` accepted `tmp/fullerene-bramble-loop.399346.0/fullerene-bramble-boot.img`; artifact SHA-256 `99ef3c6988000c149ed6b3c4dfeaa974f5085cae79501b0fab5fb07a45d6e5e`.
- Host result: the handset disconnected from Fastboot at `13:33:53 JST` (`usb 2-1`); no Fullerene HS attach, descriptor request, or Android recovery appeared during the 100-second harness window, which ended with `RUN_RC=124`. No `1234:0001`.
- Passive `/dev/usbmon1` capture `/tmp/fullerene-usbmon-run-20260906-normal-factory-dt.1u.bin` is empty with SHA-256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`. This is host software observation only, not an external analyzer or wire-level decode.
- Conclusion: the current normal AArch64 path also did not enumerate, but it failed before the standalone probe's usual HS-attach boundary. This is a distinct negative result, not evidence that the Factory DT values themselves are wrong.
- Evidence: `tmp/fullerene-bramble-loop.399346.0/boot.log`, `kernel.log`, `fastboot-usb-tree.txt`, `lsusb-timeline.txt`, and the passive capture. `fastboot boot` only; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — Factory Android USB gadget path audit and temporary-ID boot preparation

- The exact same-build Factory `vendor.img` was inspected read-only with `debugfs`. `/etc/init/hw/init.sm7250.usb.rc` is the production Android path: it creates `/config/usb_gadget/g1`, writes `idVendor 0x18d1`, and hands the configured functions to `a600000.dwc3`. Its `build.prop` contains `ro.recovery.usb.vid=18D1`, `ro.recovery.usb.adb.pid=D001`, and `ro.recovery.usb.fastboot.pid=4EE0`; this confirms that the factory image contains the USB identity policy as well as the DT and Qualcomm modules.
- A repeatable non-flashing builder, [`tools/build_bramble_gadget_boot.py`](../tools/build_bramble_gadget_boot.py), was added. It keeps the Factory kernel, vendor_boot, DTB, and original ramdisk files, appends a root `init.rc` action, and changes only the live configfs gadget identity to `0x1234:0x0001` after `sys.usb.state` is published. It does not modify `vendor.img`, `vendor_boot.img`, or any phone partition.
- The initial static image used `system/etc/init/zz-fullerene-usb-id.rc`; its physical A/B was negative because the device returned as stock Android `18d1:4ee7`. The builder is now prepared to place the same seven `idVendor=0x1234` and seven `idProduct=0x0001` actions at root `init.rc`, which is present in the boot ramdisk's init parse scope. No partition was modified.
- After Run `399346.0` timed out, the operator manually returned the connected handset to Fastboot. Read-only host status then showed serial `26191JECB00076` as `fastboot`; no user-data backup or partition operation was performed.

### 2026-09-06 — Factory boot-ramdisk system/etc/init placement A/B (manual stock-image run)

- The first stock-kernel/vendor_boot/DT temporary image was booted with the `0x1234:0x0001` configfs actions at `system/etc/init/zz-fullerene-usb-id.rc`. `fastboot boot` accepted artifact SHA-256 `dc5517cceb84be1247f8426a85c0d52ab8ab597362d239ab05e0f3b4ddb95162` at `13:58:59 JST`; this was a transient boot only.
- Host udev observed Fastboot `18d1:4ee0` removal followed by stock Android `18d1:4ee7` return. No `1234:0001` event appeared during the 77-second observation. The unprivileged host could not read `dmesg` (`Operation not permitted`); udev and final ADB evidence are retained under `/tmp/fullerene-bramble-stock-gadget-1234/`.
- Conclusion: the `system/etc/init` placement did not produce the requested identity, consistent with that path being hidden by the mounted system partition or not loaded for this boot. The next A/B keeps the same image contents and moves only the init action to root `init.rc`.

### 2026-09-06 — root init selection audit and command-line override preparation

- The root `init.rc` A/B was also negative: Android returned as stock `18d1:4ee7`, so merely adding a root file did not select it. The exact boot ramdisk contains the first-stage `init`; the Android 14 init binary's normal `LoadBootScripts` path loads `/system/etc/init/hw/init.rc`, and only uses an alternate script when `ro.boot.init_rc` is non-empty.
- This matches the Android Open Source Project init implementation: [`LoadBootScripts`](https://android.googlesource.com/platform/system/core/+/refs/tags/android-14.0.0_r3/init/init.cpp#341) parses the system primary rc by default and parses only the `ro.boot.init_rc` path in the alternate branch. The next transient boot therefore sets `androidboot.init_rc=/init.rc` in the supplied boot command line and makes root `init.rc` import the standard system/vendor/odm/product rc directories before the seven temporary ID actions.
- No phone partition, vendor image, userdata, or persistent boot configuration is modified; the command-line override applies only to the next `fastboot boot` session.

### 2026-09-06 — selected root init via transient androidboot.init_rc A/B (manual stock-image run)

- Booted the exact Factory kernel/vendor_boot/DT image with root `init.rc`, standard rc imports, and boot-header command line `androidboot.init_rc=/init.rc`. `fastboot boot` accepted artifact SHA-256 `5295eee24a393f37036e9b69f383e4ffc9392fcbc457e457e4bf1195db5a34f9` at `14:13:59 JST`; this was a transient boot only.
- Host udev observed Fastboot `18d1:4ee0` removal. During the 77-second observation there was no `1234:0001` event and no stock Android `18d1:4ee7` return; the USB gadget did not reappear. The handset was therefore not left in the requested state and needs manual return to Fastboot before another physical A/B.
- Conclusion: selecting root `init.rc` changed the boot path enough to suppress the normal USB fallback, but did not yet prove that the custom rc executed successfully. The next correction avoids importing the whole rc directories from the alternate branch; root `init.rc` imports only the stock `/system/etc/init/hw/init.rc`, whose own imports already select the Bramble/SM7250 USB rc files, then registers the temporary property actions.

### 2026-09-06 — selected root init primary-only import A/B (manual stock-image run)

- Repeated the transient `androidboot.init_rc=/init.rc` boot with root `init.rc` importing only `/system/etc/init/hw/init.rc`; the primary script's own imports select the Bramble/SM7250 USB rc files. Artifact SHA-256: `92abcfa38a7373882effbc54b0e914e8af0df19d20c547bba68c8fc1f3a7df57`; `fastboot boot` accepted it at `14:17:50 JST`.
- Host udev saw Fastboot `18d1:4ee0` removal and then no USB device for 77 seconds. Neither `1234:0001` nor stock Android `18d1:4ee7` appeared. This reproduces the no-fallback result of the previous alternate-init trial, so duplicate directory imports were not the sole cause.
- The device must be manually returned to Fastboot before the next physical trial. The next path will keep the normal init script selection and target a file that is actually parsed from the vendor ramdisk/recovery path, rather than replacing `ro.boot.init_rc` for the whole boot.

### 2026-09-06 — recovery ramdisk host-command construction check

- Built the factory recovery ramdisk with the three temporary `sys.usb.state` rebind actions. The host-side `fastboot boot KERNEL RAMDISK` construction was first attempted with the complete Factory vendor command line, but the installed fastboot client rejected it before download with `command line too large: 569` for header v2.
- No bytes were sent to the handset in this check; the phone remained Fastboot `18d1:4ee0`. The next command retains only the essential DT-selected UFS boot-device and DWC3 controller arguments, staying within the header-v2 command-line limit.

### 2026-09-06 — header-v2 recovery path rejected and v3 recovery overlay prepared

- With the shortened command line, the host built and downloaded a 93,548,544-byte header-v2 image from the exact stock kernel, factory DTB, and modified recovery ramdisk, but the bootloader rejected it before boot with `Error verifying the received boot.img: Unsupported`. The handset remained Fastboot `18d1:4ee0`; no partition was written.
- The v2 result is consistent with this Android 11 GKI device requiring the v3 boot format. A new v3 overlay builder, [`tools/build_bramble_recovery_overlay_boot.py`](../tools/build_bramble_recovery_overlay_boot.py), keeps the v3 stock boot image, overlays the factory recovery `/system/etc/init/hw/init.rc` from the generic ramdisk, appends the three temporary rebind actions, and sets only transient `androidboot.force_normal_boot=0`.
- Static check passed for artifact SHA-256 `3f9016f2ba8d87ec153e14412f9ae3fc795a406db41487b9a01e2be97bc1e949`; the v3 ramdisk/cpio contains the recovery init overlay and three `idVendor=0x1234`/`idProduct=0x0001` state hooks.

### 2026-09-06 — v3 recovery overlay physical A/B

- Booted the v3 recovery-overlay artifact with `fastboot boot`; the bootloader accepted SHA-256 `3f9016f2ba8d87ec153e14412f9ae3fc795a406db41487b9a01e2be97bc1e949` at `14:31:50 JST`.
- Host udev saw Fastboot `18d1:4ee0` removal followed by stock Android `18d1:4ee7` at `14:32` and ADB returned. No `1234:0001` event appeared. The generic overlay did not make the Pixel bootloader select its vendor recovery ramdisk; `androidboot.force_normal_boot=0` in the supplied v3 header was not sufficient in this path.
- Conclusion: the recovery init overlay is statically valid but is not the active normal-boot init path. The next no-flash check uses the already-running stock recovery entry (`adb reboot recovery`) and, only if recovery ADB is available, performs the same temporary configfs ID operation from userspace.

### 2026-09-06 — stock recovery ADB/configfs userspace check

- Entered the stock recovery with `adb reboot recovery` at `14:34:01 JST`; host udev observed stock recovery `18d1:d001`, and `adb devices` reported the handset in `recovery` state.
- Recovery `adb shell` could not execute even `id`: stock `adbd` aborted while setting the shell SELinux context (`shell_service.cpp:380`, `Could not set SELinux context for subprocess`). `adb root` was rejected as a production build. ADB sync direct pushes to `/config/usb_gadget/g1/{UDC,idVendor,idProduct}` were also denied before opening the configfs attributes.
- No `1234:0001` event occurred and the recovery gadget remained `18d1:d001`. No partition, userdata, vendor_boot, or recovery image was written.

### 2026-09-06 — transient boot-ramdisk debug-property A/B preparation

- The stock recovery ADB path cannot perform the runtime configfs write because production `adbd` rejects root and the shell context setup aborts. The next reversible path keeps the exact Factory kernel/vendor_boot/DT and appends only `ro.debuggable=1` and `ro.secure=0` to the stock boot ramdisk's existing `system/etc/ramdisk/build.prop`; no system/vendor file or phone partition is changed.
- New [`tools/build_bramble_debuggable_boot.py`](../tools/build_bramble_debuggable_boot.py) passed `py_compile`, rebuilt the v3 boot image, and round-tripped the cpio/LZ4 archive. Static checks found both properties in the target entry. Artifact: `/tmp/bramble-factory-audit/extracted/boot-1234-debuggable.img`, SHA-256 `901195979c9cf044f01d80c6e8e1ef92c9f2a0b835afadc96b5c1dd96af632c6`; archive `3,736,676` bytes, ramdisk `3,751,339` bytes.
- This is an A/B preparation only; the physical `fastboot boot` and the resulting `adb root`/configfs test are the next recorded step. The handset is currently normal Android (`adb` serial `26191JECB00076`), so it must be returned to Fastboot before the transient boot. No partition/user-data backup, analyzer, flash, erase, or secure-debug operation is used.

### 2026-09-06 — transient boot-ramdisk debug-property physical A/B

- Returned the connected handset to Fastboot with `adb reboot bootloader`, then `fastboot boot` accepted `/tmp/bramble-factory-audit/extracted/boot-1234-debuggable.img` (SHA-256 `901195979c9cf044f01d80c6e8e1ef92c9f2a0b835afadc96b5c1dd96af632c6`). The phone returned to normal Android; host udev saw the expected transient Fastboot `18d1:4ee0` removal followed by stock Android `18d1:4ee7`.
- Runtime readback was `ro.debuggable=0` and `ro.secure=1`; `adb root` still returned `adbd cannot run as root in production builds`, and `adb shell id` remained uid 2000 (`shell`). No configfs write was attempted because the required privileged channel was not available. No `1234:0001` event occurred.
- Conclusion: placing the properties in boot `system/etc/ramdisk/build.prop` does not override the final production property set on this build. Evidence is in `/tmp/fullerene-bramble-debuggable/{fastboot-boot.log,udev-follow.log,adb-after.txt,ro.debuggable.txt,ro.secure.txt,adb-root.txt,adb-root-id.txt}`. Only transient `fastboot boot` and read-only ADB/property checks were used; no partition/user-data backup, analyzer, flash, erase, or secure-debug operation.

### 2026-09-06 — existing generic-ramdisk rc replacement A/B preparation

- Because the boot-ramdisk property source loses to the final production properties, the next path moves the temporary actions into the stock generic-ramdisk entry that already exists: `system/etc/init/snapuserd.rc`. [`tools/build_bramble_snapuserd_boot.py`](../tools/build_bramble_snapuserd_boot.py) replaces that entry in-place and appends the seven `sys.usb.state` configfs rebind hooks; it does not rely on an alternate `init.rc` or a new filename hidden by a mounted partition.
- Static `py_compile`, cpio/LZ4 round-trip, and action-count checks passed. Artifact: `/tmp/bramble-factory-audit/extracted/boot-1234-snapuserd.img`, SHA-256 `803e2150b4c1c012ce8d177c198066b91f1b71b8cb81fb8f7fc7125f3d018eb8`; archive `3,738,180` bytes, ramdisk `3,752,849` bytes.
- This is preparation only. The next physical A/B is transient `fastboot boot`; success requires a host-visible `1234:0001`, and a failure will be recorded with the active USB ID and evidence path.

### 2026-09-06 — explicit USB init graph A/B preparation

- The existing generic-ramdisk rc replacement also returned stock USB, so the next image uses the already-working `androidboot.init_rc` mechanism but imports the USB graph explicitly: `/system/etc/init/hw/init.usb.rc`, `/system/etc/init/hw/init.usb.configfs.rc`, `/vendor/etc/init/hw/init.sm7250.usb.rc`, and `/vendor/etc/init/android.hardware.usb.gadget-service.bramble.rc`.
- [`tools/build_bramble_explicit_usb_init_boot.py`](../tools/build_bramble_explicit_usb_init_boot.py) passed `py_compile`, v3 header/cpio/LZ4 checks, command-line verification, and seven-action count. Artifact: `/tmp/bramble-factory-audit/extracted/boot-1234-explicit-usb-init.img`, SHA-256 `4cd0f1650f2c171660e652aaa562adcce7d96ab92d910f36973470ea58ba00ad`; archive `3,738,708` bytes, ramdisk `3,753,379` bytes.
- This is a transient boot-only preparation; no partition or persistent boot setting is changed.

### 2026-09-06 — explicit USB init graph physical A/B

- `fastboot boot` accepted `/tmp/bramble-factory-audit/extracted/boot-1234-explicit-usb-init.img` (SHA-256 `4cd0f1650f2c171660e652aaa562adcce7d96ab92d910f36973470ea58ba00ad`). The handset left Fastboot `18d1:4ee0`, but the 75-second host observation saw neither `1234:0001` nor the normal Android `18d1:4ee7` fallback; USB disappeared and ADB did not return.
- This proves the alternate `init.rc` selection is active enough to suppress the normal fallback, but importing only the USB rc graph does not provide a bootable Android/gadget state. Evidence: `/tmp/fullerene-bramble-explicit-usb-init/{fastboot-boot.log,udev-follow.log,adb-after.txt,lsusb-after.txt,result.txt}`. The phone now requires manual return to Fastboot before another physical A/B.
- No partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — official force-debuggable ramdisk path preparation

- The Android 14 first-stage init source shows the missing mechanism: when `/force_debuggable` exists, it copies `/adb_debug.prop` into the second-stage debug-property path and sets `INIT_FORCE_DEBUGGABLE`; the boot ramdisk `build.prop` alone is loaded before later vendor properties and lost in the physical A/B.
- [`tools/build_bramble_force_debuggable_boot.py`](../tools/build_bramble_force_debuggable_boot.py) now adds an empty `/force_debuggable`, `/adb_debug.prop` containing `ro.debuggable=1`, `ro.secure=0`, `ro.adb.secure=0`, and the same values to the boot ramdisk build properties. Static `py_compile`, cpio/LZ4, and exact-entry checks passed.
- Artifact: `/tmp/bramble-factory-audit/extracted/boot-1234-force-debuggable.img`, SHA-256 `9c3ad5079101ddcf0ba2efa4eebb4708bf129b6fa77555758bfb040be404749c`; archive `3,736,956` bytes, ramdisk `3,751,620` bytes. This remains a transient `fastboot boot` artifact and does not write any partition.

### 2026-09-06 — official force-debuggable ramdisk physical A/B

- After the operator manually returned the connected handset to Fastboot, `fastboot boot` accepted `/tmp/bramble-factory-audit/extracted/boot-1234-force-debuggable.img` (SHA-256 `9c3ad5079101ddcf0ba2efa4eebb4708bf129b6fa77555758bfb040be404749c`). The handset left Fastboot `18d1:4ee0` and returned as stock Android `18d1:4ee7`; no `1234:0001` appeared.
- The first-stage path did take effect: read-only ADB reported `ro.debuggable=1`, `ro.secure=0`, and `ro.adb.secure=0`; `ro.boot.verifiedbootstate=orange`. `adb root` attempted a root restart, but production SELinux denied `adbd`'s transition to `u:r:su:s0`, and init restarted normal adbd. Shell/configfs reads were also denied; logcat recorded `setcurrent` and configfs `search` AVCs.
- Conclusion: `/force_debuggable` plus `/adb_debug.prop` fixes the property-source problem but is not enough on a user/release image because the matching userdebug platform policy is absent. The next transient A/B must supply a same-platform debug policy (or a narrowly derived equivalent) before retrying runtime configfs VID/PID control.
- Evidence: `/tmp/fullerene-bramble-force-debuggable/{fastboot-boot.log,udev-follow.log,adb-after.txt,lsusb-after.txt,result.txt}` plus the read-only ADB property/logcat capture. No partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — same-build minimal userdebug policy boot preparation

- The exact Factory `system.img` `/etc/selinux/plat_sepolicy.cil` was read-only extracted and used as the base for `/userdebug_plat_sepolicy.cil`. The transient policy appends only the AOSP userdebug rules needed by this experiment: `allow adbd self:process setcurrent`, `allow adbd su:process dyntransition`, and the generated CIL form `(typepermissive su)` of the source-level `permissive su` rule.
- [`tools/build_bramble_force_debuggable_policy_boot.py`](../tools/build_bramble_force_debuggable_policy_boot.py) adds that policy alongside `/force_debuggable` and `/adb_debug.prop`. `py_compile`, v3 image construction, cpio/LZ4 round-trip, required type checks, and exact-rule checks passed. After correcting the generated CIL spelling to `(typepermissive su)`, the AOSP Android 14 `secilc`/libsepol build also compiled the exact platform+vendor policy graph with `-c 30`. The corrected policy entry is `2,130,983` bytes; the image is `/tmp/bramble-factory-audit/extracted/boot-1234-force-debuggable-policy.img`, SHA-256 `d54cf9f52c230c6d5658b4d1ea4a5d41ca7871383159206dabea06ffdae6e265`.
- The corrected image is ready for a second transient `fastboot boot` A/B. No partition/user-data backup, analyzer, flash, erase, or secure-debug operation was used.

### 2026-09-06 — same-build minimal userdebug policy corrected physical A/B: `1234:0001` success

- `fastboot boot` accepted the corrected transient image `/tmp/bramble-factory-audit/extracted/boot-1234-force-debuggable-policy.img`, SHA-256 `d54cf9f52c230c6d5658b4d1ea4a5d41ca7871383159206dabea06ffdae6e265`, and ADB returned on the stock Bramble runtime.
- Runtime proof: `ro.debuggable=1`, `ro.secure=0`, `ro.adb.secure=0`, `ro.boot.verifiedbootstate=orange`; `adb shell id` reported `uid=0(root)` with `context=u:r:su:s0` while `getenforce` remained `Enforcing`; `adb root` reported `adbd is already running as root`.
- Before the rebind, configfs showed the factory gadget `idVendor=0x18d1`, `idProduct=0x4ee7`, UDC `a600000.dwc3`. The root shell transiently unbound UDC, wrote `idVendor=0x1234` and `idProduct=0x0001`, then rebound the same UDC. No persistent partition or image was changed.
- Host proof: udev removed `PRODUCT=18d1/4ee7/440` and added `PRODUCT=1234/1/440` with `MODALIAS=usb:v1234p0001...`; final `lsusb` reported `ID 1234:0001`. ADB then showed `no permissions` because the host udev rule set does not cover the temporary VID, but USB enumeration was successful and the requested `1234:0001` condition was met.
- Evidence: `/tmp/fullerene-bramble-force-debuggable-policy-corrected/{fastboot-boot.log,udev-follow.log,runtime-before-root.txt,adb-root.txt,runtime-after-root.txt,configfs-rebind.txt,lsusb-after.txt,result.txt}`. No analyzer, flash, erase, secure-debug, or user-data backup operation was used.

### 2026-09-06 — AArch64 FullereneOS port boundary audit after USB success

- `cargo check -p fullerene-kernel --bin fullerene-kernel-aarch64 --features aarch64 --target aarch64-unknown-none` passes. This target is the standalone AArch64 early-boot binary, not the generic `src/main.rs` FullereneOS runtime; the build emits 220 unused-code warnings from the large Bramble platform contract, which is consistent with the current probe-only path.
- Direct source inspection of `fullerene-kernel/src/arch/aarch64/main.rs` confirms the terminal path is `init_interrupt_controller` → `timer::arm_ms` → `exceptions::enable_irqs` → `usb::poll()`/`wfe` forever. It does not call `init_common`, `contexts::kernel::init_kernel`, `loader`, `scheduler_loop`, or the VFS/GUI/process stack.
- The generic kernel remains x86_64-coupled at the dependency and source layers: AArch64 Cargo dependencies currently contain only `fullerene-abi`; `init.rs`, `memory_management`, `process`, and `scheduler` use x86_64 address/page-table/control-register or TSC APIs. A direct `cargo check -p petroleum --target aarch64-unknown-none` reproduces the lower-layer blocker (59 errors including x86 inline assembly, `sysv64`, port I/O, and x86 page-table instructions).
- Therefore the successful `1234:0001` enumeration is a valid reversible hardware/privilege substrate, but it is not evidence that FullereneOS itself is fully ported. The next implementation boundary is an AArch64 architecture layer for the shared runtime (memory mapping, exception/syscall frame, timer/scheduler clock, and platform device discovery), followed by storage/display/input and userspace loader integration. No analyzer, flash, erase, secure-debug, or user-data backup operation was used.

### 2026-09-06 — AArch64 DT memory-map handoff and early frame allocator

- Added `fdt::find_memory_regions`, a bounded allocation-free parser for enabled `memory@...`/`device_type = "memory"` nodes. It inherits the DT root address/size cell widths and publishes up to eight `repr(C)` `{base,size}` entries.
- `BootInfo` now carries the DT-derived map (`flags=MEMORY_MAP`, `memory_map_address`, `memory_map_size`, and descriptor size). The early AArch64 allocator reserves the linked image, USB DMA/trace sections, and DTB before exposing page-aligned physical frames; it does not yet install dynamic page-table mappings or hand frames to userspace.
- `cargo check -p fullerene-kernel --bin fullerene-kernel-aarch64 --features aarch64 --target aarch64-unknown-none` passes. Bramble and QEMU-virt release image builds pass. A bounded QEMU run printed `bootinfo: flags=0x9`, `memory_map_size=0x10`, `memory: first free frame=0x40000000`, and `memory: DTB map frame allocator ready`; timeout was intentional because the existing early path still polls forever.
- No physical Bramble result is claimed for this change: `fastboot devices` was empty. The next physical run must use `fastboot boot` only and inspect the UART memory-map lines before any runtime-port work is accepted.

### 2026-09-06 — AArch64 entry assembly isolated behind typed boot context

- `fullerene-kernel/src/arch/aarch64/main.rs` no longer contains `asm!` or `global_asm!`. The `_start` stack setup, BSS clear, EL2→EL1 transition, boot-register capture, and static-PIE relocation loop moved to `entry.rs`; `Aarch64BootContext` remains `repr(C)` and keeps the existing seven-word handoff layout.
- `wfe` and QEMU semihost exit moved to `cpu.rs`. The Rust entry now consumes `entry::Aarch64BootContext`, while low-level assembly has an explicit module boundary instead of being mixed with DTB/resource discovery and USB bring-up.
- `cargo check -p fullerene-kernel --bin fullerene-kernel-aarch64 --features aarch64 --target aarch64-unknown-none` passed. Bramble and QEMU-virt release image builds passed. A bounded QEMU-virt run still reached `bootinfo: flags=0x9`, `memory: first free frame=0x40000000`, timer initialization, and `aarch64 early boot complete`; the timeout remains intentional because the generic runtime handoff is not implemented.
- This is a source-structure change only; no physical Bramble boot was performed for this change and no claim about `1234:0001` is added. The next port boundary remains AArch64 exception/syscall frames and dynamic page-table/runtime handoff.

### 2026-09-06 — AArch64 typed trap frame and EL0 SVC smoke

- Added `Aarch64TrapFrame` as the explicit `repr(C), align(16)` ABI for x0..x30, `ELR_EL1`, `SPSR_EL1`, `SP_EL0`, `ESR_EL1`, and `FAR_EL1`. The exception vector assembly now only saves/restores this frame; Rust receives a mutable pointer for the future syscall/page-fault path. `main.rs` remains free of `asm!`/`global_asm!`; the remaining assembly is isolated to `entry.rs` and `exceptions.rs`.
- Added a bounded first user mapping (`0x40000000` one 4 KiB page) and an opt-in `aarch64-user-smoke` program. It writes two `svc #0` instructions through the identity map, enters EL0 with `eret`, handles both SVCs through the real sync vector, then returns to an EL1h continuation.
- The first QEMU attempt exposed an actual contract bug: QEMU reported `ELR_EL1=0x40000004` on the first SVC, so adding four bytes skipped the second SVC. The handler was corrected to resume at the supplied ELR; this is recorded as implementation evidence, not inferred from the older docs.
- Final command: `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`. The run reached `bootinfo: flags=0x9`, `memory: first free frame=0x40000000`, `user-smoke: svc=0x1`, `user-smoke: svc=0x2`, and `user-smoke: returned to EL1h`. The command then timed out intentionally in the EL1 `wfe` continuation; no exception fault was printed. Evidence: `/tmp/fullerene-user-smoke-flasks.1KQ1XA.log`.
- `flasks` now honors `FULLERENE_AARCH64_USER_SMOKE=1` only for this opt-in QEMU/build test and hashes the feature selection into its isolated Cargo target directory. No physical Bramble result is claimed, and this still does not constitute the generic FullereneOS runtime port.

### 2026-09-06 — AArch64 native syscall ABI smoke

- Added the architecture-local syscall dispatcher at `fullerene-kernel/src/arch/aarch64/syscall.rs`. It consumes `x8` as the `fullerene-abi::SyscallNumber`, returns through `x0`, and is reached only for user-origin AArch64 SVC exceptions. The first shared ABI slice is `ABI_QUERY`, `GETPID`, `YIELD`, `UPTIME`, and the opt-in smoke-only `EXIT` continuation.
- The user smoke payload now executes the real register convention rather than counting SVCs. Final QEMU output recorded `nr=0x0 ret=0x0000000600000000` (ABI 0.6.0), `nr=0x14 ret=1` (temporary bootstrap PID), `nr=0x16 ret=0` (yield), `exit=0`, and `returned to EL1h`.
- Reproducible command: `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`; the nonzero command status is the expected timeout after the EL1 continuation enters `wfe`. Build check passed with `--features aarch64,aarch64-user-smoke`. Evidence: `/tmp/fullerene-aarch64-syscall-smoke.2upvYm.log`.
- This is the first native syscall ABI execution on the AArch64 path, not yet process creation or a generic scheduler. PID allocation, user-copy validation, ELF loading, and the rest of the syscall table still belong to the next runtime layers; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 single-segment ELF loader smoke

- Added `fullerene-kernel/src/arch/aarch64/elf.rs`. It validates ELF64 little-endian, AArch64 `ET_EXEC`/`ET_DYN`, one `PT_LOAD`, entry-point bounds, file/memory sizes, page alignment, and executable flags; then maps one user page, zero-fills it, copies the segment, synchronizes caches, and returns the ELF entry/stack contract.
- The existing ABI payload is now synthesized as a real ELF image in the smoke path and loaded through that parser. The final QEMU run again reached ABI query (`0x0000000600000000`), bootstrap PID 1, yield 0, exit 0, and EL1h return. No direct instruction writes remain in the user-smoke launch path.
- Evidence: `/tmp/fullerene-aarch64-elf-smoke.2y6xqa.log`; command was `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`. Timeout remains intentional after the EL1 wait loop.
- Scope remains deliberately bounded: no multi-segment address spaces, demand paging, process table, user-copy fault recovery, or generic FullereneOS scheduler is claimed yet. No physical Bramble result is claimed.

### 2026-09-06 — AArch64 cooperative user-frame switching smoke

- The opt-in ELF smoke now creates two separately mapped user pages and two saved `Aarch64TrapFrame` contexts. `YIELD` saves the current frame, writes the ABI return value, and copies the next frame into the live exception frame before `eret`; `EXIT` retires one task and switches to the remaining task.
- Final UART evidence shows `task0 page=0x40000000`, `task1 page=0x40001000`, task 0 yield/switch to task 1, task 1 yield/switch back, task 0 exit status 0, task 1 exit status 0, and `returned to EL1h`. The prior save-order bug that returned task 0's GETPID value as its exit status was corrected before this final run.
- Evidence: `/tmp/fullerene-aarch64-scheduler-smoke.P0XXD7.log`; command `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`. Timeout remains intentional after the EL1 wait loop.
- This validates a real cooperative context-switch boundary, not the generic process manager. Address-space isolation, preemption, lifecycle objects, and the full scheduler remain unported; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 task scheduler extraction rerun

- Extracted the saved-frame/task-slot logic from `user_smoke.rs` into `fullerene-kernel/src/arch/aarch64/task.rs`. The scheduler remains deliberately bounded and cooperative, with no allocation, process table, or address-space policy.
- The first post-extraction `cargo check` caught one non-runtime logging type mismatch (`usize` task index passed to the UART's `u64` formatter) in `/tmp/fullerene-aarch64-task-check.Q3MYpL.log`; it was corrected, and the follow-up check passed with only the existing unused platform-code warnings (`/tmp/fullerene-aarch64-task-check.3tuMh7.log`).
- The post-extraction QEMU rerun produced the expected task IDs (`yield task=0`, `switch to=1`, `yield task=1`, `switch to=0`), both exit statuses 0, and `returned to EL1h`; this confirms the refactor preserved the prior frame-switch behavior.
- Evidence: `/tmp/fullerene-aarch64-scheduler-task-refactor.9xuRw5.log`; command `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`. The nonzero status is the intentional timeout after the EL1 wait loop. No physical Bramble result is claimed.
- Final post-format verification passed: smoke build `/tmp/fullerene-aarch64-final-check.banip4.log`, targeted rustfmt, and `git diff --check`. The default QEMU path still reached `bootinfo: flags=0x9`, `memory: first free frame=0x40000000`, timer initialization, and `aarch64 early boot complete` before its intentional 3-second wait-loop timeout (`/tmp/fullerene-aarch64-default-final.wrRmPF.log`).

### 2026-09-06 — AArch64 scheduler-owned PID/lifecycle boundary

- Extended `Aarch64TaskScheduler` with bounded task identity and lifecycle state, moved its singleton ownership out of `user_smoke.rs`, and made the native `GETPID` syscall read the current scheduler slot instead of returning a hard-coded 1.
- The QEMU smoke now installs PID 1 and PID 2. UART recorded `GETPID=1`, yield `PID 1→2`, `GETPID=2`, yield `PID 2→1`, then task exit `PID 1→2` and final exit status 0. This is the first runtime evidence that the syscall return value follows the saved user-frame switch.
- Build evidence: `/tmp/fullerene-aarch64-process-table-check.24FKJ6.log`; runtime evidence: `/tmp/fullerene-aarch64-process-table-smoke.V53XHL.log`; command `FULLERENE_AARCH64_USER_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5`. Timeout remains intentional after EL1 `wfe`; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 user stack split from executable pages

- Replaced the smoke path's code-page-end `SP_EL0` shortcut with two separately allocated and mapped stack pages at `0x40002000` and `0x40003000`. `elf.rs` now exposes an explicit zeroed user-page mapping helper, and `LoadedPage` returns only the executable entry point.
- QEMU reached both stack mappings, retained PID 1/2 switching, and returned to EL1h with exit status 0. This removes one invalid process-layout assumption; it does not yet provide per-process page tables or stack-growth faults.
- Build evidence: `/tmp/fullerene-aarch64-separate-stack-check.lAfm60.log`; runtime evidence: `/tmp/fullerene-aarch64-separate-stack-smoke.y7Rz4M.log`; timeout remains intentional after the EL1 wait loop. No physical Bramble result is claimed.

### 2026-09-06 — AArch64 multi-segment ELF load boundary

- Replaced the one-`PT_LOAD` loader with a bounded fixed-array loader accepting up to four load segments and 32 pages. It validates ELF64/AArch64 headers, page-offset congruence, file/memory bounds, entry ownership, zero-fills BSS, copies segment bytes through the identity map, and synchronizes executable pages.
- The first two-segment QEMU attempt exposed a real test-image error: `e_entry` pointed at the page start while code was emitted at file/page offset `0x200`; QEMU faulted at `ELR=0x40000000` before the first syscall (`/tmp/fullerene-aarch64-multisegment-smoke.z2tHk8.log`). The image generator was corrected to set `e_entry` to the code offset.
- Corrected QEMU output reports two image pages per task, separate stacks, PID 1/2 switching, both exits, and EL1h return. Build evidence: `/tmp/fullerene-aarch64-multisegment-rerun.t7CgQt.log`; runtime evidence: `/tmp/fullerene-aarch64-multisegment-rerun.xckGnD.log`. Timeout remains intentional; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 native YIELD/EXIT moved out of smoke-only code

- Moved the `YIELD` and `EXIT` syscall actions into `task.rs`. The AArch64 dispatcher now invokes scheduler-owned operations without a `aarch64-user-smoke` conditional; the smoke launcher only creates the test images and installs tasks.
- Both the feature-enabled and default AArch64 kernel checks passed. The feature-enabled QEMU run retained the two-segment image, separate stacks, dynamic PID 1/2 returns, cooperative switches, and EL1h exit continuation.
- Evidence: checks `/tmp/fullerene-aarch64-syscall-generic-check.bwttRg.log` and `/tmp/fullerene-aarch64-syscall-default-check.DXvd43.log`; runtime `/tmp/fullerene-aarch64-syscall-generic-smoke.pzXYzF.log`. The timeout remains intentional; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 checked user-copy and GET_PROCESS_NAME

- Added an MMU-validated `copy_to_user` path that accepts only EL0-writable descriptors, plus scheduler-owned task names. `GET_PROCESS_NAME` now copies the current task name into its user buffer and returns the byte count.
- The first QEMU run exposed descriptor attribute leakage: the `UXN` bit was included in the physical address returned by the page walk, producing `FAR=0x0040000040001003` during the kernel copy (`/tmp/fullerene-aarch64-user-copy-smoke.rTEKd7.log`). Masking the AArch64 output address bits fixed it.
- Corrected QEMU recorded syscall 21 (`GET_PROCESS_NAME`) returning 5 for both PID 1 and PID 2, followed by the existing switch/exit/EL1h sequence. Build evidence: `/tmp/fullerene-aarch64-user-copy-rerun.bkyke0.log`; runtime evidence: `/tmp/fullerene-aarch64-user-copy-rerun.Y2toCX.log`. Timeout remains intentional; no physical Bramble result is claimed.
- The non-smoke AArch64 check also passed (`/tmp/fullerene-aarch64-user-copy-default.XNxBRr.log`), alongside `cargo check -p flasks`, targeted rustfmt, and `git diff --check`.

### 2026-09-06 — physical-state check after AArch64 software work

- Read-only `fastboot devices` returned no device. No `fastboot boot`, flash, erase, partition access, analyzer, secure-debug, or user-data backup operation was attempted in this step. The prior transient physical `1234:0001` evidence remains unchanged.

### 2026-09-06 — AArch64 per-task user L3 address-space switching

- Replaced the single shared user L3 with eight bounded address-space slots. Each task now owns a slot, and `YIELD`/`EXIT` rebind the split user window to the next task's L3 before restoring its register frame.
- The smoke deliberately loads both tasks at the same user VA (`0x40000000`) and uses separate physical image/stack pages. QEMU recorded address-space 0/1, identical user VAs, successful `GETPID`/`GET_PROCESS_NAME` in both contexts, both switches, both exits, and EL1h return.
- Build evidence: `/tmp/fullerene-aarch64-address-space-rerun.Wxsqp1.log`; runtime evidence: `/tmp/fullerene-aarch64-address-space-rerun.99WA37.log`. This is serialized L3 rebinding, not yet independent TTBR0 roots/ASIDs, demand paging, or a generic process manager; no physical Bramble result is claimed.

### 2026-09-06 — AArch64 user page permission enforcement

- Marked ELF `PF_R|PF_X` pages as EL0 read-only and `PF_R|PF_W` pages as EL0 read/write. The loader now copies and cache-synchronizes segment data before publishing the final user permissions, avoiding an EL1 permission fault during image construction.
- The first permission run exposed that ordering bug: QEMU faulted on the first user instruction with `ESR=0x9600004f`, `FAR=0x40000000` while the code page was already user-read-only (`/tmp/fullerene-aarch64-page-permissions-smoke.oQswSV.log`). The loader was corrected to finish the identity-mapped zero/copy phase before installing the read-only descriptor.
- Corrected QEMU evidence: for both tasks, `GET_PROCESS_NAME` to the code page returned `0xfffffffffffffff2` (`-EFAULT`), the same syscall to the writable data page returned `5`, and the existing same-VA address-space switch, exits, and EL1h return completed. Evidence: `/tmp/fullerene-aarch64-page-permissions-rerun.log`; timeout is intentional after the EL1 `wfe` loop.
- This verifies the current descriptor permissions in QEMU only. It is not yet demand-paging/page-fault recovery, independent TTBR0 roots/ASIDs, or a physical Bramble result.

### 2026-09-06 — AArch64 independent TTBR0 user roots

- Replaced the shared-L1/L2 user-window rebinding with bounded `Aarch64UserAddressSpace` objects. Each object owns a root, a user L2, and a user L3; the root's kernel entries still point to the shared identity-map L2 tables. `activate_user_space` now writes `TTBR0_EL1` to the selected root and flushes translations.
- QEMU read back `TTBR0_EL1` as task 0 `0x40290000`, task 1 `0x40294000`, then task 0 `0x40290000` and task 1 `0x40294000` again across yield/exit. Both tasks kept the same user VA (`0x40000000`), returned `-EFAULT` for code-page writes and `5` for data-page writes, and returned to EL1h.
- Build evidence: `/tmp/fullerene-aarch64-ttbr0-root-check.log`; runtime evidence: `/tmp/fullerene-aarch64-independent-ttbr0-smoke.log`. ASIDs remain deliberately disabled; this uses a full EL1 TLB flush on root switch. Demand paging, fault recovery, allocator-owned page tables, and the generic process manager remain open; no physical Bramble result is claimed.

### 2026-09-06 — generic Fullerene kernel AArch64 entry integration

- The generic `fullerene-kernel` AArch64 target previously stopped in `build.rs` before generating `solvent-linux.rs` and `FULLERENE_LAUNCHD_IMAGE`; the baseline failure is `/tmp/fullerene-generic-aarch64-baseline.log`. `build.rs` now always generates the architecture-neutral Solvent integration and supplies an explicit empty launchd fixture for this still-early AArch64 path.
- Refactored the AArch64 implementation behind `src/arch/aarch64/bin.rs` plus the reusable runtime module. The dedicated `fullerene-kernel-aarch64` target still compiles, and the package's generic `fullerene-kernel` target now also compiles for `aarch64-unknown-none` instead of pulling the x86-only common modules.
- Added opt-in Flasks selection via `FULLERENE_AARCH64_GENERIC_KERNEL=1`. Its QEMU run reached the same DTB/MMU/EL0 syscall smoke through the generic artifact, including distinct TTBR0 roots, permission failures/successes, task switches, and EL1h return. Evidence: `/tmp/fullerene-generic-aarch64-wrapper-check2.log`, `/tmp/fullerene-generic-aarch64-qemu-smoke.log`.
- This is an entry/build integration milestone, not the complete FullereneOS runtime: the generic AArch64 path currently gates the x86-coupled process/VFS/GUI/service modules while their AArch64 backends are being ported. No physical Bramble result is claimed.

### 2026-09-06 — AArch64 user page-fault retirement boundary

- Added a bounded lower-EL instruction/data-abort path to the typed AArch64 exception handler. A user fault is logged with the current task, `ESR_EL1`, and `FAR_EL1`, then the task is marked exited and the scheduler switches to the next saved user frame; if no task remains, the handler returns to the EL1h continuation with `-EFAULT` in `x0`.
- The fault smoke deliberately executes an EL0 load from unmapped `0x05000000`. Both the dedicated and opt-in generic kernel artifacts produced `ESR=0x9200000e` (lower-EL data abort) and `FAR=0x05000000`; task 0 switched to task 1 with `TTBR0=0x40294000`, task 1 faulted, and execution returned to EL1h.
- Reproducible commands: `FULLERENE_AARCH64_USER_FAULT_SMOKE=1 cargo run -q -p flasks -- run --arch aarch64 --platform qemu-virt --timeout 5` and the same command with `FULLERENE_AARCH64_GENERIC_KERNEL=1`. Evidence: `/tmp/fullerene-aarch64-user-fault-smoke.log` and `/tmp/fullerene-generic-aarch64-user-fault-smoke.log`. Both commands end with the intentional 5-second QEMU wait-loop timeout after the expected EL1h marker.
- This is fault containment, not fault resolution: demand paging, copy-on-write, signal delivery, a general process manager, and physical Bramble validation remain open. No physical device was touched.

### 2026-09-06 — AArch64 entry assembly absolute reduction and regression fix

- The prior entry refactor was corrected to reduce the amount of assembly itself, not merely move it from `main.rs` to `entry.rs`. The `_start` trampoline now only computes the bootstrap-stack top and branches to Rust; the old `.text.boot` was 0xd4 bytes (53 AArch64 instruction slots), while the new image is 0x14 bytes (5 instructions). The architecture-required EL2→EL1 system-register transition, relocation address discovery, and EL1 SIMD enable remain as small inline primitives; `main.rs` still contains no `asm!`/`global_asm!`.
- The first reduced-entry QEMU run exposed a real EL1-only regression: QEMU enters directly at EL1, and without `CPACR_EL1.FPEN` enabled, an optimized Rust copy trapped before the normal UART banner (`ESR=0x1fe00000`, `ELR=0x40087070`). Enabling FP/SIMD for the direct-EL1 path, while retaining the existing post-`eret` EL2 path, fixed it. The diagnostic-only bootstrap SP capture was also removed from the typed handoff.
- Corrected dedicated QEMU smoke reached DTB discovery, MMU, both same-VA tasks, permission checks (`-14` on code / `5` on data), TTBR0 switches, both exits, and `returned to EL1h`. Final evidence: `/tmp/fullerene-aarch64-entry-absolute-reduction-final-check.log` and `/tmp/fullerene-aarch64-entry-absolute-reduction-final-smoke.log`; the 5-second timeout is intentional after the EL1 wait loop.
- The opt-in generic `fullerene-kernel` artifact now also launches the real embedded AArch64 ELF `launchd` payload and reached ABI query, PID 1, process-name length 7, YIELD, EXIT 0, and `returned to EL1h`. Final evidence: `/tmp/fullerene-generic-aarch64-entry-absolute-reduction-final-launchd.log`. This remains QEMU-only; exception-vector assembly and userland `svc` instructions are separate architectural boundaries, and no physical Bramble operation was performed.

### 2026-09-06 — AArch64 launchd SPAWN/WRITE child-process boundary

- Extended the real embedded AArch64 `launchd` payload with `WRITE` and `SPAWN`. The kernel now copies a bounded child ELF and name through the active user page walk, allocates child image/stack pages from the DTB-backed frame allocator, creates a separate task slot and TTBR0 root, and enters the child through the same typed EL0 frame path. The child writes a message and exits; this is process creation across the syscall boundary, not an in-kernel function call.
- The first post-change QEMU runs hung immediately after the launchd stack log. The cause was that clearing every L3 entry removed the EL1-only identity mapping for the kernel itself: the current linker image is in the same 2 MiB window as user VA `0x40000000`. The corrected MMU keeps that window as EL1-only identity pages and replaces only explicit image/stack entries with EL0 permissions. Dedicated user smoke, launchd, and fault smoke all passed afterward.
- Dedicated QEMU evidence: `/tmp/fullerene-aarch64-spawn-launchd-2.log` shows launchd `SPAWN` returning PID 2, child TTBR0 `0x40284000`, `child: spawned and running`, child `EXIT`, return to PID 1, and `returned to EL1h`. The opt-in generic artifact reproduced the same trace in `/tmp/fullerene-generic-aarch64-spawn-launchd.log`. Regression evidence is `/tmp/fullerene-aarch64-post-spawn-smoke-2.log`; fault evidence is `/tmp/fullerene-aarch64-post-spawn-fault.log`.
- Build checks passed with `cargo check -p flasks` and `cargo check -p fullerene-kernel --target aarch64-unknown-none --features aarch64,aarch64-user-launchd --bin fullerene-kernel-aarch64`; `git diff --check` passed. The graph coverage check reported `coverage_unavailable` for the changed AArch64/build paths, so direct source and runtime results are authoritative for this change.
- This remains a bounded QEMU/software milestone: eight task/address-space slots, bounded image/name/write sizes, no wait/reap, signals, fork/exec semantics, filesystem/VFS, device services, GUI, or physical Bramble validation yet. No physical device was touched in this step.

### 2026-09-06 — AArch64 parent WAIT and child wake-up boundary

- Added the ABI's `WAIT` syscall to the AArch64 scheduler. A parent can wait for one child (or any child with the ABI wildcard), becomes `Blocked` instead of spinning, and is woken by child `EXIT` with the encoded status placed in its saved `x0`. The launchd probe now uses this path rather than an explicit yield.
- QEMU evidence: `/tmp/fullerene-aarch64-wait-launchd.log` shows `wait task=0`, switch to child task 1/ PID 2, child `WRITE`, child exit, switch back to PID 1, parent exit 0, and `returned to EL1h`. The opt-in generic artifact reproduces the same sequence in `/tmp/fullerene-generic-aarch64-wait-launchd.log`.
- Existing two-task permissions, independent TTBR0, yield, exit, and fault-sensitive paths remained green in `/tmp/fullerene-aarch64-wait-regression-smoke.log`. The AArch64 build check passed in `/tmp/fullerene-aarch64-wait-check.log`; the timeout in each QEMU run is intentional after the final EL1 wait loop.
- This is still bounded lifecycle support, not the generic process manager: exited slots and physical pages are not reclaimed for reuse, parent reparenting/signals/process-control handles are absent, and VFS/filesystem/device/GUI services plus physical Bramble validation remain open. No physical device was touched.

### 2026-09-06 — AArch64 process-control handle/status/reap boundary

- Added the process-control ABI subset used by the native supervisor: `OPEN_PROCESS_CONTROL` returns an opaque tagged handle for a child, `PROCESS_CONTROL_STATUS` reports `READY/RUNNING/BLOCKED/TERMINATED`, and `PROCESS_CONTROL_REAP` returns the encoded exit status. Parent authorization is checked against the bounded scheduler's parent PID.
- The native AArch64 launchd probe now opens a control handle for PID 2, yields, observes `STATUS=3`, reaps `0`, and exits. Dedicated QEMU evidence is `/tmp/fullerene-aarch64-process-control-launchd.log`; the generic artifact reproduces it in `/tmp/fullerene-generic-aarch64-process-control-launchd.log`.
- The dedicated permission/scheduler smoke and lower-EL fault smoke remained green in `/tmp/fullerene-aarch64-process-control-regression-smoke.log` and `/tmp/fullerene-aarch64-process-control-regression-fault.log`. Build evidence is `/tmp/fullerene-aarch64-process-control-check.log`; the intentional QEMU timeout occurs only after the EL1 wait loop.
- This is still a bounded process-control implementation: handles are not a general capability table, stop/assign/reparent/signals are absent, exited slots and physical pages are not reclaimed, and VFS/filesystem/terminal/GUI/device services plus physical Bramble validation remain open. No physical device was touched.

### 2026-09-06 — AArch64 process-control handle ownership correction

- The first process-control handle encoding was only a tagged target PID (`0x800...2`), which was too guessable for an authorization boundary. It was corrected to encode both owner and target (`0x8000000100000002` for PID 1 controlling PID 2); `STATUS` and `REAP` now reject handles whose owner does not match the current process.
- Corrected dedicated and generic QEMU runs retained `STATUS=3`, `REAP=0`, child output, parent exit, and EL1h return. Evidence: `/tmp/fullerene-aarch64-process-control-owner-launchd.log` and `/tmp/fullerene-generic-aarch64-process-control-owner-launchd.log`. The earlier unowned-handle result remains recorded as the failed trial above.
- The owner-checked AArch64 build passed in `/tmp/fullerene-aarch64-process-control-owner-check.log`. This remains a bounded scheduler capability approximation, not the full kernel handle table; no physical device was touched.

### 2026-09-06 — AArch64 bounded dynamic memory syscall boundary

- Added AArch64 `MAP_MEMORY`, `UNMAP_MEMORY`, `PROTECT_MEMORY`, and `QUERY_MEMORY`. Each task now has bounded dynamic mapping records; mappings allocate and zero DTB-backed physical frames, install EL0 permissions in the task's TTBR0 root, and support protection changes or unmapping back to an EL1-only identity page. The current window is intentionally bounded to 64 pages per mapping and eight mapping records per task.
- The launchd probe exercises the complete path: map with read/write protection, copy a string into the returned user address, `WRITE` it, change it read-only, query a 64-byte `MemoryInfo`, and unmap it. QEMU returned address `0x40020000`, `WRITE=25`, `PROTECT=0`, `QUERY=64`, and `UNMAP=0`; evidence: `/tmp/fullerene-aarch64-dynamic-memory-launchd.log`.
- The opt-in generic artifact reproduced the same dynamic mapping sequence in `/tmp/fullerene-generic-aarch64-dynamic-memory-launchd.log`. Existing permission/scheduler and lower-EL fault paths remained green in `/tmp/fullerene-aarch64-dynamic-memory-regression-smoke.log` and `/tmp/fullerene-aarch64-dynamic-memory-regression-fault.log`.
- Build evidence is `/tmp/fullerene-aarch64-memory-launchd-check.log`; the existing unused platform-code warnings remain. This is a bounded user-memory backend, not yet demand paging, reclamation, fork/clone, shared buffers, or the generic architecture-neutral memory manager. No physical device was touched.

### 2026-09-06 — AArch64 copied-address-space FORK boundary

- Added the shared ABI's `FORK=2` to the native AArch64 syscall dispatcher. The scheduler copies the live parent trap frame, returns child PID 0 in the child and the new PID in the parent, records the parent PID, and starts the child at the same post-`SVC` EL0 instruction.
- `mmu::clone_user_space` now creates an independent bounded user root, copies every explicit EL0 image/stack/dynamic page into newly allocated physical frames, and preserves each page's read-only/read-write and executable state. This is a physical-copy fork; copy-on-write, page reclamation, ASIDs, and rollback of already allocated frames are still not implemented.
- The native launchd probe calls `FORK`, has the child print `launchd: fork child` and exit, and has the parent block in `WAIT` before continuing to the existing dynamic/`SPAWN`/process-control sequence. Dedicated QEMU evidence is `/tmp/fullerene-aarch64-fork-launchd.log`: `FORK` returned PID 2, the child printed and exited with status 1, the parent resumed on PID 1, then spawned PID 3 and completed process-control `STATUS=3`/`REAP=0`.
- The generic artifact reproduced the complete sequence in `/tmp/fullerene-generic-aarch64-fork-launchd.log`. Existing permission/independent-TTBR0 scheduler smoke and lower-EL abort retirement remained green in `/tmp/fullerene-aarch64-fork-regression-smoke.log` and `/tmp/fullerene-aarch64-fork-regression-fault.log`; host build evidence is `/tmp/fullerene-host-fork-check.log` and AArch64 build evidence is `/tmp/fullerene-aarch64-fork-check.log`.
- Targeted rustfmt and `git diff --check` passed. The QEMU commands end in the intentional 5-second EL1 `wfe` timeout after `returned to EL1h`. This remains a bounded QEMU/software milestone: no generic process manager, fork/exec, COW/reclamation, shared buffers, VFS/filesystem/device/GUI services, or physical Bramble validation is claimed, and no physical device was touched.

### 2026-09-06 — AArch64 fork cloned-page probe: first attempt failed

- A follow-up launchd probe kept the dynamic mapping alive across `FORK` so the child could read the copied page. The first attempt failed before `FORK`: after `PROTECT_MEMORY` changed `0x40020000` read-only, `QUERY_MEMORY` wrote into the launchd static `MEMORY_INFO` buffer at the same VA, causing an EL1 permission fault (`ESR=0x9600004f`, `FAR=0x40020000`).
- This was a probe-layout error, not evidence against the fork/MMU implementation. The static query buffer was moved onto the writable user stack; the failed evidence is `/tmp/fullerene-aarch64-fork-cloned-page-launchd.log`.

### 2026-09-06 — AArch64 fork cloned-page probe: second attempt exposed MMU copy collision

- After moving the query buffer to stack, `QUERY_MEMORY` succeeded (`64`), but the next `FORK` still faulted in EL1 while copying the dynamic page. The parent user root was still active, so the physical identity address used by the copy could resolve through an EL0 permission descriptor; evidence is `/tmp/fullerene-aarch64-fork-cloned-page-fixed-launchd.log`.
- This second failure led to the identity-root copy fix recorded next; it was distinct from the first probe-buffer failure.

### 2026-09-06 — AArch64 fork physical-copy isolation corrected

- The next run showed the deeper issue in the cloned-page probe: `clone_user_space` was copying physical frames while the parent user root was still active. When a physical frame fell in the same 2 MiB identity window as an EL0 descriptor, the EL1 copy could hit that user permission, so the fork path was changed to switch temporarily to the shared EL1 identity root, copy/synchronize all pages, and restore the parent root even on the bounded failure path.
- With the stack query buffer and identity-copy fix, dedicated QEMU evidence `/tmp/fullerene-aarch64-fork-cloned-page-fixed2-launchd.log` recorded query `64`, `FORK` PID 2, the child reading the cloned read-only dynamic page and printing `launchd: dynamic mapping`, child `launchd: fork child`, parent wake, `UNMAP=0`, SPAWN PID 3, process-control `STATUS=3`/`REAP=0`, and `returned to EL1h` before the intentional timeout.
- The generic artifact reproduced the same cloned-page output in `/tmp/fullerene-generic-aarch64-fork-cloned-page-fixed-launchd.log`. Build evidence is `/tmp/fullerene-aarch64-fork-identity-copy-check.log`; targeted rustfmt passed. This verifies physical-copy fork for the current image, stack, and active dynamic mapping, but not COW, reclamation, ASIDs, or general fork/exec semantics. No physical device was touched.

### 2026-09-06 — AArch64 entry absolute count reduction and native VFS first read

- This follow-up reduced entry.rs itself: .boot_stack is adjacent to .text.boot, so _start now loads the linker-exported stack top with one ADR, copies it to SP, and branches to Rust. The linked stub is 0x0c/3 instructions instead of the prior 0x14/5; the EL2 handoff reuses its synchronization barrier for CPACR_EL1 instead of carrying a second post-ERET sequence. main.rs remains free of AArch64 asm!/global_asm!.
- Static-PIE relocation code was kept because the Bramble-linked artifact still contains 55 R_AARCH64_RELATIVE records. The dedicated static build log is /tmp/fullerene-bramble-entry-reduction-static-build.log; dedicated QEMU and generic QEMU both booted through the reduced stub.
- The first VFS trial exposed a real handle-decoding bug: OPEN succeeded but READ/CLOSE returned -9 because the tag bit was included in the generation comparison. The failed log is /tmp/fullerene-aarch64-entry-reduction-vfs-smoke.log; the correction masks the tag before generation extraction.
- The corrected native launchd path initialized Genome's in-memory VFS, opened /etc/motd, read 24 bytes, wrote FullereneOS AArch64 VFS, and closed with 0. Dedicated evidence is /tmp/fullerene-aarch64-entry-reduction-vfs-smoke-fixed-handle.log; the generic artifact reproduced it in /tmp/fullerene-generic-aarch64-entry-reduction-vfs.log. Smoke/fault regressions retained the expected returned to EL1h, ESR=0x9200000e, and FAR=0x05000000. (The earlier `0x50000000` text was a documentation typo; the instruction encoding and UART logs identify `0x05000000`.)
- This advances only the bounded native AArch64 path: the VFS is memory-backed, descriptor inheritance and persistent mounts are absent, and this remains QEMU/software evidence with no physical device operation.

### 2026-09-06 — AArch64 file-backed VFS / executable-launch trials

- Native launchd now seeds `/bin/child` in Genome's in-memory VFS and reads the 70,768-byte child ELF through repeated `OPEN`/`READ`/`CLOSE` syscalls into a user mapping. The first QEMU trial hit physical-frame exhaustion after `CLOSE` because `UNMAP_MEMORY` did not reclaim pages (`/tmp/fullerene-aarch64-vfs-file-backed-launchd-1.log`).
- The bounded free-list correction made `UNMAP=0` and returned frames, but the next trial exposed the separate 128 KiB kernel-heap limit: the resident VFS file plus the checked `SPAWN` staging vector exhausted the heap (`/tmp/fullerene-aarch64-vfs-file-backed-launchd-reclaim.log`). The AArch64 heap is now 256 KiB for this bounded handoff.
- With that resource fix, `SPAWN=3` was reached, but the child entered an undefined instruction (`ESR=0x02000000`, `ELR=0x40000000`, `FAR=0`). The root cause was writing allocated physical pages through the parent's TTBR0 while its file buffer mapping was active; the diagnostic logs are `/tmp/fullerene-aarch64-vfs-file-backed-launchd-reclaim-2.log` and `/tmp/fullerene-aarch64-vfs-file-backed-debug-map.log`.
- The corrected loader switches to the shared EL1 identity root for ELF/stack physical writes and restores the caller root. Dedicated `/tmp/fullerene-aarch64-vfs-file-backed-final-dedicated.log` and generic `/tmp/fullerene-generic-aarch64-vfs-file-backed-final.log` now show motd read, dynamic memory, physical-copy fork, repeated file reads, `SPAWN=3`, child output, buffer unmap, process-control `STATUS=3`/`REAP=0`, and `returned to EL1h`. Dedicated/generic smoke and lower-EL fault regressions remain green. This is still QEMU/software evidence; no physical Bramble operation was attempted.

### 2026-09-06 — AArch64 exited-task reclamation and repeatable spawn

- Added real bounded teardown for inactive exited tasks. `WAIT` and `PROCESS_CONTROL_REAP` now return the child's complete explicit user-page batch to the physical-frame free list, reset its user TTBR0 root, and clear the task slot. ELF allocation, failed spawn, and failed fork-clone paths also roll back pages instead of leaking them.
- Dedicated launchd QEMU evidence `/tmp/fullerene-aarch64-task-reclaim-repeat.log` shows the fork child being waited/reclaimed, followed by two file-backed child launches that both reuse PID 3, execute `child: spawned and running`, report `STATUS=3`, `REAP=0`, and return to EL1h. Generic static-PIE evidence is `/tmp/fullerene-generic-task-reclaim-repeat-pie.log`; the non-PIE manual build failure is retained as `/tmp/fullerene-generic-task-reclaim-repeat.log` and is a build-condition failure, not a lifecycle result.
- Fault regression `/tmp/fullerene-aarch64-task-reclaim-fault.log` retained lower-EL abort retirement (`ESR=0x9200000e`, `FAR=0x05000000`). AArch64 and host checks passed in `/tmp/fullerene-aarch64-task-reclaim-check.log` and `/tmp/fullerene-host-task-reclaim-check.log`. The result remains QEMU/software-only; COW, demand paging, reparenting/signals, general `exec`, persistent filesystems, device/GUI services, and physical Bramble validation remain open.

### 2026-09-06 — AArch64 COW fork and physical-alias correction

- `FORK` now shares resident writable pages with bounded frame reference counts and read-only software-COW descriptors. The lower-EL write fault resolves by making a last-owner page private or copying the page, then returns to the faulting user instruction.
- Dedicated `/tmp/fullerene-aarch64-cow-fixed2.log` proves `cow resolved`, child-side `launchd: cow child`, parent-side unchanged `launchd: dynamic mapping`, and two later `SPAWN=3`/child/`REAP=0` cycles. Generic static-PIE evidence is `/tmp/fullerene-generic-aarch64-cow.log`.
- The first COW trial exposed a physical-alias bug in kernel frame zeroing and checked user copies when a recycled frame overlapped a read-only user stack. Those operations now use the shared EL1 identity root. Fault regression `/tmp/fullerene-aarch64-cow-fault.log`, AArch64 check `/tmp/fullerene-aarch64-cow-check.log`, and host check `/tmp/fullerene-host-cow-check.log` passed; no physical device was touched.
- COW is currently bounded to resident writable mappings. Demand paging, ASIDs, dynamic page tables, general `exec`, reparenting/signals, persistent filesystems, device/GUI services, and physical Bramble validation remain open.

### 2026-09-06 — AArch64 blocking WAIT auto-reap after COW

- A blocked `WAIT` now reaps the child after switching back to its parent, so the wakeup path also releases the child address space and COW references. Dedicated `/tmp/fullerene-aarch64-cow-wait-reap.log` and generic `/tmp/fullerene-generic-aarch64-cow-wait-reap.log` both show `wait task=0`, COW resolution, child exit, two later `SPAWN=3` cycles, and `returned to EL1h`.
- This closes the bounded wait-wakeup lifetime hole. Reparenting, signals, general process manager/`exec`, demand paging, persistent filesystems, device/GUI services, and physical Bramble validation remain open; no physical device was touched.

### 2026-09-06 — AArch64 monotonic PID allocation after reap

- The scheduler now uses a monotonic PID cursor with live-collision checks instead of deriving the next PID from the current maximum. Dedicated `/tmp/fullerene-aarch64-pid-cursor.log` and generic `/tmp/fullerene-generic-aarch64-pid-cursor.log` retain COW resolution and launch PID 3 then PID 4 after reaping; both reach `returned to EL1h` before the intentional timeout.
- AArch64 and host checks passed in `/tmp/fullerene-aarch64-pid-cursor-check.log` and `/tmp/fullerene-host-pid-cursor-check.log`; no physical device was touched. PID wrap-generation handling, reparenting, signals, general `exec`, demand paging, persistent filesystems, device/GUI services, and physical Bramble validation remain open.

### 2026-09-06 — AArch64 orphan adoption and init WAIT/reap

- The file-backed child probe now forks a grandchild and exits first. Dedicated `/tmp/fullerene-aarch64-reparent-smoke.log` and generic `/tmp/fullerene-generic-aarch64-reparent-smoke.log` show PID 3→4 and PID 5→6 sequences, `orphan adopted by init`, init-side `reaped adopted child`, and `returned to EL1h` with no synchronous exception.
- This verifies the bounded parent-exit adoption path and `WAIT(-1)` reap. Signals, PID-wrap generations, general `exec`, demand paging, persistent filesystems, device/GUI services, and physical Bramble validation remain open; no physical device was touched.

### 2026-09-06 — AArch64 same-PID image replacement and staging correction

- Added ABI `Exec=24`: checked image/name copy, staged ELF in an unreferenced TTBR0 root, fresh user stack, old-root release after activation, and unchanged PID/parent. `SPAWN`/`FORK` now select free roots independently of task-slot indices.
- An initial exec trial exhausted the bump heap before `EXEC` returned; fixed 96 KiB syscall image staging and stack-backed VFS paths remove the per-call temporary-allocation leak. Final dedicated and generic QEMU logs `/tmp/fullerene-aarch64-exec-smoke.log` and `/tmp/fullerene-generic-aarch64-exec-smoke.log` show PID 7 `aarch64 exec`, replacement child output, orphan adoption/reap, and `returned to EL1h` with no synchronous fault or allocator exhaustion.
- Checks passed: `/tmp/fullerene-aarch64-exec-check.log`, `/tmp/fullerene-host-exec-check.log`, `/tmp/fullerene-abi-exec-test.log`; QEMU nonzero status is only the intentional timeout after EL1 `wfe`. This is bounded image-buffer exec, not yet pathname/argv/envp, demand paging, signals, full handle inheritance, persistent filesystems, services, or physical Bramble validation.

### 2026-09-06 — AArch64 pathname exec and argv/envp

- Added `EXEC_PATH=25`: `/bin/child` is read from the native VFS into fixed staging, its basename becomes the process name, and bounded `argv`/`envp` data is installed on the replacement image's user stack. Dedicated/generic QEMU logs `/tmp/fullerene-aarch64-exec-argv-smoke.log` and `/tmp/fullerene-generic-aarch64-exec-argv-smoke.log` show PID 9, `argc=1`, `envc=0`, `sp=0x40010fc0`, replacement execution, orphan adoption/reap, and `returned to EL1h` without a synchronous exception or allocator exhaustion.
- This is a bounded ABI only: auxv, interpreter/path search, demand paging, signals, and physical Bramble validation remain open. Checks passed in `/tmp/fullerene-aarch64-fd-lifecycle-check.log`, `/tmp/fullerene-host-fd-lifecycle-check.log`, and `/tmp/fullerene-abi-fd-lifecycle-test.log`.

### 2026-09-06 — AArch64 fork file-descriptor inheritance and cleanup

- Fork now duplicates logical handle values and local VFS descriptions, preserving the shared file offset; the last close across owners closes the underlying descriptor. Exit/rollback removes owner rows. Dedicated/generic runtime logs include `launchd: inherited fd child`, the child prints `FullereneOS AArch64 VFS` after the parent closes its copy, and both reach `returned to EL1h` without a synchronous exception.
- The table is bounded to 32 storage rows and 16 logical handles. `CLOEXEC`, `dup`, pipes, general capabilities, persistent filesystems, and physical Bramble validation remain open.

### 2026-09-06 — AArch64 initramfs-backed native VFS

- Replaced hard-coded VFS seeds with a build-produced `newc` CPIO containing `/etc/motd`, `/bin/child`, and `/bin/launchd`. The kernel validates bounds, alignment, path components, file types, and a 32-entry limit before installing the archive into the native VFS.
- Dedicated/generic QEMU logs `/tmp/fullerene-aarch64-initramfs-smoke.log` and `/tmp/fullerene-generic-aarch64-initramfs-smoke.log` report five unpacked entries, retain pathname exec/orphan reap/FD inheritance, print the VFS message, and reach `returned to EL1h`; only the intentional wait-loop timeout follows.

### 2026-09-06 — AArch64 launchd loaded from initramfs VFS

- The first user process no longer reads a second direct embedded launchd image. It reads `/bin/launchd` through the native VFS into a bounded 96 KiB staging buffer, then uses the ordinary ELF loader and TTBR0/task path.
- Dedicated/generic logs `/tmp/fullerene-aarch64-initramfs-launchd-from-vfs-smoke.log` and `/tmp/fullerene-generic-aarch64-initramfs-launchd-from-vfs-smoke.log` report five entries, three launchd image pages at `0x40000000`, the existing exec/orphan/FD checks, and `returned to EL1h`; only the intentional timeout follows.

### 2026-09-06 — AArch64 Bramble USB handoff before launchd

- Corrected a real boot-order hole: the launchd-enabled path previously entered non-returning EL0 before the Bramble DWC3/Type-C handoff. The kernel now installs the frame allocator/VFS, completes USB handoff, then enters `/bin/launchd`.
- QEMU smoke `/tmp/fullerene-aarch64-usb-before-launchd-smoke.log` and AArch64 check `/tmp/fullerene-aarch64-usb-before-launchd-check-fixed.log` passed. Latest temporary Bramble image `/tmp/fullerene-bramble-aarch64-launchd.img` passed its boot audit with SHA-256 `21d7be84213ce83ba8015b5561a85f520d497e87d97fb2d950faa2e4c28428ab` (2,473,984 bytes); it is not yet physically booted.

### 2026-09-06 — AArch64 Bramble launchd image preparation and current descriptor

- The earlier launchd-enabled temporary Bramble image `/tmp/fullerene-bramble-aarch64-launchd.img` after the VFS switch had SHA-256 `bf596af44afca263a0337d362e73d767c1784a286ce275cac738b5050d581337`; the current reordered image and its audit are recorded in the preceding entry. It has not yet been booted because the handset was not in Fastboot during this build.
- Read-only capture `/tmp/fullerene-bramble-1234-descriptor-current.txt` confirms the connected Pixel host identity `1234:0001`, USB 3.20, one vendor-specific interface, and bulk EP1 IN/OUT. This proves the host-visible identity, not execution of the newly built Fullerene image. No flash, erase, partition write, analyzer, or user-data operation was used.

### 2026-09-06 — AArch64 VFS file write and read-back

- `WRITE` now handles owner/generation-checked VFS file handles as well as UART descriptors. It copies a bounded 4 KiB EL0 buffer and calls Genome `Vfs::write_at`; the initramfs contains an empty `/etc/write-test` used by launchd for write, close, reopen, read-back, and UART output.
- Dedicated and generic QEMU logs `/tmp/fullerene-dedicated-aarch64-vfs-write-smoke.log` and `/tmp/fullerene-generic-aarch64-vfs-write-smoke.log` both show `WRITE=21`, read-back `READ=21`, `AArch64 VFS write ok`, and `returned to EL1h`; the final 15-second timeout is intentional. No synchronous exception or allocator exhaustion occurred.
- Checks passed: `/tmp/fullerene-aarch64-check-vfs-write.log`, `/tmp/fullerene-host-check-vfs-write.log`, `/tmp/fullerene-abi-test-vfs-write.log` (11 ABI tests), rustfmt, and `git diff --check`. Temporary Bramble artifact `/tmp/fullerene-bramble-aarch64-vfs-write.img` passed audit with SHA-256 `cf84125a008c71d50f1c20233f0fb5ebadce21c87b7c2fc4a487486f6f60b9f7` and size 2,473,984 bytes; it has not been physically booted.

### 2026-09-06 — AArch64 pipe, handle duplication, and fork inheritance

- Wired ABI `PIPE_CREATE=83` and `HANDLE_DUPLICATE=91` into the native AArch64 syscall path. Pipes are bounded 4 KiB byte queues with directional handles, owner/generation validation, duplicate reference accounting, fork inheritance, and cleanup on close/exit/failed fork; empty/full operations return `-EAGAIN`.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-pipe-fork-smoke.log` and `/tmp/fullerene-generic-aarch64-pipe-fork-smoke.log` show duplicate write-end creation, parent close, child PID 12 inheriting the write handle, child status 16, parent `WAIT`, read/write count 16, `AArch64 pipe ok`, and `returned to EL1h`; only the final wait-loop timeout is intentional.
- Checks passed: `/tmp/fullerene-aarch64-pipe-fork-check.log`, `/tmp/fullerene-host-check-pipe-fork.log`, `/tmp/fullerene-abi-test-pipe-fork.log` (11 tests), rustfmt, and `git diff --check`. Temporary Bramble image `/tmp/fullerene-bramble-aarch64-pipe-fork.img` passed audit with SHA-256 `92950f6168e3d3dab081b6528cb1e222e8518488a330165c7aac878b13e431d7` and size 2,478,080 bytes; physical boot remains unverified.

### 2026-09-06 — AArch64 channel create/send/receive

- Added native `CHANNEL_CREATE=80`, `CHANNEL_SEND=81`, and `CHANNEL_RECV=82`. The bounded static queue supports eight channels, sixteen messages each, and 4 KiB per message; duplicated channel handles share the queue and release references on close/fork/exit.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-channel-smoke.log` and `/tmp/fullerene-generic-aarch64-channel-smoke.log` show create, duplicate, send/receive count 19, `AArch64 channel ok`, and `returned to EL1h`; only the final wait-loop timeout is intentional.
- Checks passed: `/tmp/fullerene-aarch64-channel-check.log`, `/tmp/fullerene-host-check-channel.log`, `/tmp/fullerene-abi-test-channel.log` (11 tests), rustfmt, and `git diff --check`. Temporary Bramble image `/tmp/fullerene-bramble-aarch64-channel.img` passed audit with SHA-256 `2f21efb6db21f1c26fdc97c7b340eaea35114a80512a856e384ad087100f3d15` and size 2,482,176 bytes; physical boot remains unverified.

### 2026-09-06 — AArch64 clock, uptime, and sleep syscalls

- Corrected native `UPTIME=103` from raw counter ticks to microseconds and added `CLOCK_GETTIME=100` plus `SLEEP=102`, all derived from `CNTPCT_EL0`/`CNTFRQ_EL0` with checked user copies.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-time-smoke.log` and `/tmp/fullerene-generic-aarch64-time-smoke.log` show the four time calls, `AArch64 time ok`, and `returned to EL1h`; only the final 15-second timeout is intentional.
- Checks passed in `/tmp/fullerene-aarch64-time-check.log`, `/tmp/fullerene-host-time-check.log`, `/tmp/fullerene-abi-time-test.log` (11 tests), `/tmp/fullerene-fmt-time-check.log`, and `/tmp/fullerene-diff-time-check.log`. Bramble image `/tmp/fullerene-bramble-aarch64-time.img` passed QEMU preflight/boot audit; SHA-256 `bb0bb2d256d68f65984f8cff3886f02d18cef127b851e104a26460e2d4ea2e70`, size 2,301,952 bytes.
- `SLEEP` is presently a bounded busy-wait rather than scheduler-backed timer blocking; physical boot remains unverified because Fastboot was empty.

### 2026-09-06 — AArch64 event create, wait, signal, and subscription

- Added bounded native `CREATE_EVENT=40`, `WAIT_EVENT=41`, `SIGNAL_EVENT=42`, and `SUBSCRIBE_EVENT=43` with auto/manual-reset state, handle reference accounting, event blocking, and timer-IRQ timeout wake support.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-event-smoke.log` and `/tmp/fullerene-generic-aarch64-event-smoke.log` show parent event blocking, child signal through the inherited handle, `AArch64 event ok`, the channel/pipe/time regressions, and `returned to EL1h`; only the final timeout is intentional.
- Checks passed in `/tmp/fullerene-aarch64-event-check.log`, `/tmp/fullerene-host-event-check.log`, `/tmp/fullerene-abi-event-test.log` (11 tests), `/tmp/fullerene-fmt-event-check.log`, and `/tmp/fullerene-diff-event-check.log`. Bramble image `/tmp/fullerene-bramble-aarch64-event.img` passed QEMU preflight/boot audit; SHA-256 `34457328c185205089da513da9c913ccd5995394fbf3cb169d954a062548f5cc`, size 2,306,048 bytes.
- Subscription delivery is still validation-only and timer timeout resolution is bounded by the current 100 ms IRQ cadence; physical boot remains unverified because Fastboot was empty.

### 2026-09-06 — AArch64 capability transfer and revoke

- Wired native `HANDLE_TRANSFER=90` and `HANDLE_REVOKE=92`. Transfer moves an owner/generation-checked handle row to a live target PID with a fresh target slot/generation and no second resource retain; revoke uses the same close path and reference accounting.
- `/tmp/fullerene-dedicated-aarch64-cap-smoke.log` passed the transfer probe: child PID 12 closed its inherited event capability, parent transferred the event, sent the returned handle through the pipe, child signaled with it, and parent observed source invalidation, completion, child status 0, `AArch64 event ok`, channel/pipe/time regressions, and `returned to EL1h`; only the final timeout is intentional.
- `/tmp/fullerene-generic-aarch64-cap-smoke.log` reproduced the transfer lines and all existing smoke messages. `/tmp/fullerene-host-cap-check.log`, `/tmp/fullerene-abi-cap-test.log`, `/tmp/fullerene-fmt-cap-check.log`, and `/tmp/fullerene-diff-cap-check.log` passed. Bramble artifact `/tmp/fullerene-bramble-aarch64-cap.img` passed audit, SHA-256 `65d11b3ef9f8fab71466a9da0aea5acc24a4caa25d6affc9d94cc9740f57dcd7`, size 2,306,048 bytes. Physical boot remains pending; Fastboot was empty and no device mutation was attempted.

### 2026-09-06 — AArch64 shared-buffer create, map, fork visibility, and unmap

- Added native `SHARED_BUFFER_CREATE=34`, `SHARED_BUFFER_MAP=35`, and `SHARED_BUFFER_UNMAP=36`. The bounded path allocates zeroed DTB-backed frames, maps requested rights, tracks process-local mappings, prevents close/transfer while mapped, and preserves shared writable frames across fork instead of applying private COW. Exec/exit cleanup removes mappings before address-space teardown.
- Final dedicated `/tmp/fullerene-dedicated-aarch64-shared-final-smoke.log` and generic `/tmp/fullerene-generic-aarch64-shared-cow-fork-smoke.log` both report `AArch64 shared buffer ok`, all prior event/channel/pipe/time messages, and `returned to EL1h`; only the final timeout is intentional. `/tmp/fullerene-aarch64-shared-cow-check.log`, `/tmp/fullerene-host-shared-buffer-check.log`, `/tmp/fullerene-abi-shared-buffer-test.log`, `/tmp/fullerene-fmt-shared-buffer-check.log`, and `/tmp/fullerene-diff-shared-buffer-check.log` passed.
- Bramble artifact `/tmp/fullerene-bramble-aarch64-shared-buffer.img` passed audit, SHA-256 `0a099b068f4898d19c23819fc4248bbaca08934292da67724104f5ab12c2a605`, size 2,318,336 bytes. Physical boot remains pending; Fastboot was empty and no device mutation was attempted.

### 2026-09-06 — post-build read-only handset state recheck

- `fastboot devices` remained empty; `adb devices` reported serial `26191JECB00076` as `no permissions`; `lsusb` still showed `1234:0001 Brain Actuated Technologies Pixel 4a (5G)`. This does not attribute the current host-visible identity to the new image; no device mutation was attempted.

### 2026-09-06 — AArch64 ProcessControl assign, stop, status, and reap

- Added native `PROCESS_CONTROL_STOP=111` and `PROCESS_CONTROL_ASSIGN=114`. Bounded task slots now retain a separate supervisor PID; parent/supervisor control opening, supervisor assignment, PID-1 protection, remote target retirement, child reparenting, parent wake, owned-capability cleanup, and exit-code preservation are implemented. Exited address-space teardown now excludes recorded shared-buffer ranges, fixing the shared-frame reclamation boundary for both normal exit and control-stop.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-process-control-stop-assign-launchd.log` and `/tmp/fullerene-generic-aarch64-process-control-stop-assign-launchd.log` both show `ASSIGN=0`, `STOP=0`, `STATUS=3`, `REAP=37`, `AArch64 process control ok`, all prior shared/event/channel/pipe/time/VFS messages, and `returned to EL1h`; only the final wait-loop timeout is intentional. The first probe attempt had a signed-handle check in the user payload and was corrected before this result.
- Checks passed in `/tmp/fullerene-aarch64-process-control-stop-assign-check.log`, `/tmp/fullerene-host-process-control-stop-assign-check.log`, `/tmp/fullerene-abi-process-control-stop-assign-test.log`, `/tmp/fullerene-fmt-process-control-stop-assign-check.log`, and `/tmp/fullerene-diff-process-control-stop-assign-check.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-process-control-stop-assign.img` passed audit; SHA-256 `91724f40170cfcdde174677fbad9f8d2e5dbc701e77fe2138015af43a5a14401`, size `2,506,752` bytes. Physical boot remains unverified: Fastboot was empty, ADB lacked permissions, and the read-only host descriptor remained `1234:0001`; no device mutation was attempted.

### 2026-09-06 — AArch64 ProcessControl teardown correction retake

- Corrected remote-stop teardown to reassign both parent and designated supervisor of adopted children to init, and normalized stop exit codes to the ABI low-32-bit form. Dedicated/generic r2 QEMU logs `/tmp/fullerene-dedicated-aarch64-process-control-stop-assign-launchd-r2.log` and `/tmp/fullerene-generic-aarch64-process-control-stop-assign-launchd-r2.log` again report `AArch64 process control ok`, `STATUS=3`, `REAP=37`, all previous smoke markers, and `returned to EL1h`; only the intentional timeout is nonzero.
- R2 checks passed in `/tmp/fullerene-aarch64-process-control-stop-assign-check-r2.log`, `/tmp/fullerene-host-process-control-stop-assign-check-r2.log`, `/tmp/fullerene-abi-process-control-stop-assign-test-r2.log`, `/tmp/fullerene-fmt-process-control-stop-assign-check-r2.log`, and `/tmp/fullerene-diff-process-control-stop-assign-check-r2.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-process-control-stop-assign-r2.img` passed audit; SHA-256 `388bf7917096465d46ebd43ecce9f39b6e7c9b2d87c111f3d4f7bef43a7a46ea`, size `2,506,752` bytes. The final physical recheck remained Fastboot-empty, ADB `no permissions`, and host-visible `1234:0001`; no device mutation was attempted.

### 2026-09-06 — AArch64 TimerCreate one-shot event boundary

- Added native `TIMER_CREATE=101` with a bounded static pool of eight one-shot timers. Timers keep an absolute monotonic-nanosecond deadline and the exact event owner/generation; expiry revalidates the capability before signaling, and timer references are released through close/exit cleanup. This prevents a closed, reused, transferred, forked, or exited event capability from receiving a stale timer signal.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-timer-create-smoke.log` and `/tmp/fullerene-generic-aarch64-timer-create-smoke.log` show `0x65` returning `0x4000000000001401`, sleep completion, timer/event close returns `0`, `AArch64 timer ok`, all prior smoke markers, and `returned to EL1h`; only the final timeout is intentional. Launchd starts before GIC setup, so the sleep progress boundary exercises expiry there; the timer IRQ path also checks expiry after setup.
- Checks passed in `/tmp/fullerene-aarch64-timer-create-check.log`, `/tmp/fullerene-host-timer-create-check.log`, `/tmp/fullerene-abi-timer-create-test.log`, `/tmp/fullerene-fmt-timer-create-check.log`, and `/tmp/fullerene-diff-timer-create-check.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-timer-create.img` passed audit; SHA-256 `d6eaf4429e86917a22bdb4db895763c9afa76fb95150ec2fe930aa1d50f16961`, size `2,506,752` bytes.
- Physical recheck `/tmp/fullerene-timer-create-physical-recheck.log` remains read-only: Fastboot empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001`. The artifact was not booted and no device mutation was attempted.

### 2026-09-06 — AArch64 thread create, join, detach, and exit boundary

- Added native `CREATE_THREAD=50`, `JOIN_THREAD=51`, `DETACH_THREAD=52`, and `EXIT_THREAD=53`. Threads use separate scheduler/PID slots and saved EL0 frames while sharing the creator's TTBR0/address space and process-owned handles. Join blocks and receives the target's exit status; detach and last-handle close make later exit self-reclaiming, while join/exit cleanup never releases the shared address-space root.
- Dedicated/generic QEMU `/tmp/fullerene-dedicated-aarch64-thread-smoke-r2.log` and `/tmp/fullerene-generic-aarch64-thread-smoke-r2.log` both show the join switch, `AArch64 thread ok`, all previous smoke markers, and `returned to EL1h`; only the intentional 20-second timeout follows. The probe enters an ELF-resident thread function, calls `EXIT_THREAD` with status `37`, then the parent closes the handle and unmaps the 8 KiB stack mapping.
- Checks passed in `/tmp/fullerene-aarch64-thread-check.log`, `/tmp/fullerene-host-thread-check.log`, `/tmp/fullerene-abi-thread-test.log`, `/tmp/fullerene-fmt-thread-check.log`, and `/tmp/fullerene-diff-thread-check.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-thread.img` passed audit; SHA-256 `2070dc8277bd82fdf6ff3d4b48f31d0936a156ca49dcbe87b0e4c0a3f2fdfdee`, size `2,510,848` bytes.
- Physical recheck `/tmp/fullerene-thread-physical-recheck.log` remains read-only: Fastboot empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001`. The artifact was not booted and no device mutation was attempted.

### 2026-09-06 — AArch64 thread detach follow-up retake

- Extended the smoke with a detached thread: `DETACH_THREAD=52`, `YIELD`, target `EXIT_THREAD=53`, then last-handle close and shared-stack unmap. The close path now reaps an already-exited detached slot so a detached thread cannot remain as a stale scheduler entry after its capability disappears.
- Dedicated/generic `/tmp/fullerene-dedicated-aarch64-thread-detach-r3.log` and `/tmp/fullerene-generic-aarch64-thread-detach-r3.log` report the join path, detached target exit, zero close/unmap results, `AArch64 thread ok`, all previous markers, and `returned to EL1h`; only the intentional 20-second timeout follows.
- R2 checks passed in `/tmp/fullerene-aarch64-thread-check-r2.log`, `/tmp/fullerene-host-thread-check-r2.log`, `/tmp/fullerene-abi-thread-test-r2.log`, `/tmp/fullerene-fmt-thread-check-r2.log`, and `/tmp/fullerene-diff-thread-check-r2.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-thread-r3.img` passed audit; SHA-256 `cc63d4d4a484fdab453e94199c252d22b3b35c8447770da9f0de653bb6553ac4`, size `2,510,848` bytes.
- Physical recheck `/tmp/fullerene-thread-physical-recheck-r3.log` remains read-only: Fastboot empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001`. The r3 artifact was not booted and no device mutation was attempted.

### 2026-09-06 — AArch64 thread shared-capability close correction retake

- Corrected the thread-resource reference boundary: closing one duplicated or fork-inherited thread handle no longer detaches the target while another reference remains; implicit detach now occurs only when the last reference is closed, while explicit `DETACH_THREAD` remains immediate.
- Dedicated/generic `/tmp/fullerene-dedicated-aarch64-thread-r4.log` and `/tmp/fullerene-generic-aarch64-thread-r4.log` again report joined and detached thread paths, `AArch64 thread ok`, all previous markers, and `returned to EL1h`; only the intentional 20-second timeout follows.
- R4 checks passed in `/tmp/fullerene-aarch64-thread-check-r4.log`, `/tmp/fullerene-host-thread-check-r4.log`, `/tmp/fullerene-abi-thread-test-r4.log`, `/tmp/fullerene-fmt-thread-check-r4.log`, and `/tmp/fullerene-diff-thread-check-r4.log`. Bramble artifact `/tmp/fullerene-bramble-aarch64-thread-r4.img` passed audit; SHA-256 `6ed587adc6c31888de822853942b05393e67f2dd0cdcbcde047f28df23aa9dc8`, size `2,510,848` bytes.
- Final physical recheck `/tmp/fullerene-thread-physical-recheck-r4.log` remains read-only: Fastboot empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001`. The r4 artifact was not booted and no device mutation was attempted.

### 2026-09-07 — AArch64 scheduler-backed SLEEP initial retake

- Replaced the normal AArch64 `SLEEP=102` architectural-counter spin with a scheduler wait record: the current frame is saved, the task is blocked until a monotonic deadline, and the timer wake path returns success for sleep while retaining `-ETIMEDOUT` for event waits. A `sleep_wait` discriminator also prevents event slot `0` from being confused with a sleep.
- Initial dedicated `/tmp/fullerene-dedicated-aarch64-sleep-r1.log` and generic `/tmp/fullerene-generic-aarch64-sleep-r1.log` passed the existing `AArch64 time ok`, timer/thread/VFS markers, and `returned to EL1h`, but launchd still ran before GIC setup; this run therefore exercised the documented pre-IRQ counter fallback, not the new scheduler path.
- Checks passed in `/tmp/fullerene-aarch64-sleep-check-r1.log`, `/tmp/fullerene-host-sleep-check-r1.log`, `/tmp/fullerene-abi-sleep-test-r1.log`, and `/tmp/fullerene-fmt-sleep-check-r1.log`; `git diff --check` is `/tmp/fullerene-diff-sleep-check-r1.log`.

### 2026-09-07 — AArch64 SLEEP timer-IRQ retake and GIC ordering correction

- Kept the Bramble USB handoff first, then initialized the platform GIC/timer and published IRQ readiness before entering launchd. The launchd time probe now keeps a helper child runnable, sleeps PID 1, then stops/reaps the helper after timer wake; this makes the scheduler-backed path reachable rather than silently falling back because there is no peer task.
- Dedicated/generic `/tmp/fullerene-dedicated-aarch64-sleep-r2.log` and `/tmp/fullerene-generic-aarch64-sleep-r2.log` both reached `user-smoke: sleep task=0`, `sleep switch to=1`, `AArch64 time ok`, all later markers, and `returned to EL1h`. The r2 run exposed excessive UART output from logging every 1 ms timer IRQ; no synchronous fault or allocator exhaustion occurred, but the diagnostic path was corrected before the final retake.
- R2 checks passed in `/tmp/fullerene-fmt-sleep-check-r2.log`, `/tmp/fullerene-aarch64-sleep-check-r2.log`, `/tmp/fullerene-host-sleep-check-r2.log`, `/tmp/fullerene-abi-sleep-test-r2.log`, and `/tmp/fullerene-diff-sleep-check-r2.log`.

### 2026-09-07 — AArch64 SLEEP final timer-IRQ retake

- Timer IRQs are no longer printed as unexpected IRQ diagnostics, preventing the 1 ms architectural timer cadence from flooding UART after the user smoke returns to EL1h. The scheduler wait, deadline wake, and single-task/pre-IRQ fallback boundaries remain explicit.
- Final dedicated `/tmp/fullerene-dedicated-aarch64-sleep-r3.log` and generic `/tmp/fullerene-generic-aarch64-sleep-r3.log` are compact (2,125 and 2,137 lines), both show the sleep task switch, `AArch64 time ok`, `AArch64 VFS write ok`, `returned to EL1h`, and no `esr`, panic, or allocator-exhaustion record. The command's nonzero result is only the intentional 20-second post-EL1 timeout.
- Final checks are `/tmp/fullerene-fmt-sleep-check-r3.log`, `/tmp/fullerene-aarch64-sleep-check-r3.log`, `/tmp/fullerene-host-sleep-check-r3.log`, `/tmp/fullerene-abi-sleep-test-r3.log`, and `/tmp/fullerene-diff-sleep-check-r3.log`.
- Final Bramble artifact `/tmp/fullerene-bramble-aarch64-sleep-r2.img` passed audit; SHA-256 `cac998e53f66ba136f1aa3688836d5c036be7db0b7b7645886a0c2d4aa7adce5`, size `2,514,944` bytes, build log `/tmp/fullerene-bramble-sleep-build-r2.log`. Read-only handset recheck `/tmp/fullerene-sleep-physical-recheck-r2.log` still shows empty Fastboot, ADB serial `26191JECB00076` with `no permissions`, and host-visible `1234:0001`; the artifact was not booted and no device mutation was attempted.

### 2026-09-07 — AArch64 ProcessControl capability lifecycle completion

- Replaced the AArch64 raw process-control token with a bounded owner-local capability table. Tokens now carry owner PID, target PID, and a generation; `CLOSE=6`, `HANDLE_DUPLICATE=91`, `HANDLE_TRANSFER=90`, and `HANDLE_REVOKE=92` dispatch process-control tokens through the scheduler instead of treating them as file descriptors. Transfer is a move with a fresh destination generation, duplicate retains an independent entry, and stale/closed tokens fail with `-EBADHANDLE`.
- The launchd probe now exercises duplicate, close, same-process transfer, assign, stop, status, reap, revoke, and stale-token rejection. `/tmp/fullerene-generic-aarch64-proc-cap-r5.log` prints `AArch64 process control ok`, shows `STATUS=3` and `REAP=37` in the syscall trace, and later reaches `AArch64 time ok`, `AArch64 VFS write ok`, and `returned to EL1h`; its nonzero harness status is only the intentional post-EL1 timeout. The earlier r4 log also exposed the signed high-bit smoke-check bug; the final probe uses the process-handle tag, not `i64 >= 0`.
- Checks passed in `/tmp/fullerene-fmt-proc-cap-r1.log`, `/tmp/fullerene-aarch64-proc-cap-r1-check.log`, `/tmp/fullerene-host-proc-cap-r1-check.log`, `/tmp/fullerene-abi-proc-cap-r1-test.log`, and `/tmp/fullerene-diff-proc-cap-r1.log`. The Bramble Android-v3 boot image `/tmp/fullerene-bramble-aarch64-proc-cap-r1.img` passed audit; SHA-256 `1ebbde8797c79f04f039622311f1e9352bb9207c62ef0a4b97a29830aa096514`, size `2,519,040` bytes, build log `/tmp/fullerene-bramble-proc-cap-r1-build.log`.
- Read-only physical recheck `/tmp/fullerene-proc-cap-physical-recheck-r1.log` at `2026-09-07T00:25:49+09:00` still shows empty Fastboot, ADB serial `26191JECB00076` with `no permissions`, and host-visible `1234:0001` with the same Pixel 4a (5G) bulk endpoints. The artifact was not booted; no flash, erase, partition write, analyzer, or user-data operation was performed.

### 2026-09-07 — AArch64 spawn supervisor relationship parity

- Native AArch64 `SPAWN` now consumes the ABI's nonzero `supervisor_pid` argument (arg6). Zero retains the existing parent-as-supervisor default; a nonzero value must name a live non-thread task or the syscall returns `-ESRCH`. Forked children explicitly retain the parent as supervisor, and ProcessControl assignment now rejects thread PIDs.
- Generic QEMU `/tmp/fullerene-generic-aarch64-supervisor-r1.log` generated children through the explicit PID-1 supervisor path (`aarch64 spawn pid=0x3` and `0xd`), then retained `AArch64 process control ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `returned to EL1h`; the only nonzero harness result is the intentional 25-second timeout.
- Checks passed in `/tmp/fullerene-fmt-supervisor-r1.log`, `/tmp/fullerene-aarch64-supervisor-r1-check.log`, `/tmp/fullerene-host-supervisor-r1-check.log`, `/tmp/fullerene-abi-supervisor-r1-test.log`, and `/tmp/fullerene-diff-supervisor-r1.log` (all status `0`). The Bramble Android-v3 boot image `/tmp/fullerene-bramble-aarch64-supervisor-r1.img` passed audit; SHA-256 `099aaf31fe4f1671e48e8598f37614fc314287cbe5f4ce23b576be535ba08864`.
- Read-only physical recheck `/tmp/fullerene-supervisor-physical-recheck-r1.log` at `2026-09-07T00:35:32+09:00` remains Fastboot-empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001` for the same Pixel 4a (5G) bulk interface. The artifact was not booted and no flash, erase, partition write, analyzer, or user-data operation was used.

### 2026-09-07 — AArch64 native terminal capability and spawn attachment

- Connected native `CREATE_TERMINAL=65` to the AArch64 capability table. Titles cross the EL0 copy boundary with bounded length, UTF-8, and control-character validation; the resulting owner/generation handle writes through the platform UART, returns `-EAGAIN` for the currently nonblocking read side, and releases its bounded terminal slot through close, fork/child inheritance, failed-spawn rollback, and process-exit cleanup.
- Extended native `SPAWN` so arg5 attaches a terminal capability to the new child while arg6 selects its supervisor. The launchd probe creates a terminal, writes `AArch64 terminal ok`, spawns and reaps a child with the terminal attached, then closes the parent handle. Generic QEMU `/tmp/fullerene-generic-aarch64-terminal-r1.log` shows the terminal marker twice (capability write and the post-close fd1 verification), followed by `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; status `1` is only the intentional 25-second harness timeout. The child attachment is validated by the successful spawn/exit path and the kernel-side fresh child capability installation; the child image does not yet issue a terminal-specific operation.
- Checks passed: `/tmp/fullerene-fmt-terminal-r1.log`, `/tmp/fullerene-aarch64-terminal-r1-check.log`, `/tmp/fullerene-host-terminal-r1-check.log`, `/tmp/fullerene-abi-terminal-r1-test.log`, and `/tmp/fullerene-diff-terminal-r1.log` (all status `0`). Bramble `/tmp/fullerene-bramble-aarch64-terminal-r1.img` passed the Android v3 boot audit (`2,523,136` bytes); SHA-256 `d3487a3314cb8aa3e3f4f77c0d313d13a37ebfcba4beee5e38a176b6e13ecbcb`, build log `/tmp/fullerene-bramble-terminal-r1-build.log`.
- Read-only physical recheck `/tmp/fullerene-terminal-physical-recheck-r1.log` at `2026-09-07T00:46:01+09:00` remains Fastboot-empty, ADB serial `26191JECB00076` with `no permissions`, and host-visible `1234:0001` for the same Pixel 4a (5G) bulk interface. The artifact was not booted; no flash, erase, partition write, analyzer, or user-data operation was performed.

### 2026-09-07 — AArch64 native device inventory from the USB descriptor

- Connected native `ENUMERATE_DEVICES=70` to a bounded platform inventory. On Bramble, the USB row is published only after the USB2 gadget handoff (including its cold fallback) succeeds, and its `vendor_id`/`product_id` are decoded from `usb_protocol::DEVICE_DESCRIPTOR`, not duplicated constants: `0x1234:0x0001`, class `USB`, local device id `1`. A failed handoff publishes no row. This is the configured Fullerene gadget identity, not a claim that the kernel has enumerated an external USB host device.
- The syscall validates a non-null, bounded user buffer, filters by ABI device class, copies all fitting `DeviceInfo` records, and returns the total matching count. `OpenDevice`/device ioctls remain separate work; this step deliberately implements only the inventory boundary.
- Generic QEMU `/tmp/fullerene-generic-aarch64-devices-r1.log` reports `AArch64 device inventory ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; status `1` is only the intentional 25-second harness timeout. QEMU has no Bramble gadget row, so the smoke accepts the valid empty inventory there while the Bramble build compiles the descriptor-backed path.
- Checks passed in `/tmp/fullerene-fmt-devices-r1.log`, `/tmp/fullerene-aarch64-devices-r1-check.log`, `/tmp/fullerene-host-devices-r1-check.log`, `/tmp/fullerene-abi-devices-r1-test.log`, and `/tmp/fullerene-diff-devices-r1.log` (all status `0`). Bramble `/tmp/fullerene-bramble-aarch64-devices-r1.img` passed the Android v3 boot audit (`2,523,136` bytes); SHA-256 `4f551c1fb51b7049b2ce72e3342d6933b07be582b1d997efc3b0e5cb166b2311`, build log `/tmp/fullerene-bramble-devices-r1-build.log`.
- Read-only physical recheck `/tmp/fullerene-devices-physical-recheck-r1.log` at `2026-09-07T00:58:12+09:00` remains Fastboot-empty, ADB serial `26191JECB00076` with `no permissions`, and `lsusb` shows `1234:0001` / Pixel 4a (5G), vendor-specific bulk interface. No artifact boot, flash, erase, partition write, analyzer, or user-data operation was performed.

### 2026-09-07 — AArch64 native OpenDevice and read-only device capability

- `OpenDevice=71` now accepts the descriptor-backed Bramble identity as `1234:0001`, `/dev/usb/1234:0001`, `usb/1234:0001`, or local id `1`. The returned handle is a bounded owner/generation-checked filesystem resource, so duplicate, close, fork inheritance, owner cleanup, and stale-handle rejection use the existing capability lifecycle. Unknown identifiers return `-ENODEV`; malformed or overlong EL0 identifiers are bounded and rejected.
- `DeviceIoctl=72` currently implements only `GET_CAPABILITIES=8`. For the identity-only USB row it returns class `USB` and zero driver capability flags. PCI/MMIO/block operations and actual external-device control remain unsupported; this is an openable identity resource, not a completed USB host driver.
- Generic QEMU `/tmp/fullerene-generic-aarch64-device-open-r1.log` reports `AArch64 device inventory ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; its status `/tmp/fullerene-generic-aarch64-device-open-r1.status` is `1` only because the harness intentionally times out after the smoke returns. QEMU has an empty inventory, so this run verifies the no-row path; the descriptor-backed open/ioctl path is compiled into the Bramble image but was not claimed as physically booted.
- Formatting, AArch64/host checks, ABI tests, and diff check passed in `/tmp/fullerene-fmt-device-open-r1.log`, `/tmp/fullerene-aarch64-device-open-r1-check.log`, `/tmp/fullerene-host-device-open-r1-check.log`, `/tmp/fullerene-abi-device-open-r1-test.log`, and `/tmp/fullerene-diff-device-open-r1.log`. Bramble `/tmp/fullerene-bramble-aarch64-device-open-r1.img` passed the Android v3 boot audit; size `2,527,232` bytes, SHA-256 `9d1778cc78ebceb3f36d3b00b966b2b703ce4b553be51435ed83d0f29d445fd4`, build log `/tmp/fullerene-bramble-device-open-r1-build.log`.
- Read-only handset/host check `/tmp/fullerene-device-open-physical-recheck-r1.log` at `2026-09-07T01:06:19+09:00` remains Fastboot-empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001` / Pixel 4a (5G) with the vendor-specific bulk interface. No boot, flash, erase, partition write, analyzer, or user-data operation was used.

### 2026-09-07 — AArch64 headless Window capability boundary

- Native `CreateWindow=60`, `DestroyWindow=61`, `ResizeWindow=62`, `PresentWindow=63`, and `GetWindowEvent=64` now use the same owner/generation-checked capability table as files and devices. The bounded slot validates dimensions up to `16,384`, supports duplicate/close/fork/owner cleanup, and emits one checked ABI `WindowEvent::REDRAW` after `PresentWindow`; invalid event destinations do not consume the pending event.
- The first QEMU attempt returned `-EBADF` because the new slot initialization omitted its `active` bit. The failure was corrected before the final retake. Final QEMU `/tmp/fullerene-generic-aarch64-window-r1.log` reports `AArch64 window ok`, `AArch64 device inventory ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; status `/tmp/fullerene-generic-aarch64-window-r1.status` is `1` only for the intentional 25-second harness timeout.
- This is deliberately headless: no framebuffer, display controller, pixel buffer, or physical screen output is claimed. The slot/event contract is ready for a real AArch64 display backend, which remains separate work.
- Formatting, AArch64/host checks, ABI tests, and diff check passed in `/tmp/fullerene-fmt-window-r1.log`, `/tmp/fullerene-aarch64-window-r1-check.log`, `/tmp/fullerene-host-window-r1-check.log`, `/tmp/fullerene-abi-window-r1-test.log`, and `/tmp/fullerene-diff-window-r1.log`. Bramble `/tmp/fullerene-bramble-aarch64-window-r1.img` passed the Android v3 boot audit; size `2,531,328` bytes, SHA-256 `d7d7721e86af545f162f7e413caa7d56a69c0b8cf7da6b52c61c76b13bcd7294`, build log `/tmp/fullerene-bramble-window-r1-build.log`.
- Read-only handset/host check `/tmp/fullerene-window-physical-recheck-r1.log` at `2026-09-07T01:15:21+09:00` remains Fastboot-empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001` / Pixel 4a (5G) with the same vendor-specific bulk interface. The Bramble image was not booted and no flash, erase, partition write, analyzer, or user-data operation was used.

### 2026-09-07 — AArch64 DTB-derived Bramble platform inventory

- Native inventory now consumes the supplied DTB in addition to the USB descriptor. The matching factory `vendor_boot.dtb` contains the source-visible `qcom,ufshc`, two `qcom,sdhci-msm-v5`, and `qcom,mdss_mdp` nodes; when the bootloader's merged DTB enables those nodes, they become bounded STORAGE/DISPLAY identity rows with local ids `2..5`. The USB gadget remains local id `1` with VID/PID `0x1234:0x0001`. Platform rows intentionally keep vendor/product zero because they are not PCI/USB identities.
- This remains discovery, not driver control: DT `reg` existence is checked through the existing FDT parser, but no Qualcomm UFS, SDHCI, or MDSS MMIO is touched and their capability flags remain zero. `OpenDevice` can address these rows by local id; unsupported I/O still returns `-ENOTSUP`.
- Generic QEMU `/tmp/fullerene-generic-aarch64-platform-devices-r1.log` retains `AArch64 device inventory ok`, `AArch64 window ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; status `/tmp/fullerene-generic-aarch64-platform-devices-r1.status` is `1` only for the intentional timeout. QEMU's non-Bramble DT has no matching Qualcomm rows, so its valid empty path remains unchanged.
- The offline aligned-buffer FDT check `/tmp/fullerene-fdt-platform-check.log` passed the parser's own tests and verified QEMU's `arm,gic-v3`/`arm,pl011` lookup, but the unmerged factory base DTB and standalone DTBO fragments returned no enabled Qualcomm rows; their node/compatible strings are present while activation is supplied by the bootloader's DT merge. Therefore no physical platform-row enumeration is claimed yet.
- Formatting, AArch64/host checks, ABI tests, and diff check passed in `/tmp/fullerene-fmt-platform-devices-r1.log`, `/tmp/fullerene-aarch64-platform-devices-r1-check.log`, `/tmp/fullerene-host-platform-devices-r1-check.log`, `/tmp/fullerene-abi-platform-devices-r1-test.log`, and `/tmp/fullerene-diff-platform-devices-r1.log`. Bramble `/tmp/fullerene-bramble-aarch64-platform-devices-r1.img` passed the Android v3 boot audit; size `2,531,328` bytes, SHA-256 `fa5c38f1408df397ae6de4df654c52e931dbd489da37ef53aa371b2bff3ee794`, build log `/tmp/fullerene-bramble-platform-devices-r1-build.log`.
- Read-only handset/host check `/tmp/fullerene-platform-devices-physical-recheck-r1.log` at `2026-09-07T01:24:37+09:00` remains Fastboot-empty, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001` / Pixel 4a (5G) with the same vendor-specific bulk endpoints. The image was not booted and no flash, erase, partition write, analyzer, or user-data operation was used.

### 2026-09-07 — AArch64 DT resource metadata boundary

- Added ABI `DeviceResourceInfo` and `GET_RESOURCES=12`. An opened DT-backed device now exposes its first `reg` window as `{base, size, kind=MMIO}` metadata; the ioctl never maps, reads, or writes the register window. USB rows use the `snps,dwc3`/`qcom,dwc-usb3-msm` window when the boot DT supplies one. Devices without a usable DT resource return `-ENOTSUP`, while `GET_CAPABILITIES` remains zero until a real driver claims an operation.
- The first AArch64 cross-check caught and fixed a no-std static-initialization error (`DeviceResourceInfo::default()` is not const). After changing the resource table to an explicit const zero value, formatting, AArch64/host checks, ABI tests, and `git diff --check` passed in `/tmp/fullerene-fmt-resource-r1.log`, `/tmp/fullerene-aarch64-resource-r1-check.log`, `/tmp/fullerene-host-resource-r1-check.log`, `/tmp/fullerene-abi-resource-r1-test.log`, and the final diff check.
- Generic QEMU `/tmp/fullerene-generic-aarch64-resource-r1.log` still reports `AArch64 device inventory ok`, `AArch64 window ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; `/tmp/fullerene-generic-aarch64-resource-r1.status` is `1` only for the intentional 25-second harness timeout. QEMU has no Bramble Qualcomm rows, so no resource ioctl was claimed as physically exercised.
- Bramble `/tmp/fullerene-bramble-aarch64-resource-r1.img` passed the Android v3 boot audit; size `2,531,328` bytes, SHA-256 `42ee13f8128a5be6f5f74547e0222006081700d1fa2cbb106f346d905c7d8f8d`, build log `/tmp/fullerene-bramble-resource-r1-build.log`. This is a build/audit artifact, not a booted image.
- Read-only physical recheck `/tmp/fullerene-resource-physical-recheck-r1.log` at `2026-09-07T01:35:02+09:00` shows empty Fastboot, ADB `26191JECB00076` with `no permissions`, and host-visible `1234:0001` / Pixel 4a (5G). No boot, flash, erase, partition write, analyzer, or user-data operation was performed. `tools/` remains Git-ignored (`.gitignore: /tools/`).

### 2026-09-07 — AArch64 DT resource smoke retake and ioctl-order correction

- The first QEMU resource smoke retakes (`/tmp/fullerene-generic-aarch64-resource-r2.log` through `r4`) did not print the inventory marker. A temporary read-only UART dump then showed the DT row itself was correct (`class=0xFFFF`, `id=6`, `base=0x09000000`, `size=0x1000`, `kind=MMIO`), but `GET_RESOURCES=12` returned `-ENOTSUP`. The cause was an old early guard that rejected every command other than `GET_CAPABILITIES` before reaching the new branch; the dump was removed after diagnosis.
- After allowing both commands through that guard, final QEMU `/tmp/fullerene-generic-aarch64-resource-r5.log` prints `AArch64 device inventory ok`, `AArch64 window ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`. The status `/tmp/fullerene-generic-aarch64-resource-r5.status` is `1` only for the intentional 25-second timeout. The probe now opens a non-USB DT row by local id, checks its class and zero driver flags, reads the MMIO metadata, then duplicates/closes the capability.
- Final formatting passed in `/tmp/fullerene-fmt-resource-final.log`; AArch64/host checks and ABI tests passed in `/tmp/fullerene-aarch64-resource-r5-check.log`, `/tmp/fullerene-host-resource-r5-check.log`, and `/tmp/fullerene-abi-resource-r5-test.log`, with `git diff --check` clean. The earlier `/tmp/fullerene-fmt-resource-r5.log` correctly recorded the pre-formatting failure and is retained as trial evidence. The Bramble image `/tmp/fullerene-bramble-aarch64-resource-r5.img` passed the Android v3 boot audit; size `2,531,328` bytes, SHA-256 `170bce586b08b67c04d03a006a33fe9ac4843002e47a162299150873a0cca591`, build log `/tmp/fullerene-bramble-resource-r5-build.log`. It was not booted.
- Final read-only physical recheck `/tmp/fullerene-resource-physical-recheck-r5.log` at `2026-09-07T01:45:07+09:00` shows empty Fastboot, ADB `26191JECB00076` with `no permissions`, and `lsusb` `1234:0001` / Pixel 4a (5G). No flash, erase, partition write, analyzer, or user-data operation was performed. This advances DT resource plumbing only; UFS/SDHCI/MDSS drivers and a bootloader-merged Bramble DTB observation are still outstanding.

## Document routing and context cost

| Document | Size | Use | Loading policy |
| --- | ---: | --- | --- |
| [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md) | 1,650 lines / 662.5 KB | Full Bramble ledger and source audit | Read targeted sections or this index first |
| [`HARDWARE.md`](HARDWARE.md) | 926 lines / 418.6 KB | Cross-platform hardware notes plus Bramble summary | Read the Bramble section and this index; avoid loading the full table |
| [`BUG_JOURNAL.md`](BUG_JOURNAL.md) | 1,410 lines / 65.7 KB | Historical software investigations, mostly Wi-Fi and runtime | Not needed for the Bramble USB path unless a related regression appears |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 1,037 lines / 37.9 KB | Project-wide design rules | Read only when changing architecture or ownership boundaries |
| [`BUILD.md`](BUILD.md) | 689 lines / 28.8 KB | Build and run procedures | Read the Bramble command section when running hardware |
| `docs/history/*.png` | 3.1 MB | Historical screenshots/artifacts | Do not load for USB source debugging |

The two hardware ledgers account for about 1,059.7 KB of text and contain the
only unusually long lines in this USB context: 961 rows in
`HARDWARE_aarch64.md` and 552 rows in `HARDWARE.md` exceed 200 characters;
the longest row is about 1,512 characters. The individual
experiment rows are valuable evidence, but loading the whole ledger into an
agent context repeats the same negative conclusion many times. This index is
the compact working memory; the ledgers remain the evidence archive.

## Source of truth

- Full per-run evidence: [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md)
- Cross-platform status: [`HARDWARE.md`](HARDWARE.md)
- Build commands: [`BUILD.md`](BUILD.md)
- Historical non-USB bug records: [`BUG_JOURNAL.md`](BUG_JOURNAL.md)

### 2026-09-07 — Factory Bramble DTB + DTBO 17 merge and active-node correction

- The existing same-build artifacts were merged offline with system `libfdt` by the Git-ignored helper `tools/merge_bramble_dtbo.py`; the input was `tmp/bramble-stock-vendor_boot.dtb` plus the exact factory `tmp/bramble-stock-dtbo-17.dtb` selected by the recorded `ro.boot.dtbo_idx=17`. This is a real overlay (`fragment`, `__overlay__`, `__fixups__`, and `__local_fixups__`), not a standalone board DTB. The merged DTB is `/tmp/bramble-merged-dtbo17.dtb` (749,736 bytes, SHA-256 `7bf7b0844e54708725c7c92e117aaa5b89e661ca6156b8779529d11985e2f165`); the structured report is `/tmp/bramble-merged-dtbo17.json` (SHA-256 `c78441451a3ceef35e507cbf0747013ee228f88482494360f0a859004e0ff146`).
- The merged result corrects the previous source-only assumption: active UFS is `/soc/ufshc@1d84000` (`qcom,ufshc`, status `ok`, first resources `0x1d84000/0x3000` and `0x1d90000/0x8000`, SPI 265) with active `/soc/rsc@18200000/ufsphy_mem@1d87000` (`qcom,ufs-phy-qmp-v4-lito`); both `qcom,sdhci-msm-v5` nodes are `disabled`. The display core is `/soc/qcom,mdss_mdp` with compatible `qcom,sde-kms`, the selected panel is `google,dsi_binned_lp` under `qcom,mdss_dsi_sofef01_sdc_1080p_cmd`, and the selected touch node is `/soc/spi@880000/touchscreen@0` (`st,fts`, status `ok`, interrupt cells `9,8`). No UFS/SDHCI/MDSS/touch MMIO was read.
- `devices.rs` now matches the active DT identities: display uses `qcom,sde-kms` with `qcom,mdss_mdp` fallback, and input local id `7` uses `st,fts` with Synaptics DSX fallback. Disabled SDHCI nodes are not published. The rows remain identity/resource metadata only; capability flags and block/display/input I/O remain unsupported.
- The exact AArch64 check passed in `/tmp/fullerene-aarch64-dtbo-profile-check.log` (status `0`); host Flasks check plus 11 ABI tests passed in `/tmp/fullerene-host-abi-dtbo-profile-check.log`; formatting, `git diff --check`, helper bytecode compilation, and Git-ignore verification passed in `/tmp/fullerene-dtbo-profile-format-check.log`. Generic launchd QEMU `/tmp/fullerene-generic-aarch64-dtbo-profile-launchd.log` reached `AArch64 device inventory ok`, `AArch64 window ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`; status `1` is only the intentional 12-second post-return timeout. The shorter non-launchd early-boot check is `/tmp/fullerene-generic-aarch64-dtbo-profile-qemu.log`.
- Rebuilt Bramble image `/tmp/fullerene-bramble-aarch64-dtbo-profile.img` passed the Android v3 boot audit; size `2,342,912` bytes, SHA-256 `62a376d676a149ab48c8a7196d941bd6f29e0d2ba009df26467c9e66ddb7d58f`, build log `/tmp/fullerene-bramble-dtbo-profile-build.log`. It was not booted because Fastboot was not re-entered for this trial; no flash, erase, partition write, analyzer, or user-data operation was performed. `tools/` remains outside Git via `.gitignore: /tools/`.

### 2026-09-07 — AArch64 Qualcomm UFS platform-contract boundary

- Added `fullerene-kernel/src/arch/aarch64/ufs.rs` as a read-only DT platform layer. It matches the active `qcom,ufshc` and `qcom,ufs-phy-qmp-v4-lito` nodes, retains up to two controller and two PHY `reg` windows, decodes the Qualcomm SPI interrupt cell, and records the presence/byte-length of `clocks`, `clock-names`, `resets`, `reset-names`, `power-domains`, `iommus`, controller supplies, and PHY supplies. The profile is side-effect free: it does not read or write UFS, PHY, GCC, RPMh, GIC, or IOMMU registers.
- The profile has an explicit bring-up stage enum: `Absent`, `Described`, `PowerReady`, `PhyReady`, `ControllerReady`, `LinkReady`, and `BlockReady`. Only `Described` is currently reachable. The inventory now publishes the UFS STORAGE row only when the required platform contract is present; this prevents a bare `reg` match from masquerading as an initialized storage driver. `GET_CAPABILITIES` and block ioctls remain zero/unsupported.
- This boundary follows the primary Linux Qualcomm UFS source's separation between the generic UFS host and Qualcomm platform work (`ufs-qcom.c`), rather than copying the historical docs' identity-only assumption: https://linux.googlesource.com/linux/kernel/git/torvalds/linux/+/2b414a95b8f7307d42173ba9e580d6d3e2bcbfce/drivers/ufs/host/ufs-qcom.c. The inactive SDHCI path remains separate and is not inferred from the UFS node; its Linux primary source is https://github.com/torvalds/linux/blob/master/drivers/mmc/host/sdhci-msm.c.
- First AArch64 compile caught two no-std issues in the new module: `fdt::Region` did not provide the derived comparison/debug traits requested by the profile, and `Default::default()` is not callable from the const empty initializer. The profile derives were narrowed and explicit const zero values were added. The corrected check passed in `/tmp/fullerene-aarch64-ufs-contract-check.log` (status `0`); host Flasks and 11 ABI tests passed in `/tmp/fullerene-host-ufs-contract-check.log` and `/tmp/fullerene-abi-ufs-contract-test.log`.
- Generic launchd QEMU `/tmp/fullerene-generic-aarch64-ufs-contract-launchd.log` reports the expected `ufs: no active qcom,ufshc platform contract` for QEMU, then reaches `AArch64 device inventory ok`, `AArch64 window ok`, `AArch64 time ok`, `AArch64 VFS write ok`, and `user-smoke: returned to EL1h`. The harness status is `1` only because the requested 12-second run times out after the smoke; no QEMU UFS row is fabricated.
- Bramble `/tmp/fullerene-bramble-aarch64-ufs-contract.img` passed the Android v3 boot audit; size `2,342,912` bytes, SHA-256 `072049fcc92a9abc8031ccefd9656b0bfdc45e71f764e018bb87a2999ec6968`, build log `/tmp/fullerene-bramble-aarch64-ufs-contract-build.log`. The already merged factory DTB/DTBO report supplies the expected active UFS resources, but this image was not booted because the handset was not re-entered into Fastboot for this trial. No flash, erase, partition write, analyzer, or user-data operation was performed.
- Formatting and `git diff --check` passed after the correction. The next real implementation boundary is Qualcomm power/clock/reset and UFS PHY initialization followed by UFSHCI link and descriptor access; no such MMIO sequence is claimed yet.

### 2026-09-07 — UFSHCI register and UTRD pure-layout boundary

- Added a pure `hci` layer inside `fullerene-kernel/src/arch/aarch64/ufs.rs`. It records the Linux UFSHCI register offsets through `REG_UFS_VERSION=0x08`, controller status/enable at `0x30/0x34`, transfer-list base/doorbell/run-stop at `0x50..0x60`, and UIC command arguments at `0x90..0x9c`. It also decodes device-present/list-ready/controller-ready status and the 64-bit-addressing/transfer-slot capability bits without touching MMIO.
- The UTRD serializer follows Linux's primary `include/ufs/ufshci.h`: a 16-byte request header, 64-bit command-descriptor address, and four little-endian 16-bit length/offset fields produce exactly 32 bytes. The layout constants enforce 512-byte aligned UPIU frames, 4-byte PRD granularity, and the 256 KiB PRD byte-count limit. No DMA allocation, doorbell write, UIC command, link-startup command, or block request is issued.
- This is source-aligned to [`include/ufs/ufshci.h`](https://github.com/torvalds/linux/blob/master/include/ufs/ufshci.h) and the UPIU transaction definitions in [`include/ufs/ufs.h`](https://github.com/torvalds/linux/blob/master/include/ufs/ufs.h). The Qualcomm platform contract remains a prerequisite; the bring-up stage is still only `Described`.
- AArch64 compilation passed in `/tmp/fullerene-aarch64-ufs-hci-layout-final.log` (status `0`); host Flasks and 11 ABI tests passed in `/tmp/fullerene-host-ufs-hci-layout.log` and `/tmp/fullerene-abi-ufs-hci-layout.log`. Generic QEMU `/tmp/fullerene-generic-aarch64-ufs-hci-layout.log` again reports no fabricated UFS contract and reaches the inventory/window/time/VFS/EL1 markers; status `1` is only the intentional 12-second timeout.
- Bramble `/tmp/fullerene-bramble-aarch64-ufs-hci-layout.img` passed the Android v3 boot audit and is byte-identical to the previous contract image (size `2,342,912`, SHA-256 `072049fcc92a9abc8031ccefd9656b0bfdc45e71f764e018bb87a2999ec6968`). It was not booted; no flash, erase, partition write, analyzer, or user-data operation was performed. Formatting and `git diff --check` remain clean.

### 2026-09-07 — Bramble/Lito UFS resource contract and guarded bring-up sequence

- The merged same-build factory DTB/DTBO 17 was parsed again with a standalone `libfdt` harness. The active UFS controller reports two `reg` windows (`0x1d84000/0x3000`, `0x1d90000/0x8000`) and SPI `265`; the PHY reports `0x1d87000/0xe00` and `0x1d90000/0x8000`. The controller clock tuples are exactly `<0x4f,0x67>`, `<0x4f,0x81>`, `<0x4f,0x66>`, `<0x4f,0x70>`, `<0x4f,0x69>`, `<0x4f,0x8a>`, `<0x56,0>`, `<0x4f,0x6f>`, `<0x4f,0x6d>`, `<0x4f,0x6e>`; PHY clocks are `<0x56,0>`, `<0x4f,0x65>`, `<0x4f,0x6b>`; reset is `<0x4f,0x0c>`. The 10/3 clock-name and reset-name FNV-1a checks match the Lito source definition, so `resource_contract_valid=true`, `platform_ready=true`, and `missing=0` were observed.
- `ufs.rs` now retains provider phandles and IDs rather than treating clock IDs as global. `lito_clock_gate()` rejects non-GCC providers before mapping the Lito branch offsets; the hardware-control UFS ICE clock retains its separate enable bit (`0x77064`, bit 1). The source-derived mappings cover GCC IDs `0x67`, `0x81`, `0x66`, `0x70`, `0x69`, `0x8a`, `0x6f`, `0x6d`, `0x6e`, `0x65`, and `0x6b`, with the RPMh CXO tuple kept separate. No register was read or written.
- A data-only 15-step sequence is now exposed after the exact contract passes: controller clocks, RPMh QPHY resource, PHY rails, analog power, PHY interface clocks, reference clocks, controller-PHY reset assert, rate-A calibration, second-lane calibration, reset deassert, SerDes start, PCS-ready wait, UniPro mode, lane clocks, and controller enable. This follows the primary Qualcomm Linux split in [`ufs-qcom.c`](https://linux.googlesource.com/linux/kernel/git/torvalds/linux/+/2b414a95b8f7307d42173ba9e580d6d3e2bcbfce/drivers/ufs/host/ufs-qcom.c) and the Lito QMP PHY source; the runtime stage is still only `Described`, with no MMIO/RPMh executor yet.
- Trial record: the first ephemeral Rust DTB harness used `include!` and failed with E0753 because module-level inner documentation comments were inserted into the wrong context. The harness was corrected without changing repository code and then returned the exact tuples, hashes, `platform_ready=true`, and 15 sequence steps. `cargo check` for AArch64, host Flasks, 11 ABI tests, rustfmt, and `git diff --check` all passed; final logs are `/tmp/fullerene-aarch64-ufs-platform-contract-final.log`, `/tmp/fullerene-host-ufs-platform-contract-final.log`, `/tmp/fullerene-abi-ufs-platform-contract-final.log`, `/tmp/fullerene-fmt-ufs-platform-contract-final.log`, and `/tmp/fullerene-diff-ufs-platform-contract-final.log`.
- Generic launchd QEMU `/tmp/fullerene-generic-aarch64-ufs-platform-contract.log` retained `ufs: no active qcom,ufshc platform contract` and reached the inventory/window/time/VFS/EL1 smoke markers. Its status `1` is only the intentional timeout. The rebuilt Bramble image `/tmp/fullerene-bramble-aarch64-ufs-platform-contract.img` passed the Android v3 boot audit; size `2,351,104` bytes, SHA-256 `8cc68af2889991ae3ddf88731fb2f0ec253f6a92f594da511f6377632722a48a`, build log `/tmp/fullerene-bramble-aarch64-ufs-platform-contract-build.log`.
- The image was not booted because the phone was not re-entered into Fastboot during this trial. No flash, erase, partition write, analyzer, or user-data operation was performed. `tools/` remains outside Git via `.gitignore: /tools/`. The next boundary is a separately testable, DT-gated Qualcomm RPMh/GCC/PHY executor, followed by UFSHCI link startup; it must not be enabled on generic QEMU or from a bare `reg` match.

### 2026-09-07 — Qualcomm UFS guarded executor and Lito QMP calibration data

- Added a narrow `PlatformOps` interface and `execute_platform()` to `fullerene-kernel/src/arch/aarch64/ufs.rs`. It refuses any profile that fails the exact merged Bramble/Lito contract, skips the RPMh CXO tuple during GCC-only branch mapping, and then drives the Linux order through provider-qualified clocks, `qphy.lvl`, rails/analog power, PHY interface/reference clocks, controller PHY reset, calibration, SerDes, PCS-ready polling, UniPro selection, lane clocks, and HCE. A backend must implement the side effects; the generic build has no backend call and therefore performs no Qualcomm MMIO.
- Imported the Android primary Lito QMP calibration data as 8-bit register/value pairs: Rate-A/no-G4 `84` entries, second lane/no-G4 `46` entries, and Rate-B override `1` entry. The executor also performs the PHY soft-reset pulse around those tables and the two 1 ms source-specified settling delays. The controller stage can advance to `ControllerReady` only after the backend reports PCS ready and HCE success; link startup, DMA, interrupt ownership, query commands, and block I/O remain subsequent stages.
- Source basis: [Lito QMP header](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/phy/qualcomm/phy-qcom-ufs-qmp-v4-lito.h), [Lito QMP implementation](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/phy/qualcomm/phy-qcom-ufs-qmp-v4-lito.c), [generic Qualcomm UFS PHY](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/phy/qualcomm/phy-qcom-ufs.c), and [Qualcomm UFS host](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/scsi/ufs/ufs-qcom.c). The source sequence is now data- and order-aligned, but no claim of physical PHY success is made.
- Verification chronology: the first ephemeral executor wrapper had an extra closing delimiter; the next wrapper omitted `use super::fdt`; both were corrected only in the stdin harness. A later mock assertion incorrectly assumed lane-clock events were the final two events; correcting that expectation produced `/tmp/fullerene-ufs-executor-harness` status `0`. The host `cargo test` attempt for the no-std AArch64 binary is not a valid unit-test route: it failed before tests with duplicate `panic_impl`/`alloc_error_handler` and existing `fdt` test `Vec` imports; repository AArch64 `cargo check` remained status `0`.
- Final checks passed: AArch64 `/tmp/fullerene-aarch64-ufs-executor-final.log`, host Flasks `/tmp/fullerene-host-ufs-executor-final.log`, ABI tests `/tmp/fullerene-abi-ufs-executor-final.log`, rustfmt `/tmp/fullerene-fmt-ufs-executor-final.log`, and `git diff --check` `/tmp/fullerene-diff-ufs-executor-final.log`. Generic QEMU `/tmp/fullerene-generic-aarch64-ufs-executor-final.log` retained `ufs: no active qcom,ufshc platform contract` and all inventory/window/time/VFS/EL1 smoke markers; status `1` is the intentional timeout. Bramble `/tmp/fullerene-bramble-aarch64-ufs-executor-final.img` passed the Android v3 audit, size `2,351,104` bytes, SHA-256 `8cc68af2889991ae3ddf88731fb2f0ec253f6a92f594da511f6377632722a48a`, build log SHA-256 `9b22c24062aa6eb0f18320e2a3746c2a08eb3f431559fdec81ae0de6016fc375`.
- Read-only physical recheck `/tmp/fullerene-ufs-executor-physical-recheck.log` at `2026-09-07T02:51:10+09:00`: Fastboot was empty, ADB serial `26191JECB00076` was `no permissions`, and `lsusb` still showed `1234:0001` / Pixel 4a (5G). No physical boot, Fastboot command, flash, erase, partition write, analyzer, secure-debug capture, or user-data operation was performed. The next implementation boundary is the real Bramble RPMh/GCC/PHY backend plus UFSHCI link startup, not another QEMU-only identity row.

### 2026-09-07 — DT-gated Bramble UFS backend and opt-in image verification

- **Contract proof:** a standalone Rust parser over `/tmp/bramble-merged-dtbo17.dtb` returned `bramble UFS DT contract ok` in `/tmp/fullerene-ufs-backend-dtb-contract.log`: controller/PHY region counts `2/2`, GCC phandle `0x4f` region `0x100000/0x1f0000`, SPI `265`, QMP RPMh resource property length `9`, and FNV-1a `0xea4c294d`. The parser passed `resource_contract_valid()` and `platform_ready()`; this confirms the exact same-build factory DTB evidence before any backend write is considered.
- **Backend implementation:** `fullerene-kernel/src/arch/aarch64/ufs.rs` now has `BramblePlatformOps` behind `fullerene_aarch64_bramble`. It uses the provider-qualified Lito GCC mappings and source rates (UFS AXI 200 MHz, UniPro 150 MHz, ICE 300 MHz, PHY auxiliary CXO), programs branch gates with readback, enables the existing RPMh active-TCS path for `qphy.lvl`, and votes `xo.lvl=0x3` once for the reference clock. The QMP analog sequence clears the RX interface clock-edge bit and asserts power-down; calibration, reset, SerDes, PCS polling, UniPro selection, and HCE are all readback-checked in Linux-derived order.
- **Power safety boundary:** the active merged UFS PHY node has no `vdda-phy-supply` or `vdda-pll-supply`, but the controller does describe `vdd-hba-supply`, `vcc-supply`, and `vccq2-supply` (the parser prints this in `/tmp/fullerene-ufs-backend-dtb-contract.log`). No PMIC regulator provider mapping is implemented yet. The backend now refuses the whole transaction before any GCC/RPMh/PHY write whenever any controller or PHY supply property is present; it does not invent regulator MMIO. This is a deliberate stop condition, not a claim that the physical PHY has been proven powered.
- **Opt-in only:** the generic image never calls the backend. The call is compiled for Bramble but requires the explicit build environment `FULLERENE_AARCH64_UFS_EXECUTE=1`; `FULLERENE_AARCH64_UFS_RATE_B=1` additionally applies the source's Rate-B override. No QEMU path can execute this Qualcomm backend.
- **Verification:** AArch64 check, host Flasks check, ABI tests, rustfmt, and `git diff --check` all returned status `0` in `/tmp/fullerene-aarch64-ufs-backend-check.log`, `/tmp/fullerene-host-ufs-backend-check.log`, `/tmp/fullerene-abi-ufs-backend-check.log`, `/tmp/fullerene-fmt-ufs-backend-check.log`, and `/tmp/fullerene-diff-ufs-backend-check.log`. Generic QEMU `/tmp/fullerene-generic-aarch64-ufs-backend.log` reached the inventory/window/time/VFS/EL1 markers; status `124` is only the outer 15-second timeout after the smoke. No QEMU UFS backend was invoked.
- **Images:** the default Bramble image `/tmp/fullerene-bramble-aarch64-ufs-backend.img` passed the Android v3 boot audit, size `2,351,104` bytes, SHA-256 `0d95fb038f18cf45bdab0b1e6f5c97b904bea5aac14073e1da62916c8fb88f0f`; its build log SHA-256 is `a3e0d4e2123b6d28bbd4ad1a489c7f1f5041a0305938b6923495d6f9180e568b`. The compile-time opt-in image `/tmp/fullerene-bramble-aarch64-ufs-backend-optin.img` also passed the audit, SHA-256 `7fb4902e16269536b91a059013f637386001b82d969cc5c1ed6c50083f7e88c6`; it was not booted or sent to the phone.
- **Physical result:** read-only recheck `/tmp/fullerene-ufs-backend-physical-recheck.log` at `2026-09-07T03:03:51+09:00` found an empty `fastboot devices`, ADB serial `26191JECB00076` with `no permissions`, and USB `1234:0001` / `Brain Actuated Technologies Pixel 4a (5G)`. No fastboot command, boot, flash, erase, partition write, analyzer, secure-debug capture, or user-data operation was performed. `tools/` remains Git-ignored.
- **Remaining gap:** this is the first real Bramble MMIO/RPMh backend boundary, not successful physical UFS enumeration. The next work is UFSHCI link startup, interrupt/DMA ownership, query descriptors, and block translation, followed by a separately logged physical boot only after the device is intentionally in Fastboot and the USB permissions are fixed. Source basis remains the [Qualcomm UFS host](https://linux.googlesource.com/linux/kernel/git/torvalds/linux/+/2b414a95b8f7307d42173ba9e580d6d3e2bcbfce/drivers/ufs/host/ufs-qcom.c), [Android generic Qualcomm PHY](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/phy/qualcomm/phy-qcom-ufs.c), [Lito QMP PHY](https://android.googlesource.com/kernel/msm.git/+/8787e484d7b365cc4fd6d113f61b91a80e221543/drivers/phy/qualcomm/phy-qcom-ufs-qmp-v4-lito.c), and [Lito GCC](https://android.googlesource.com/kernel/msm/+/62de338c632ca3768f8a1eaa6e916ea023293421/drivers/clk/qcom/gcc-lito.c).

### 2026-09-07 — UFSHCI DME link-startup boundary

- **Implementation:** `fullerene-kernel/src/arch/aarch64/ufs.rs` now separates `execute_link_startup()` from the PHY/controller power transaction. After HCE it waits for `UIC_COMMAND_READY`, enables only the UIC error/link-loss/completion interrupt bits, writes zero UIC arguments plus `DME_LINK_STARTUP=0x16`, polls `UIC_COMMAND_COMPL`, reads the low-byte UIC result, clears only the UIC W1C status bits, and requires `DEVICE_PRESENT` before advancing the stage to `LinkReady`. Timeout, nonzero UIC result, controller-not-ready, and device-absent are distinct errors. Early boot polls because the GIC/IRQ owner is not installed at this point; UTRD/DMA/interrupt delivery for data requests remains separate.
- **Primary-source basis:** the command opcode/result mask, HCS readiness/device-present bits, UIC interrupt bits, and Linux ordering come from [`include/ufs/ufshci.h`](https://github.com/torvalds/linux/blob/master/include/ufs/ufshci.h) and [`ufshcd.c` UIC/link-startup code](https://raw.githubusercontent.com/torvalds/linux/master/drivers/ufs/core/ufshcd.c). Linux also retries link startup with controller re-enable and may issue it twice depending on device-active state; this first Fullerene boundary deliberately does not claim those later recovery/power-mode policies.
- **Mock result:** `/tmp/verify_bramble_ufs_link` passed with the exact merged DTB profile, a fake controller completion, one command write `0x16`, UIC completion enabled, and `LinkReady`. This is a software-order test only; it does not emulate a PHY or UFS device.
- **Build result:** AArch64 check, host Flasks check, ABI tests (`11 passed`), rustfmt, and `git diff --check` passed in `/tmp/fullerene-aarch64-ufs-link-check.log`, `/tmp/fullerene-host-ufs-link-check.log`, `/tmp/fullerene-abi-ufs-link-check.log`, `/tmp/fullerene-fmt-ufs-link-check.log`, and `/tmp/fullerene-diff-ufs-link-check.log`. An initial ABI command used the nonexistent package selector `fullerene-kernel-abi` and returned `101`; it was corrected to the workspace package `fullerene-abi`, which passed. The harness compiler emitted only unused-symbol warnings from embedding the full FDT/UFS modules.
- **QEMU result:** `/tmp/fullerene-generic-aarch64-ufs-link.log` returned status `124` only from the intentional 15-second wrapper timeout, printed `ufs: no active qcom,ufshc platform contract`, and reached the inventory/window/time/VFS/EL1 markers. No link-startup code was reachable in generic QEMU.
- **Bramble artifacts:** default `/tmp/fullerene-bramble-aarch64-ufs-link.img` passed the Android v3 audit and remained byte-identical to the guarded-backend default image, SHA-256 `0d95fb038f18cf45bdab0b1e6f5c97b904bea5aac14073e1da62916c8fb88f0f`. Opt-in `/tmp/fullerene-bramble-aarch64-ufs-link-optin.img` passed the same audit, SHA-256 `47d9245f29cb73637979fd98cf524c9157dbbe5bfe1c291356302cf6861b8eb1`; neither image was booted or sent to Fastboot.
- **Physical result:** no physical command or device mutation was attempted for this link-startup trial. The latest read-only USB record remains `/tmp/fullerene-ufs-backend-physical-recheck.log`: Fastboot empty, ADB `no permissions`, and host-only USB `1234:0001`. The next boundary is descriptor/DMA ownership and a minimal NOP/query path after link startup, not a claim of block storage readiness.

### 2026-09-07 — UFS regulator-ownership correction and pre-MMIO stop

- **Correction from the merged DTB:** the first backend note only highlighted that the PHY node lacks `vdda-phy-supply`/`vdda-pll-supply`. A direct parser recheck showed the UFS controller has `vdd-hba-supply`, `vcc-supply`, and `vccq2-supply`, while `iommus` and `power-domains` are absent; the complete output is `/tmp/fullerene-ufs-regulator-contract.log`. The historical note has been corrected so this DT observation is not mistaken for a regulator-free UFS host.
- **Safety fix:** `BramblePlatformOps::new()` now rejects any controller or PHY supply property with `PlatformError::UnmappedRegulators` before constructing an executable backend. Thus the explicit UFS opt-in cannot send even GCC clock or RPMh votes until the referenced PMIC regulator provider is decoded and implemented. The generic sequence/mock interfaces remain available for order tests; they do not bypass the physical guard.
- **Verification after correction:** AArch64, host Flasks, 11 ABI tests, rustfmt, and `git diff --check` passed in `/tmp/fullerene-aarch64-ufs-regulator-guard-check.log`, `/tmp/fullerene-host-ufs-regulator-guard-check.log`, `/tmp/fullerene-abi-ufs-regulator-guard-check.log`, `/tmp/fullerene-fmt-ufs-regulator-guard-check.log`, and `/tmp/fullerene-diff-ufs-regulator-guard-check.log`. Default `/tmp/fullerene-bramble-aarch64-ufs-regulator-guard.img` passed the Android v3 audit with the same SHA-256 `0d95fb038f18cf45bdab0b1e6f5c97b904bea5aac14073e1da62916c8fb88f0f`; opt-in `/tmp/fullerene-bramble-aarch64-ufs-regulator-guard-optin.img` passed with SHA-256 `5315337aee61a99587e1d1258efc323b7b354d490e08a7ca7c09542a182d462a`. Neither image was booted.
- **QEMU and physical result:** `/tmp/fullerene-generic-aarch64-ufs-regulator-guard.log` status `124` is only the intentional timeout; it retained the no-UFS-contract message and all existing smoke markers. No physical trial, Fastboot command, analyzer, or user-data operation was performed. The next real boundary is mapping the three DT-described PMIC rails and the UFS controller's DMA/IOMMU ownership from primary DT/source evidence before link startup can be physically attempted.
