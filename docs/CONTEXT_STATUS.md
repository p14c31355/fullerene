# Bramble context entry point

This is the default LLM-facing entry point for the Pixel 4a 5G (Bramble)
investigation. It contains the current state, fixed safety boundary, and the
next useful discriminator. Do not load the full ledgers unless a run or source
detail is needed.

Full evidence is preserved in the [status history](../evidence/bramble/CONTEXT_STATUS_FULL.md)
and the [AArch64 hardware ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md).
Use the [small Run index](../evidence/bramble/RUN_INDEX.md) to select a
targeted section before opening either archive.

## Current goals

| Goal | Success criterion | Current state |
| --- | --- | --- |
| USB handoff | Fullerene-owned `idVendor=1234`, `idProduct=0001` | Not reached |
| FullereneOS AArch64 port | Boot the real FullereneOS runtime on Bramble | Early bring-up; generic runtime not yet entered |
| Recovery safety | Failed handoff returns to Android without persistent writes | Confirmed for the recorded RAM-only runs |

## Current state (last evidence update: 2026-09-09)

- The only `1234:0001` observation was produced by a prohibited Android
  configfs rebind. No Fullerene-owned descriptor success has been observed.
- The attach-reaching Fullerene USB2 path crosses HS attach, then fails at the
  address-0 Device Descriptor boundary with zero-payload `-110`/`-71`
  completions. This is a pre-descriptor / pre-USB2-RX-data failure.
- The current source-exact control remains the reference artifact. Run
  `1700055.0` is byte-identical to the current-source standalone control and
  reaches HS attach before the same descriptor timeout.
- Run `2179479.0` tested only `--skip-typec-spmi` on the normal
  Android-init/direct-handoff profile. QEMU and image audit passed and
  RAM-only `fastboot boot` was accepted, but no Fullerene attach, descriptor,
  Android fallback, or Fastboot return was observed during the bounded window;
  the final state was `device-absent`.
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
  and gadget-start variants
- EP0 MPS, SETUP timing, TRB form, and downstream EP0 permutations
- SuperSpeed QMP, lane, Type-C, VBUS, and old-session cleanup variants
- Factory XBL/ABL and Android-init profile replays

The exact run-by-run evidence, commands, timestamps, artifact hashes, and
negative results remain in the [full status history](../evidence/bramble/CONTEXT_STATUS_FULL.md).

## Next useful work

1. Prefer a new primary-source or known-good capture at the USB2 PHY RX/SOF or
   DWC3 event-ingress boundary.
2. If the handset returns, use the existing bounded candidate queue in its
   prescribed pre-DTB/post-DTB order and retain the safety log.
3. Do not add guessed EP0/TRB, packet-format, or register mutations while the
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
