# Context-efficient project status

Read this file first when investigating the current hardware goal. It is a
compact index of the evidence; the linked hardware ledgers retain the full
commands, timestamps, hashes, and per-run notes.

## Current goal

| Goal | Success criterion | Current state |
| --- | --- | --- |
| Pixel 4a 5G (Bramble) USB handoff | Fullerene enumerates as `idVendor=1234` | Not reached |
| Recovery safety | Failed handoff returns to Android without flashing or erasing | Confirmed with `fastboot boot` runs |

## What is actually known

| Boundary | Evidence | Interpretation |
| --- | --- | --- |
| Fastboot handoff | The device leaves Fastboot and Fullerene reaches USB High-Speed attach | Pull-up/attach works; this is not enumeration success |
| First control request | Host submits `GET_DESCRIPTOR(Device)` | The host sees a Fullerene USB device address path |
| Response | usbmon records no returned bytes (`len=0`, `cap=0`) | No descriptor payload exists to wrap or re-encode |
| Failure | A valid capture shows `-2`, followed by zero-length `-71` retries | The error is before a usable EP0 response; Linux `-EPROTO` is a broad host error class |
| Software latches | Current setup/event/SOF diagnostic latches have not produced a usable readout | DWC3 event production/consumption and USB2 RX/UTMI remain unresolved |
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
| ABL request/TRB flags | ABL's `0x405` request control base (`HWO|CHN|ISP_IMI`) also produced `-2`→`-71` with `len=0`/`cap=0` | The `LST|IOC` versus ABL flag difference alone is not sufficient; resolve request-object/ring ownership before another flag permutation |
| ABL event ownership order | ABL-style per-event dispatch followed by four-byte `GEVNTCOUNT` ACK with EHB preserved also produced `-2`→`-71` with `len=0`/`cap=0` | Event ACK timing/order alone is not sufficient; compare the physical request object and EP0 TRB/ring ownership state |
| EP0 setup DMA/readback semantics | Android msm `ep0.c` targets `CONTROL_SETUP` at `dwc->ep0_trb_addr` and reads the received request from `dwc->ep0_trb`; Fullerene now uses the same EP0 TRB for DMA and parsing | The coherent source-aligned run `2201961.0` still returned no payload, so the remaining suspect is request/ring ownership, DWC3 event delivery, or lower USB2/UTMI reception |
| MMIO write ordering barrier | `write()` in `mmio.rs` lacked the DSB store-barrier that Linux's `writel()` provides via `__iowmb()` | Fix applied: `dsb st` after every `write_volatile` in `write()`. Real-hardware verification (runs 2263604.0, 2266529.0, 2278047.0, 2284470.0, 2287872.0) showed NO behavioral change: HS attach still succeeds, descriptor read still times out with -110. The DSB fix is correct (matches Linux) but was not the root cause. The DALEPENA=0 readback may be a timing-channel artifact rather than a real register state. |
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
| Explicit QMP lane override | Source audit added `--qmp-lane a|b`; runs `2679376.0`/`2685319.0` tested lanes B/A with `--skip-typec-spmi`, and `2688865.0` repeated explicit lane A with Type-C observation enabled | Negative/inconclusive explicit-lane and observer A/B. Lane B produced xHCI address `-62`; lane A without observation reproduced HS attach only; lane A with observation returned to the ordinary HS/`-110` path. Neither produced an SS identity or `1234:0001`. The override changes only QMP `TYPEC_CTRL` (`0x02`/`0x03`) and leaves PMIC Type-C role/VBUS writes unchanged |
| Same-boot QMP phase readout | Source correction added `--qmp-phase-stop 1..8` and made stage 13 participate in the automatic recovery gate | Implemented; cargo checks, CLI help, and `git diff --check` pass; phases 1–8 physical runs complete | Each selected QMP marker (`QMPB` through `QMOK`) now stops before the next risky QMP access and falls back to the known USB2 pull-up. Runs `2706606.0`, `2710010.0`, `2712894.0`, `2715864.0`, `2718831.0`, `2722509.0`, `2725423.0`, and corrected `2739860.0` produced the expected same-boot HS attach for phases 1–8, proving the QMP entry/control preamble/table/table-complete/PCS-start/status-read/poll-entry/PHY-ready prefix survives; the fallback does not make partial QMP state or USB2 RX valid |
| Post-QMP SS link boundary | Stage 13 and corrected stage 14 now stop after QMP completion, with stage 14 extending through DWC3 global/PHY setup and minimal SS Run/Stop | Negative; `2751078.0`/`2755382.0` are stage-13 controls; `2760948.0`/`2763971.0` predate the stage-14 SUSPHY-clear correction; corrected `2787804.0` still produced no `1234:0001` | The corrected stage `>=13` path clears USB3 `GUSB3PIPECTL.SUSPHY` before SS Run/Stop, but the host still sees no SS identity and falls back to Android. The `ss-speed` readout timing was not independently decodable from saved host artifacts, so speed/link/PIPE state remains unclassified. SMMU, endpoint, event-ring, and EP0 are still downstream |
| Full SS post-stage-14 tail | `2817124.0` crashed on the unrestricted no-core path; stages 15–20 crossed the pre-Run/Stop tail; stages 21–24 crossed `init_with_super_speed()`; stages 25–29 crossed GIC setup and repeated post-init loop probes; `2902315.0` and `2921674.0` still fell back on the unrestricted path; `2935868.0` tested the opt-in PIPE reset release at stage 13; `2966590.0` was an invalid stage-21 `ss-link` attempt; `2983039.0` produced `ss-link=0`; `2988810.0` produced `ss-speed=0` | Negative/inconclusive for enumeration; stages 27–29 produced the intentional USB2 marker and `-110`, not `1234:0001`; `2935868.0` produced no SS identity; `2983039.0` produced no SS identity but decoded `DSTS.USBLNKST=0` (`U0`); `2988810.0` produced no SS identity and decoded `DSTS.CONNECTSPD=0` (unspecified) | Stage 29 rules out the repeated progress/trace bookkeeping calls as the immediate reset point. Adding the Android `dwc3_set_mode(DEVICE)` GCTL tail to SS made no difference, and clearing `GUSB3PIPECTL.PHYSOFTRST` after QMP-ready made no difference at stage 13. The diagnostic prologue was found to perturb the SS stage path and is now skipped for `ss-*` readouts. The combined valid readouts show no negotiated SS speed at the post-Run/Stop boundary; inspect PIPE reset/suspend/run state next. Packet wrapping remains downstream |

