# AArch64 Hardware Notes

This file contains the AArch64 notes split from `docs/HARDWARE.md`, especially
the Google Pixel 4a 5G (Bramble) USB handoff record. The history is organized
as tables, and every table retains a Notes column. Physical-test results are
recorded here before the next test.

## Target and safety boundary

| Item | Value / result | Status | Notes |
| --- | --- | --- | --- |
| Device | Google Pixel 4a 5G / Bramble / Qualcomm SM7250 (Lito) | Confirmed | `product: bramble` |
| Serial | `26191JECB00076` | Confirmed | Fixed target for physical tests |
| Bootloader | `b5-0.6-10489838` | Confirmed | Production secure boot; unlocked |
| Test operation | `fastboot boot` only | Ongoing | No `flash`, `erase`, or partition writes |
| Success condition | Host journal contains `New USB device found, idVendor=1234` | Not reached | The implementation uses PID `0001` |
| Current failure | `device descriptor read/64, error -110` | Reproduced | After attach, before the first EP0 data response |

## Public device tree and implementation

The primary source is the [Bramble Lito USB device tree](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/heads/android-msm-bramble-4.19-android11-qpr1/qcom/lito-usb.dtsi) and the related Lito nodes in the same repository.

| Device-tree item | Public value | Fullerene side | Status | Notes |
| --- | --- | --- | --- | --- |
| DWC3 wrapper | `0x0a600000`, size `0x100000` | `0x0a600000`, child size `0xcd00` | Match | The `snps,dwc3` child uses the same core |
| Apps-SMMU stream | `0xe0` | `stream_id: 0xe0` | Match | `iommus = <&apps_smmu 0xE0 0x0>` |
| IOMMU DMA pool | Base `0x90000000`, size `0x60000000` | Same values | Match | DMA range is `0x90000000..0xF0000000` |
| Controller clocks | Core / iface / bus / UTMI / sleep / XO | Six resources | Match | Core rate `133333333`; HS rate `66666667` |
| Core reset | `GCC_USB30_PRIM_BCR` | Resolved from DT/provider ID | Match | Executed by the platform layer |
| USB2 PHY | `0x088e3000`; `GCC_QUSB2PHY_PRIM_BCR` | Same values | Match | HS PHY reset and parameter override are reapplied |
| QMP PHY | `0x088e8000`; two reset lines | Same values | Match | Not used by the USB2 handoff branch |
| GSI | Three buffers; offsets `0x0fc,0x110,0x120,0x130,0x144,0x1a4` | Same values | Match | `qcom,gsi-disable-io-coherency` is also applied |
| IRQs | PDC `14/9/15`; GIC `144/240` | Same values | Match | DP/DM HS are rising-edge; SS/power/DWC3 are level-high |
| DWC3 global | `snps,disable-clk-gating`; HIRD `0x10` | `DSBLCLKGTNG`; HIRD `0x10` | Match | Uses the 8-bit UTMI baseline |
| HS PHY override | Stock Bramble DTB: `0x63@0x6c`, `0x85@0x70`, `0x17@0x74` | Same fallback; DT takes priority | Confirmed | A separate QRD overlay uses `0xc8@0x70`; it is not present in this Bramble factory DTB |

## ABL, XBL, and USB function-driver audit

| Subject | Observation | Status | Notes |
| --- | --- | --- | --- |
| ABL | The extracted ABL ELF is ARM32 and contains no strings identifying the USB implementation | Not the main USB implementation | Treat ABL as the entry point for the next-stage boot and handoff |
| XBL | Bramble XBL is AArch64 and contains USB strings and DWC3 initialization | Primary comparison target | Proprietary function path equivalent to `UsbfnDwc3Dxe` |
| XBL event ring | `GEVNTADRLO0=0x0a6fc010`, size configuration, `DEPSTARTCFG`, and EP0 resource setup observed | Source-derived | An A/B using a fixed event-ring address has been run |
| XBL endpoint setup | EP0 OUT/IN are configured with direction-specific contexts, followed by `SETTRANSFRESOURCE` and `STARTTRANSFER` | Source-derived | TRB chain, HWO/CHN/IOC/ISP_IMI were compared |
| Difference from XBL | Parameter-offset meaning, EP0 slot, `DALEPENA`, `DEVTEN`, and post-global ordering compared | Broadly aligned | Individual factors remain under A/B isolation |

## Verification and diagnostic ledger

