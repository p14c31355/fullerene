# AArch64 hardware notes: Bramble entry point

Read [CONTEXT_STATUS.md](CONTEXT_STATUS.md) first. This file is the compact
hardware contract for the Pixel 4a 5G (Bramble); the complete historical
ledger is preserved in the compressed [full AArch64 ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz).

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

The decisive internal discriminator is already recorded: STARTTRANSFER and
pre-Run/Stop event DMA succeed, but the SOF gate observes no SOF frames. The
remaining blocker is the USB2 PHY HS receive/clock-recovery path (or an
external/secure owner of it). Endpoint/TRB and packet-format changes are
downstream of the observed boundary and remain deferred until a valid EP0 data
stage exists; further progress needs a known-good comparison, USB analyzer, or
permitted JTAG/secure-debug capture.

For exact ABL/XBL audits, physical A/B results, commands, timestamps, hashes,
and source links, use the compressed [full ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz)
with a targeted topic or Run ID.
