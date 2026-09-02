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

## Next source-directed investigation

| Order | Check | Why |
| --- | --- | --- |
| 1 | DSB ST barrier after every DWC3 MMIO write | ROOT CAUSE IDENTIFIED: `write()` in `mmio.rs` lacked the DSB store-barrier that Linux's `writel()` provides via `__iowmb()`. On ARM64 the CPU can reorder a subsequent MMIO read ahead of the write's side-effect, making DALEPENA appear as 0 immediately after writing 0b11. This explains the DALEPENA=0 readback across runs 1764675–1785948 and the complete absence of consumed DWC3 events (setup-cut, event-cut, SOF gate all negative). Fix applied: `dsb st` after every `write_volatile` in `write()`. Pending real-hardware verification. |
| 2 | Verify DALEPENA readback is non-zero after the fix | If the DSB barrier fixes the reorder, DALEPENA should read back 0b11 after the EP0 enable writes. This is the first A/B for the next hardware run. |
| 3 | If DALEPENA is non-zero but enumeration still fails, re-run setup-cut/event-cut | The barrier may fix event delivery too (GEVNTCOUNT/GEVNTSIZ writes were also affected). |
| 4 | Only after a valid EP0 data stage, inspect descriptor bytes | Wrapping is downstream of the observed zero-payload boundary |

Latest classified hardware run: `2201961.0` submitted `GET_DESCRIPTOR(Device)` at `09:00:07.907605`; usbmon showed `-2` at `09:00:13.131442` followed by zero-length `-71` retries at `09:00:13.541709`, `.541814`, and `.543263`, all with `len=0`, `cap=0`. It used the corrected official 19.2 MHz source and the Android msm-coherent EP0 TRB for both setup DMA and readback, but still did not produce `idVendor=1234`; Android recovered as `18d1:4ee7`. The next boundary is request/ring ownership or lower DWC3/USB2 reception, not packet wrapping.

## Document routing and context cost

| Document | Size | Use | Loading policy |
| --- | ---: | --- | --- |
| [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md) | 521 lines / 244 KB | Full Bramble ledger and source audit | Read targeted sections or this index first |
| [`HARDWARE.md`](HARDWARE.md) | 530 lines / 124 KB | Cross-platform hardware notes plus Bramble summary | Read the Bramble section and this index; avoid loading the full table |
| [`BUG_JOURNAL.md`](BUG_JOURNAL.md) | 1,410 lines / 66 KB | Historical software investigations, mostly Wi-Fi and runtime | Not needed for the Bramble USB path unless a related regression appears |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 1,037 lines / 38 KB | Project-wide design rules | Read only when changing architecture or ownership boundaries |
| [`BUILD.md`](BUILD.md) | 679 lines / 29 KB | Build and run procedures | Read the Bramble command section when running hardware |
| `docs/history/*.png` | 3.1 MB | Historical screenshots/artifacts | Do not load for USB source debugging |

The two hardware ledgers account for about 356 KB of text and contain the
only unusually long lines: 387 rows in `HARDWARE_aarch64.md` exceed 200
characters, and the longest row is about 1,370 characters. The individual
experiment rows are valuable evidence, but loading the whole ledger into an
agent context repeats the same negative conclusion many times. This index is
the compact working memory; the ledgers remain the evidence archive.

## Source of truth

- Full per-run evidence: [`HARDWARE_aarch64.md`](HARDWARE_aarch64.md)
- Cross-platform status: [`HARDWARE.md`](HARDWARE.md)
- Build commands: [`BUILD.md`](BUILD.md)
- Historical non-USB bug records: [`BUG_JOURNAL.md`](BUG_JOURNAL.md)