| Date / stage | Change or observation | Result / status | Notes |
| --- | --- | --- | --- |
| Initial | USB2 pull-up and HS attach are visible, but the first descriptor read returns `-110` | Boundary is after physical attach and before EP0 response | QEMU DWC3/EP0 model passes; PHY, Type-C, PDC, and SMMU are not modeled |
| Initial | Android v3 image / LZ4 / boot-template audit added | Build-side pass | Stock ramdisk and tail are preserved |
| Initial | Lito DT DWC3, PHY, GDSC, clock, reset, SMMU, GSI, and PDC implemented | Platform contract aligned | Does not by itself explain `-110` |
| 2026-08-30 | USB30 GDSC/clock/reset recovery added at probe entry | Partly moved past core collapse after attach | `-71` and `-110` varied between runs |
| 2026-08-30 | EP0 OUT SETUP `STARTTRANSFER` and direction-specific EP0 IN data/status audited | OUT reaches U0/SETUP; IN is the main failure point | The 16-bit PHYIF `-71` is outside the DT baseline |
| 2026-08-30 | EP1 raw status / No Resource / BUS_EXPIRY / other gates tested | Readout does not establish host enumeration | Stopped adding status gates as new actuators |
| 2026-08-30 | Host USB log collected with timestamps | First request is `GET_DESCRIPTOR` with `wLength=64` | It is not an 8-byte first read |
| 2026-08-31 | `start-after-reset`, arm blip, ungated arm, and TX FIFO A/B tests | `-110` continues | No attach blip or `1234` |
| 2026-08-31 | PON / APSS-WDT / collapse timing measured | About 5.6 seconds to death, including host port-reset effects | `boot-reason.txt` often reports watchdog |
| 2026-09-01 | Bramble branch of `msm-extra/devicetree` checked | DT values fixed as source of truth | IRQ edge, DMA pool, and PHY override confirmed |
| 2026-09-01 | RAMCLKSEL capture-validity fix implemented | Build / QEMU preflight pass | `RAMCLKSEL=0` is no longer mistaken for an uncaptured value |
| 2026-09-01 | RAMCLKSEL A/B run on the device | `-110` unchanged; `1234` not reached | Next boundary is the endpoint/PHY data path |
| 2026-09-01 | Factory `vendor_boot` DTB extracted and parsed | HS PHY override is `0x63@0x6c`, `0x85@0x70`, `0x17@0x74`; matches the compiled fallback | No code change from the `0xc8` hypothesis; `vendor_boot` SHA `8af68ba6199cb6947fdb1e1f49cb2052537d284ef38e1ea1beb3c4a2ca7bc135`; DTB SHA `26dd8f5df8e1e5db7cb5167368ef573304fd7a8e20aee16f54dffaf24ff71a3a` |
| 2026-09-01 | Linux msm 4.19 DWC3 event-status definitions checked | `CONTROL_DATA=1`, `CONTROL_STATUS=2`; the hardware handler's phase split is correct | No code change; the local QEMU generic NRDY constant is not a phase-specific hardware definition |
| 2026-09-01 | Android msm 4.19 `ep0.c` control-data flow checked | DATA phase is started immediately when the gadget queues the response; `XferNotReady(DATA)` is not the normal arm point | Fullerene now follows the immediate DATA arm and retains the NRDY path only as a failed-command retry |
| 2026-09-01 | Next A/B: XBL-style EP0 IN data TRB (`TRB_NORMAL`) | Pending | Keep the standard handoff and change only the EP0 IN data-phase TRB shape; success still requires host `idVendor=1234` |
| 2026-09-01 | Next A/B: XBL deferred initial SETUP request | Pending | Move the initial EP0 SETUP arm into the XBL-style queued/request path; this is mutually exclusive with the EP0 IN-data TRB A/B |
| 2026-09-01 | Next A/B: XBL observed EP0 event-ring DMA address | Pending | Keep Fullerene's setup/TRB objects and switch only `GEVNTADR0` to `0x0a6fc010`; success still requires host `idVendor=1234` |
| 2026-09-01 | Next A/B: XBL EP0 notification mask (`P1=0x300`) | Pending | Change only EP0 `SETEPCONFIG` notifications from Linux-style NRDY to XBL-style transfer-complete + transfer-in-progress |
| 2026-09-01 | Next A/B: XBL between-direction EP0 request ordering | Pending | Arm the OUT SETUP request between the EP0 OUT and IN configuration/resource pairs; keep the remaining standard path unchanged |
| 2026-09-01 | Next A/B: XBL direction-specific EP0 TRB slots | Pending | Use TRB slot 0 for EP0 OUT and slot 1 for EP0 IN while retaining the standard control-data TRB type |
| 2026-09-01 | Next A/B: XBL fixed initial EP0 DMA objects | Pending | Publish the initial SETUP TRB at XBL's observed fixed DDR address while retaining the standard response buffer and event ring |
| 2026-09-01 | Next A/B: XBL raw Run/Stop write | Pending | Change only the final DCTL transition to preserve XBL HIRD/APPL1RES and modify `RUN_STOP` directly |
| 2026-09-01 | Next A/B: XBL post-endpoint global register ordering | Pending | Apply the usb31 global deltas after EP0 `SETEPCONFIG`/resource publication, matching XBL's ordering |
| 2026-09-01 | Next A/B: XBL chained EP0 TRBs | Pending | Add only `TRB_CHN` to the EP0 setup/data/status control words; retain the standard endpoint and DMA setup |
| 2026-09-01 | Android msm immediate EP0 DATA `STARTTRANSFER` A/B | Negative on hardware | Applied at `handle_setup()` after the response TRB was prepared; the first descriptor still returned `-110` |
| 2026-09-01 | Historical run-profile audit | Earlier attach-reaching rows reclassified | Build-script outputs show that `150741.0` through `223537.0` used a direct-handoff profile with additional cfg/env controls; the physical ledger now identifies that profile explicitly |
| 2026-09-01 | EP0 early-drop observation window corrected | Pending hardware readout | The previous 1.5-second window ended before Bramble's ~14-second Fastboot-to-HS attach delay; the window is now 20 seconds |
| 2026-09-01 | EP0 SETUP DMA diagnostic latch corrected | Pending hardware readout | `handle_setup()` clears the SETUP buffer immediately; the signal-probe latch is now set at the processing boundary so a real packet cannot be missed by polling |
| 2026-09-01 | Corrected SETUP DMA observation A/B | No early-drop disconnect; HS attach still reached `-110` | The processing-boundary latch did not observe code 3; event-ring delivery must be tested separately before concluding that SETUP DMA is absent |
| 2026-09-01 | Host-visible EP0 SETUP TRB retire probe (`--signal-early-drop 2`) | Negative / no retire evidence | No early disconnect; the probe did not observe EP0 TRB HWO clearing before the normal timeout |
| 2026-09-01 | Host-visible event-ring delivery probe (`--signal-early-drop 1`) | Negative / no delivery evidence | No early disconnect; the run still reached HS attach, then `-110`, so the probe did not observe a delivered event record |
| 2026-09-01 | USB2-path SMMU-disable coverage correction | Pending hardware A/B | The attach-reaching USB2 entry bypassed `init_with_super_speed`, so `--smmu-disable` was not applied there; the shared helper now covers both handoff entry points |
| 2026-09-01 | USB2 post-Run/Stop event-DMA probe wiring | Pending hardware A/B | The post-link event probe existed only in the SuperSpeed fallback; the USB2 reuse entry now runs the same STARTTRANSFER/ENDTRANSFER/GEVNTCOUNT check |
| 2026-09-01 | Post-Run/Stop probe fallback isolation | Pending hardware A/B | When this diagnostic is enabled, a failed USB2 probe must not fall through to the SuperSpeed retry, otherwise the host attach cannot identify which path produced it |
| 2026-09-01 | Main boot fallback isolation for post-Run/Stop probe | Pending hardware A/B | The top-level Bramble boot path also retried `init_usb2_only()` after a failed handoff; the diagnostic mode now suppresses that cold fallback and limits retries to one |
| 2026-09-01 | Post-Run/Stop probe SETUP re-arm correction | Negative / HS attach unchanged | After ENDTRANSFER, the real EP0 OUT SETUP TRB was started again before returning to the host-facing enumeration loop; this did not prevent the descriptor read timeout |
| 2026-09-01 | Fastboot DMA-page reuse A/B (`--no-smmu --reuse-fastboot-dma`) | Negative / earlier boundary | The reuse path produced no Fullerene HS attach; Android `18d1:4ee7` recovered. It stopped before the host could test EP0 on the reused page |
| 2026-09-01 | SMMU-page adoption A/B (`--direct-handoff --dma-adopt-smmu`) | Negative / earlier boundary | No Fullerene HS attach; the Fastboot device disconnected and Android `18d1:4ee7` recovered. No usable adopted DMA window was demonstrated |
| 2026-09-01 | Host-visible SOF reception probe (`--direct-handoff --signal-probe --signal-early-drop 5`) | Negative / no probe trigger | No early disconnect and no Fullerene HS attach; the run returned through Android before the SOF readout produced a host-visible boundary |
| 2026-09-01 | EP0 event-ring re-publication A/B (`--direct-handoff --event-ring-at-runstop`) | Negative / earlier boundary | No Fullerene HS attach; Fastboot `18d1:4ee0` disconnected and Android `18d1:4ee7` recovered. The timing-only ring rewrite did not reach host enumeration |
| 2026-09-01 | Android msm gadget-start repeat A/B (`--direct-handoff --gadget-restart-at-runstop`) | Negative / earlier boundary | No Fullerene HS attach; Fastboot `18d1:4ee0` disconnected and Android `18d1:4ee7` recovered. Re-running the full EP0 start sequence immediately before Run/Stop did not help |
| 2026-09-01 | Android Qualcomm USB2 PHY suspend-policy A/B (`--direct-handoff --usb2-susphy`) | Negative / earlier boundary | No Fullerene HS attach; Fastboot `18d1:4ee0` disconnected and Android `18d1:4ee7` recovered. Enabling USB2 `SUSPHY` at the final boundary suppresses the attach on this handoff |
| 2026-09-01 | Mainline DWC3 `DCFG.IGNSTRMPP` A/B (`--direct-handoff --dcfg-ignstrmpp`) | Negative / earlier boundary | No Fullerene HS attach; Fastboot disconnected and Android `18d1:4ee7` recovered. The packet-stream-ignore bit did not produce a Fullerene descriptor transaction |
| 2026-09-01 | Next control: repeat the unchanged direct baseline (`--direct-handoff`) | Pending | Confirm that the attach path remains reproducible before selecting another EP0-only change |

