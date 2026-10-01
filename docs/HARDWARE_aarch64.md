# AArch64 hardware notes: Bramble entry point

Read [CONTEXT_STATUS.md](CONTEXT_STATUS.md) first. This file is the compact
hardware contract for the Pixel 4a 5G (Bramble); the complete historical
ledger is preserved in the compressed [full AArch64 ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz).

The second AArch64 target, Huawei P20 Lite (ANE-LX2J, Kirin 659 / hi6250), had its
bootloader unlocked in software on 2026-10-02. The unlock procedure and the boot
contract that device exposes are documented separately:
[ANE-LX2J bootloader unlock](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.md) ·
[日本語版](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.ja.md).

## Target and safety boundary

| Item | Value / result |
| --- | --- |
| Device | Google Pixel 4a 5G / Bramble / Qualcomm SM7250 (Lito) |
| Serial | `26191JECB00076` |
| Bootloader | `b5-0.6-10489838`, unlocked |
| Test operation | `fastboot boot` only; no flash, erase, or partition writes |
| Success condition | Fullerene-owned `1234:0001` descriptor |
| Current result | Not reached; attach-reaching runs fail before descriptor payload |

## Device-tree contract

| Resource | Public / Fullerene value | Status |
| --- | --- | --- |
| DWC3 wrapper | `0x0a600000`, child window `0xcd00` | Matched |
| Apps-SMMU stream | `0xe0` | Matched |
| IOMMU DMA pool | `0x90000000..0xf0000000` | Matched |
| USB2 PHY | `0x088e3000` plus `GCC_QUSB2PHY_PRIM_BCR` | Matched |
| QMP PHY | `0x088e8000` | Not used by the USB2 handoff branch |
| Controller clocks | Core / iface / bus / UTMI / sleep / XO | Matched |
| DWC3 baseline | 8-bit UTMI, source-derived Bramble settings | Current control |

## Current USB boundary

Fullerene's direct USB2 path can reach a host-visible HS attach. The host then
requests the address-0 Device Descriptor but receives no descriptor bytes;
the representative failures are `-110` timeout and zero-payload `-71`
retries. No valid Fullerene identity has been observed.

The failure is not localized inside the device yet. The host-side timeout and
zero-payload protocol errors do not establish whether DWC3 received SETUP,
whether its event DMA reached Fullerene, whether event-register polling
returned, or whether a response was generated. Earlier notes attributed this
to HS-PHY receive/clock recovery based on a SOF gate; later `DSTS.SOFFN`
readouts advanced while attached, so that gate is not enough to retain the
PHY-RX conclusion. Until a same-boot device trace is captured, treat the PHY,
event ingress, and EP0 response as unresolved. The latest run ledger and its
exact next capture are in [the native USB investigation](BRAMBLE_USB_NATIVE_20260929.md).

The qpr1 `DCTL.KEEP_CONNECT` branch remains closed because the live
`GHWPARAMS1` readout shows this DWC3 instance does not advertise hibernation.

For exact ABL/XBL audits, physical A/B results, commands, timestamps, hashes,
and source links, use the compressed [full ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz)
with a targeted topic or Run ID.