## Next source-directed investigation

| Order | Check | Why |
| --- | --- | --- |
| 1 | Read stage-21 `ss-pipe` | `2983039.0` decoded `ss-link=0` (`DSTS.USBLNKST=U0`) and `2988810.0` decoded `ss-speed=0` (`DSTS.CONNECTSPD` unspecified) while no SS device appeared on the host. Read PIPE state before changing SUSPHY or endpoint/packet code |
| 2 | Reconcile final USB3 `SUSPHY` and PIPE-clock policy with Android gadget start | Official msm source keeps USB3 `SUSPHY=1` in the initial `dwc3_phy_setup()` state and does not clear it in `__dwc3_gadget_start()` or `dwc3_gadget_run_stop()`; do not make a blanket `SUSPHY=0` change before the snapshot is decoded |
| 3 | Reconcile QMP-ready with SS PIPE/link training | Stage 14, stage 29, and the PIPE-reset-release stage-13 control return markers but no host-visible SS identity. Compare the post-Run/Stop PCS state and DWC3 connect-speed/link-state fields to distinguish PHY training failure from DWC3 device-mode/Run/Stop failure |
| 4 | Only if SS link is present, inspect DWC3/SMMU/endpoint ownership | Then move to `DCFG`, `DEPSTARTCFG`, endpoint resources, event ring, and EP0. Until that point, endpoint and packet-format changes are downstream |
| 5 | Only after a valid EP0 data stage, inspect descriptor bytes | Wrapping is downstream of the observed no-identity/no-payload boundary |

Latest classified hardware run: `2988810.0` ran stage 21 with the corrected `ss-speed` same-boot readout, QMP lane A, `--no-core-reset`, and `--no-smmu`. Fastboot `18d1:4ee0` disconnected at `18:09:41`; one intentional HS marker appeared at `18:09:51` and its descriptor read timed out with `-110` at `18:09:57`; no SS device or `1234:0001` appeared; Android SuperSpeed returned at `18:10:18`; `boot-reason.txt` is `watchdog`. The single marker decodes `DSTS.CONNECTSPD=0` (unspecified); the preceding `2983039.0` read `DSTS.USBLNKST=0` (`U0`). Attempt `2966590.0` remains invalid. The next control is stage-21 `ss-pipe`; packet wrapping remains downstream.

## Document routing and context cost

| Document | Size | Use | Loading policy |
| --- | ---: | --- | --- |
| [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md) | 700 lines / 302 KB | Full Bramble ledger and source audit | Read targeted sections or this index first |
| [`HARDWARE.md`](HARDWARE.md) | 607 lines / 176 KB | Cross-platform hardware notes plus Bramble summary | Read the Bramble section and this index; avoid loading the full table |
| [`BUG_JOURNAL.md`](BUG_JOURNAL.md) | 1,410 lines / 66 KB | Historical software investigations, mostly Wi-Fi and runtime | Not needed for the Bramble USB path unless a related regression appears |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 1,037 lines / 38 KB | Project-wide design rules | Read only when changing architecture or ownership boundaries |
| [`BUILD.md`](BUILD.md) | 679 lines / 29 KB | Build and run procedures | Read the Bramble command section when running hardware |
| `docs/history/*.png` | 3.1 MB | Historical screenshots/artifacts | Do not load for USB source debugging |

The two hardware ledgers account for about 478 KB of text and contain the
only unusually long lines: 479 rows in `HARDWARE_aarch64.md` exceed 200
characters, and the longest row is about 1,409 characters. The individual
experiment rows are valuable evidence, but loading the whole ledger into an
agent context repeats the same negative conclusion many times. This index is
the compact working memory; the ledgers remain the evidence archive.

## Source of truth

- Full per-run evidence: [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md)
- Cross-platform status: [`HARDWARE.md`](HARDWARE.md)
- Build commands: [`BUILD.md`](BUILD.md)
- Historical non-USB bug records: [`BUG_JOURNAL.md`](BUG_JOURNAL.md)