## Physical run ledger

Rows `150741.0` through `223537.0` use the attach-reaching direct profile unless a
row explicitly names a different option: `--direct-handoff --no-smmu
--start-after-connect --signal-probe --smmu-disable --u0-arm-probe
--observe-secs 1 --skip-typec-spmi`. The build-script output in each run
directory is the source of truth for the exact cfg set; the row title names the
isolated differential.

| Run | A/B or condition | Host observation | Status | Notes |
| --- | --- | --- | --- | --- |
| `1551282.0` | EP0 TX FIFO fix | Fullerene attach followed by `-110`; Android `18d1:4ee7` | Negative | SHA `a5ac8d976f23316bba94842908d0d912fd8138912d29b2d8f07c525786185854` |
| `1554505.0` | Composite `pub` readout | Ended during collapse before parking | Inconclusive | Do not interpret readout code as a success value |
| `1557406.0` | SETUP-arm link blip | No host-visible blip; `-110` | Negative | SHA `f9d097e9cfc131455570594998baf4187b6d0dfc9214429b5ccfd490414d2254` |
| `1560461.0` | Ungated SETUP arm | `-110`; no `1234` | Negative | SHA `33c98edfa9ece556da937eb8e59f9bcc13f969565832f24a899c5d17ca7c76d2` |
| `1579019.0` | Reduced cooldown / retry | `-110`; Android recovered | Negative | SHA `47dde08b2bf209804e05b2bff38cdaa74704b09b312528e07478f075467a9150` |
| `1581994.0` | Same family A/B | `-110`; Android recovered | Negative | SHA `9e33b2d10ed5d1b5a9147240cf291e98181c40ca3f687dba5aecefef98003178` |
| `1589000.0` | Start-after-reset | `-110`; Android recovered | Negative | SHA `8017d582c663572b4b365c75d687081decadbd0339cb67208f0a64fb0ffcb79f` |
| `816112.0` | EP1 raw status via APSS-WDT | Descriptor `-110`; no extra disconnect | Transport readout invalid | SHA `7682d469ca62b07c70eddc3bb5d7d3d89a5f3cd109e96fb354f248da77cb9c9f` |
| `824061.0` | EP1 status-nibble success gate | No extra disconnect | Gate not reached | SHA `913971a7505dfc529cc24e0b964d6dc8f55d4275ca67211b89375e7a1e046ddc` |
| `827607.0` | EP1 BUS_EXPIRY gate | No extra disconnect | Gate not reached | SHA `8b215a91419213d1d11543ea12ac80581d0eebdb1e2da68eac5aeb104c1bc598` |
| `830620.0` | EP1 No Resource gate | No extra disconnect | Gate not reached | SHA `cbb8640c1286d23be888c7d61f9c8d792dcc31af6cf86d156c0b6951fb836cb7` |
| `833373.0` | EP1 other-status gate | No extra disconnect | Gate not reached | SHA `3e1a61a3c70f36e3473d8ae49456b8503739b2140b47417f142d4137cdc3adb3` |
| `150741.0` | Standard loop after RAMCLKSEL capture-validity fix | Attach at `10:36:20`; descriptor `-110` at `10:36:25`; Android `18d1:4ee7` at `10:36:47` | Negative | Artifact SHA `99f2c636a6b86d63ca1132fdc524070998be0f2028e36e84257a9fd8e2251cc7`; watchdog |
| `180096.0` | Standard loop + `--xbl-ep0-in-data` (`TRB_NORMAL` for EP0 IN data) | HS attach at `10:58:44`; descriptor `-110` at `10:58:50`; Android `18d1:4ee7` at `10:59:10` | Negative | Artifact SHA `f3bb299170cba3feadb66bd1af03ea10a878f91d14e65e14dc03a205a5c3d0e1`; XBL data-TRB shape alone did not change the boundary |
| `182895.0` | XBL deferred initial SETUP request (`--xbl-deferred-setup`) | No Fullerene HS attach; stock Android `18d1:4ee7` recovered at `11:00:41` | Negative / earlier boundary | Artifact SHA `8b420f5d7d31780feae910f2cd613fd95a3d726792afb71b477b7969fa73d9c3`; the XBL deferred setup path did not reach host attach |
| `186267.0` | Standard loop + `--xbl-event-dma` (`GEVNTADR0=0x0a6fc010`) | HS attach at `11:02:00`; descriptor `-110` at `11:02:06`; Android `18d1:4ee7` at `11:02:26` | Negative | Artifact SHA `df62ca9ef8e8fa0a04d6e3553d28e1e46deb9348f35fe2d5c1bedfe1e60a7631`; XBL event-ring address alone did not change the boundary |
| `189389.0` | Standard loop + `--xbl-ep0-config` (`SETEPCONFIG P1=0x300`) | HS attach at `11:03:37`; descriptor `-110` at `11:03:42`; Android `18d1:4ee7` at `11:04:03` | Negative | Artifact SHA `27da2e56a263f91b42e7a01b35d3db2d5a34a05d803d760c550d632a71fb7ac4`; XBL EP0 notification mask alone did not change the boundary |
| `192119.0` | Standard loop + `--xbl-between-ep0` (request inserted between EP0 directions) | HS attach at `11:05:04`; descriptor `-110` at `11:05:09`; Android `18d1:4ee7` at `11:05:30` | Negative | Artifact SHA `0dcf35df67f02c98cb215d9a4a99784da2d5b5e68c18a8426413871646dd7fc5`; XBL inter-pair ordering alone did not change the boundary |
| `194327.0` | Standard loop + `--xbl-direction-trb` (EP0 IN uses TRB slot 1) | HS attach at `11:06:32`; descriptor `-110` at `11:06:37`; Android `18d1:4ee7` at `11:06:58` | Negative | Artifact SHA `c5d53d614cdb7bdc4451b2b5cf7b7394bfe20925312042edd0f0982f5a8dbab0`; direction-specific TRB slot alone did not change the boundary |
| `198419.0` | Standard loop + `--xbl-stock-ep0-dma` (fixed initial SETUP/TRB addresses) | HS attach at `11:09:09`; descriptor `-110` at `11:09:15`; Android `18d1:4ee7` at `11:09:35` | Negative | Artifact SHA `d9238a85c62c709bd8e12b39105ba34586144b6b6786e546ab96a3baecb1d537`; fixed initial DMA locality alone did not change the boundary |
| `201976.0` | Standard loop + `--xbl-raw-runstop` (XBL HIRD/APPL1RES/RUN_STOP write) | HS attach at `11:11:05`; descriptor `-110` at `11:11:10`; Android `18d1:4ee7` at `11:11:31` (then host retried Android once) | Negative | Artifact SHA `25dafe7793501fe97124a12f3739bfbd2d6445f90b54376a0639aef900b64a8b`; raw Run/Stop state alone did not change the boundary |
| `207823.0` | Standard loop + `--xbl-post-endpoint-global` (usb31 global deltas after EP0 setup) | HS attach at `11:14:45`; descriptor `-110` at `11:14:51`; Android `18d1:4ee7` at `11:15:12` | Negative | Artifact SHA `41d91c66f687fb6f9cfe365e3e9a2452ad94cb8e173fb42c5d309f94bca7a6a8`; register ordering alone did not change the boundary |
| `211079.0` | Standard loop + `--xbl-trb-chain` (`TRB_CHN` on EP0 TRBs) | HS attach at `11:16:33`; descriptor `-110` at `11:16:39`; Android `18d1:4ee7` at `11:16:59` | Negative | Artifact SHA `16f836ab2a69e6591ec2b9101472f5763add88d9eda99c3592be476a1281bab3`; TRB chaining alone did not change the boundary |
| `216523.0` | Standard loop after Android msm immediate EP0 DATA `STARTTRANSFER` fix | HS attach at `11:19:40`; descriptor `-110` at `11:19:45`; Android `18d1:4ee7` at `11:20:06` | Negative | Artifact SHA `2908bea8c02e3cae2af3c9ae5770dd77ab85f835965971e7e255eea78a69bfc3`; moving DATA arm earlier did not change the host boundary |
| `220460.0` | Standard loop + `--signal-early-drop 2` (SETUP TRB retire probe) | No early disconnect; HS attach at `11:21:56`; descriptor `-110` at `11:22:01`; Android `18d1:4ee7` at `11:22:22` | Negative / no SETUP-retire evidence | Artifact SHA `c546abd1c3b23dca696a70dd5cbc698a37d8a71b27f5be2818910d22976f5120`; the code-2 probe did not observe EP0 TRB HWO clearing |
| `223537.0` | Standard loop + `--signal-early-drop 1` (event-ring delivery probe) | HS attach at `11:23:32`; descriptor `-110` at `11:23:37`; Android `18d1:4ee7` at `11:23:58` | Negative / no event-ring delivery evidence | Artifact SHA `a792f2cf392cf9230f40ceec78cad437dc4828a7c78235d5aff8a12c086f7ef0`; no early disconnect occurred |
| `230358.0` | `--no-smmu --reuse-fastboot-dma` (reuse the firmware-exposed DMA page) | No Fullerene HS attach; Android `18d1:4ee7` at `11:28:22` | Negative / earlier boundary | Artifact SHA `5c3509df8b9b88b0adfd21b4db5a5942544feb523ea7ac35b4855ed292a1d9f8`; the host never reached a Fullerene descriptor transaction |
| `234473.0` | `--direct-handoff --dma-adopt-smmu` (adopt a page from the Fastboot SMMU context) | No Fullerene HS attach; Fastboot disconnect at `11:30:04`; Android `18d1:4ee7` at `11:30:41` | Negative / earlier boundary | Artifact SHA `99f9f47eea3a55db679edb0d24037b47a9003a3877e3499a9419eecf603f03b3`; no adopted DMA window was shown to reach the host |
| `238242.0` | `--direct-handoff --signal-probe --signal-early-drop 5` (SOF reception probe) | No Fullerene HS attach; Fastboot disconnect at `11:32:13`; Android `18d1:4ee7` at `11:32:54` | Negative / no probe trigger | Artifact SHA `70f610f5be3c5916337a6446e47a2eeade55df16b20cdee95ec8cb244b1f3d8f`; the SOF condition did not produce an early disconnect |
| `241855.0` | `--direct-handoff --event-ring-at-runstop` (re-publish event ring before Run/Stop) | No Fullerene HS attach; Fastboot disconnect at `11:34:21`; Android `18d1:4ee7` at `11:34:57` | Negative / earlier boundary | Artifact SHA `1925b800592d6b29613ae04d2fd4e6d9195dfe7d61e79b78206a5d4aabe75e38`; event-ring timing rewrite alone did not produce a Fullerene descriptor transaction |
| `245371.0` | `--direct-handoff --gadget-restart-at-runstop` (repeat Android EP0 gadget-start) | No Fullerene HS attach; Fastboot disconnect at `11:36:11`; Android `18d1:4ee7` at `11:36:48` | Negative / earlier boundary | Artifact SHA `3b624030d1e5698ddd3d1192cdaeff1fa7337b6bc5995a5c9abc39b3f7a22dff`; full gadget restart at the final boundary did not produce a Fullerene descriptor transaction |
| `248329.0` | `--direct-handoff --usb2-susphy` (enable USB2 PHY `SUSPHY` before Run/Stop) | No Fullerene HS attach; Fastboot disconnect at `11:37:45`; Android `18d1:4ee7` at `11:38:22` | Negative / earlier boundary | Artifact SHA `2a28e0d1fbf2b769d451230d48e1abf98532601cc41d8369f98e34f3147afebc`; the PHY suspend-policy A/B suppressed the Fullerene attach |
| `251538.0` | `--direct-handoff --dcfg-ignstrmpp` (set `DCFG.IGNSTRMPP`) | No Fullerene HS attach; Fastboot disconnect at `11:39:34`; Android `18d1:4ee7` at `11:40:11` | Negative / earlier boundary | Artifact SHA `e9a8fdf5402935e925768abdb7a42b0dcf56c9f2cd1f7aed7de7d53d7eee48fc`; the DCFG-only A/B did not produce a Fullerene descriptor transaction |
| `254067.0` | `--direct-handoff --dcfg-ignstrmpp` (repeat after the first boot command returned without OKAY) | No Fullerene HS attach; Fastboot disconnect at `11:40:40`; Android recovered | Negative / earlier boundary | Artifact SHA `e9a8fdf5402935e925768abdb7a42b0dcf56c9f2cd1f7aed7de7d53d7eee48fc`; no new host-side Fullerene identity |
| `264301.0` | Ordinary gadget-handoff `loop` without `--direct-handoff` | Fastboot `18d1:4ee0` disconnected at `11:47:29`; no Fullerene HS attach; Android `18d1:4ee7` recovered at `11:48:05` | Negative / earlier boundary | Artifact SHA `a92e88f2988046a49ddd39fcbfc81cd5cc7d7addf904cfd54df6050f73ce60e0`; the normal IRQ-enabled route also stopped before host enumeration |
| `257475.0` | Unchanged direct baseline (`--direct-handoff`) | Fastboot `18d1:4ee0` disconnected at `11:42:50`; no Fullerene HS attach; Android `18d1:4ee7` recovered | Negative / earlier boundary | Artifact SHA `ca4418088cab5367f3945fccd20d339210d363390bc6e2bb45355b6c601ee92e`; the unchanged direct path also stopped before host enumeration |
| `manual-1151` | Direct `fastboot boot` of the previously attach-reaching `216523.0` artifact | HS attach at `11:51:51`; descriptor `-110` at `11:51:56`; Android `18d1:4ee7` at `11:52:17` | Negative / attach reproduced | Artifact SHA `2908bea8c02e3cae2af3c9ae5770dd77ab85f835965971e7e255eea78a69bfc3`; device/harness path is healthy, while the current rebuilt profiles need the attach-reaching cfg combination |
| `276223.0` | Attach-reaching direct cfg profile with current source | HS attach at `11:55:43`; descriptor `-110` at `11:55:49`; Android `18d1:4ee7` at `11:56:09` | Negative / attach reproduced | Artifact SHA `2908bea8c02e3cae2af3c9ae5770dd77ab85f835965971e7e255eea78a69bfc3`; the immediate DATA arm remains at the same host boundary |
| `281467.0` | Attach-reaching direct cfg profile + `--signal-early-drop 3` before the latch fix | HS attach at `11:59:01`; descriptor `-110` at `11:59:07`; Android `18d1:4ee7` at `11:59:28`; no early-drop disconnect | Negative / diagnostic inconclusive | Artifact SHA `f48089b4d9199470c363803d7979c21644511328bea028c7b36ac5be534bdcaf`; the polling-only latch could miss the buffer after `handle_setup()` cleared it |
| `285508.0` | Attach-reaching direct cfg profile + corrected `--signal-early-drop 3` | HS attach at `12:01:17`; descriptor `-110` at `12:01:23`; Android `18d1:4ee7` at `12:01:43`; no early-drop disconnect | Negative / SETUP latch not observed | Artifact SHA `f73d1d915d7c1cb79dff99eab74af4edf3c8ac75e469387081e08e0897587450`; code 3 did not fire despite the corrected latch |
| `288597.0` | Attach-reaching direct cfg profile + corrected `--signal-early-drop 1` | HS attach at `12:02:53`; descriptor `-110` at `12:02:58`; Android `18d1:4ee7` at `12:03:19`; no early-drop disconnect | Negative / event-ring delivery not observed | Artifact SHA `06c80026b85c8786fb1cc336e87678ffbac99a890d606a076e408ca40ba3a536`; no event record was observed before the host descriptor timeout |
| `299059.0` | Attach-reaching direct cfg profile + USB2-path `--smmu-disable` coverage correction + `--signal-early-drop 1` | HS attach at `12:10:25`; descriptor `-110` at `12:10:30`; Android `18d1:4ee7` at `12:10:51`; no early-drop disconnect | Negative / event-ring delivery still not observed | Artifact SHA `4bf85e45e2317abc6c50055575e80d90e1634370b6df297c00874741b52fd4d5`; applying the verified SMMU bypass on the actual USB2 entry did not change the boundary |
| `302211.0` | Corrected USB2 profile + XBL event-ring address A/B (`--xbl-event-dma`) + event-delivery probe | HS attach at `12:12:24`; descriptor `-110` at `12:12:29`; Android `18d1:4ee7` at `12:12:51`; no early-drop disconnect | Negative / address A/B unchanged | Artifact SHA `7b3510dee427066d477ae472229c3434597005a885834a60929351f98424c34d`; XBL's observed `GEVNTADR0=0x0a6fc010` did not produce a delivered event |
| `308598.0` | Corrected USB2 profile + post-Run/Stop event-DMA probe (`--signal-dma-post-runstop`) | HS attach at `12:16:23`; descriptor `-110` at `12:16:28`; Android `18d1:4ee7` at `12:16:49`; no early-drop disconnect | Negative / probe result not host-isolated | Artifact SHA `588aa9608929b35210b478448c82d22973a4bc1a0f2e73c29927282fc35e3d04`; the probe path still allowed a later fallback attempt, so this run does not distinguish probe success from fallback behavior |
| `316953.0` | One-shot USB2 post-Run/Stop event-DMA probe with fallback isolation | HS attach at `12:20:36`; descriptor `-110` at `12:20:41`; Android `18d1:4ee7` at `12:21:02` | Negative / probe passed, SETUP re-arm missing | Artifact SHA `e3f8abcbdf8f26262a09b8af32fcc5f6eeef75cda7448ee55fb7f22c1c556426`; the isolated path reached HS attach, proving the synthetic event-DMA test passed, but it left only a prepared (not started) SETUP TRB |
| `320551.0` | One-shot USB2 post-Run/Stop event-DMA probe with SETUP re-arm correction and fallback isolation | HS attach at `12:22:35`; descriptor `-110` at `12:22:40`; Android `18d1:4ee7` at `12:23:01` | Negative / re-arm did not reach enumeration | Artifact SHA `24ea278e0734326c467bb6683a7ac170486c1c0de214bb1fbcdd3c67001baf75`; `idVendor=1234` was absent; the real SETUP restart did not change the host boundary |
| `328652.0` | Post-Run/Stop probe result gate (`--signal-cmd-gate post`) | HS attach at `12:28:00`; descriptor `-110` at `12:28:05`; Android `18d1:4ee7` at `12:28:25` | Negative / all-three probe conditions not confirmed | Artifact SHA `896b12a82e26c5d7817e003fdfe082891cbc9123cb6e6384d552d7ffd41614c5`; the post-probe trace gate evaluated false, so the prior attach-only inference was insufficient to prove STARTTRANSFER, ENDTRANSFER, and event delivery |
| Next control | Split the post-probe result into STARTTRANSFER, ENDTRANSFER, and event-delivery gates | Pending | Keep the same direct profile and fallback isolation; identify the failing post-probe condition before changing the EP0 path again |
| Next control | Instrument the post-reset SETUP arm and event-consumer path | Pending | Keep the synthetic event-DMA probe and fallback isolation; identify whether the host reset clears the re-armed transfer or whether EP0 events are not consumed |

