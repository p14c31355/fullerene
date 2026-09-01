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
| HS PHY override | Bramble branch: `0x63@0x6c`, `0xc8@0x70`, `0x17@0x74` | DT takes priority | Needs physical confirmation | The base `lito-usb.dtsi` value `0x85@0x70` differs because of the QRD overlay |

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

## Physical run ledger

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

## Current boundary and next checks

| Item | Current value / next check | Status | Notes |
| --- | --- | --- | --- |
| USB identity | `1234:0001` not reached | Incomplete | Host journal `idVendor=1234` is the sole physical success criterion |
| Physical layer | HS attach is reproducible | Partially confirmed | D+/D- pull-up and port attach work |
| EP0 OUT | Evidence of SETUP arrival | Relatively good | Preserve the comparison against EP0 IN data/status |
| EP0 IN data | Timeout on the first 64-byte descriptor response | Blocker | Isolate endpoint context, FIFO, interrupter, and PHY wire behavior |
| Collapse | Automatic recovery to Android | Recovery works | Re-test with `fastboot boot` only |
| Next A/B | Verify the stock DTB HS PHY override and observe the EP1 data path at the wire level | Pending | Record the hypothesis and boundary here before changing code |

## References

- [Bramble/Lito USB device tree](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/heads/android-msm-bramble-4.19-android11-qpr1/qcom/lito-usb.dtsi)
- [Android Lito DT tag](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/tags/android-11.0.0_r0.56/qcom/lito-usb.dtsi)
- [Linux DWC3 gadget source](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/gadget.c)
- [Linux Qualcomm DWC3 glue](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/dwc3-qcom.c)
- [Bramble USB implementation](../fullerene-kernel/src/arch/aarch64/usb.rs)
- [Bramble platform resources](../fullerene-kernel/src/platform/bramble.rs)