## Current boundary and next checks

| Item | Current value / next check | Status | Notes |
| --- | --- | --- | --- |
| USB identity | `1234:0001` not reached | Incomplete | Host journal `idVendor=1234` is the sole physical success criterion |
| Physical layer | HS attach is reproducible | Partially confirmed | D+/D- pull-up and port attach work |
| EP0 OUT | Evidence of SETUP arrival | Relatively good | Preserve the comparison against EP0 IN data/status |
| EP0 IN data | Immediate Android-style DATA arm is applied, but the first 64-byte descriptor response still times out | Blocker | The remaining failure is earlier or lower than the response-data arm; no `1234` identity has appeared |
| Collapse | Automatic recovery to Android | Recovery works | Re-test with `fastboot boot` only |
| Next control | Run the attach-reaching profile after USB2-path SMMU-disable correction | Pending | The next A/B is limited to applying the already-requested SMMU bypass on the actual USB2 entry path |

## References

- [Bramble/Lito USB device tree](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/heads/android-msm-bramble-4.19-android11-qpr1/qcom/lito-usb.dtsi)
- [Bramble/Lito QRD device-tree overlay](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/heads/android-msm-bramble-4.19-android11-qpr1/qcom/lito-qrd.dtsi)
- [Android Lito DT tag](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/tags/android-11.0.0_r0.56/qcom/lito-usb.dtsi)
- [Linux DWC3 gadget source](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/gadget.c)
- [Linux Qualcomm DWC3 glue](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/dwc3-qcom.c)
- [Bramble USB implementation](../fullerene-kernel/src/arch/aarch64/usb.rs)
- [Bramble platform resources](../fullerene-kernel/src/platform/bramble.rs)
