# Native USB investigation, 2026-09-29

Native Fullerene enumeration **has not succeeded**. Six RAM-only Fastboot boots
were captured in this session. No Android/configfs identity is counted as native
progress. All six recovered through stock Android and the harness restored the
selected phone to Fastboot. No partition writes were performed.

## Verified boundary

The host detects a high-speed attachment and submits the first address-zero
Device Descriptor request (`wLength=64`). No descriptor payload is returned.
There is **no verified successful native control transaction**, SET_ADDRESS,
SET_CONFIGURATION, or stable configured device. The last positive USB observation
is host-side attachment, not a device-side Connect Done record.

The pulse-free control produces an approximately five-second timeout (kernel
`-110`; cancelled usbmon URB `-2`) followed by three zero-payload `-71` completions.
These errnos do not distinguish missing SETUP reception, DMA/event delivery,
blocked MMIO, or a failed response. The earlier notes' claims that RX is dead,
or that SETUP reached software, are not established by these runs. Native EP0
attribution still requires an independent, same-boot device-side trace.

## Initialization chain checked in the current source

The standalone `usb_probe_entry` installs DT resources, handles watchdogs and
clears the DMA region. The direct `init_usb2_handoff` requests regulator power,
GDSC and performance/interconnect votes, then enters
`init_usb2_gadget_handoff` -> `init_usb2_gadget_reuse_fastboot_ep0`.

The selected path prepares the Qualcomm session/PHY interface and stops the
inherited controller; refreshes HS-PHY rails and reference clock; performs PHY
reset/analog initialization **before** DWC3 device soft reset; enables the UTMI
clock and restores controller interface/refclock settings; establishes the DMA
policy; publishes the event ring; allocates/configures EP0; enables device events;
starts Run/Stop; then arms SETUP and polls control traffic. `--start-after-connect`
defers the first SETUP arm. USB Reset and Connect Done have separate handlers.

The tested USB2 path does not initialize the QMP SuperSpeed PHY. The standalone
gadget profile skips initial SPMI role setup; optional Type-C IRQ routes are
separate experiments. The existing Type-C deferred handler reads the child state,
applies a role change, acknowledges the PMIC and reenables its parent SPI.
No new IRQ masking or Type-C policy was introduced here. These reads establish
source order, **not successful hardware execution of each step**.

The control uses `--no-smmu`, so it assumes a firmware DMA mapping compatible with
physical addresses; that assumption is not verified by an attachment. A separate
run enabled Fullerene's existing SMMU configuration and did not move the host
boundary. Linker DMA objects are aligned; TRB publication includes `dsb st`,
cache helpers include `dsb sy`, and the explicit cache-maintenance run enabled
clean/invalidate operations. The standalone probe does not enable the normal
kernel MMU. No DMA address, cache visibility or PHY register value was read back
independently from this handset in this session.

Graph discovery used Tier 2. Coverage metadata was unavailable for the relevant
Rust files, so material functions were read directly. This is a bounded review,
not a completeness audit of every experimental build flag.

## Changes

Pre-existing changes to `ep0.rs` and `usb_protocol.rs` were preserved and completed;
the incoming patch is saved in `tmp/usb-native-20260929/pre-existing.patch`.

- Removed an unconditional `pullup_mark(2)` in the normal handoff, before DCFG,
  core reset and EP0 construction. It toggled Run/Stop without a diagnostic flag.
- EP0 now distinguishes event LST/IOC flags from TRB result status, handles
  SETUP_PENDING without committing interrupted address/configuration state, waits
  for STATUS XferNotReady, and avoids republishing an active STATUS TRB.
- Polled SETUP requires a retired SETUP TRB; a delayed ring completion is consumed
  once without completing the new data/status phase. Real SETUP events do not
  require the polling-only residual/type predicate.
- Kept the incoming early DCFG SET_ADDRESS write and software commit after status;
  repaired stall/rearm, external trace-response lifecycle and physical endpoint
  number conversion for halt commands.
- Added one-shot RAM witnesses before DCTL (`BDCT`), before event-count (`BCNT`,
  including returned DCTL), and after event-count (`ACNT`, including count).
  `TRBC` records raw endpoint/TRB control/TRB size/event before reuse. These add
  no USB pulses or extra MMIO reads. They have **not been retrieved from hardware**.
- Added `contrib/debug/decode_bramble_usb_trace.py` for an offline debugger RAM
  dump. It validates the header and circular sequence numbers; it cannot prove
  dump freshness or boot ownership.

EP0 behavior was checked against Linux's
[actual DWC3 EP0 implementation](https://raw.githubusercontent.com/torvalds/linux/master/drivers/usb/dwc3/ep0.c)
and the locally retained qpr1 `drivers/usb/dwc3/ep0.c`. This supports the STATUS,
SETUP_PENDING, SET_ADDRESS and endpoint-direction handling; it does not establish
the cause of the pre-descriptor hardware failure.

## Controlled experiments

Artifacts are under `tmp/fullerene-bramble-loop.<run>.0/`, including image hashes,
build command/environment, source diff, boot acceptance, kernel logs and usbmon.
The compact machine-readable ledger is `tmp/usb-native-20260929/run-summary.json`.

| Run | Variable / hypothesis | Result and conclusion |
| --- | --- | --- |
| 91463 | Incoming code baseline | HS attach, three zero-byte `-71` descriptor completions; no native identity |
| 96074 | Repair EP0 completion lifecycle | Same host boundary; EP0 changes alone did not restore first descriptor |
| 98841 | Remove premature Run/Stop pulses | HS attach retained; first completion changed to timeout/cancellation, then three `-71`; diagnostic pulse affected the observed failure |
| 102547 | Explicit DMA clean/invalidate | Same timeout/zero-byte boundary; this cache policy alone is not a fix |
| 107185 | Add non-pulsing RAM witnesses | Same boundary; trace instrumentation builds/boots but requires another transport to read |
| 109929 | Enable Fullerene SMMU setup | Same timeout/zero-byte boundary; no evidence that enabling this path alone restores DMA/EP0 |

Each run's QEMU preflight, Android boot-image audit and `fastboot boot` acceptance
passed. Android fallback reported `watchdog` where captured; that does not identify
which Fullerene instruction or controller state preceded recovery.

## Verification

- 27 host USB tests passed: protocol, modeled DWC3 registers and modeled Linux
  host enumeration. Includes interrupted control requests and TRB polling guards.
- Explicit AArch64 QEMU USB simulation passed through SET_CONFIGURATION.
- `cargo check -p fullerene-kernel --target x86_64-unknown-uefi` passed.
- Changed Rust files pass rustfmt; `git diff --check` passes.
- Offline trace decoder checks passed for ordinary, circular-buffer and u32
  sequence wrap, and invalid input.

These tests exercise shared protocol/models; they do not emulate the Qualcomm PHY
or execute the real MMIO EP0 wrapper on host hardware. Existing build warnings
remain. No end-to-end x86 runtime test was performed.

## Ready image and next external measurement

`tmp/usb-native-20260929/native-usb-ready.img` is built from the final source,
with direct-only, pulse-free USB2 handoff, HS-PHY-before-core-reset and no-SMMU.
Its build log/environment/hash are beside it. SHA-256:
`f56bf4878b848c154ba174e8ffeadd800a634b3e610e6ba254002713d84cb9b9`.

The highest-value next experiment is a **same-boot RAM/register capture during the
first descriptor timeout**, through a working external debug connection. No such
connection is accessible in this session; host usbmon alone cannot supply it.
Connect that debugger, retain the USB cable, and start the host captures in
separate terminals from the repository root:

```sh
journalctl -kf -o short-monotonic > tmp/usb-native-20260929/host-kernel.log
```

```sh
tshark -i usbmon0 -a duration:90 \
  -w tmp/usb-native-20260929/host-usbmon.pcapng
```

Then boot only this RAM image:

```sh
fastboot -s 26191JECB00076 boot tmp/usb-native-20260929/native-usb-ready.img
```

For this exact ELF, `.usb_trace` is `0x90007000..0x9000a000` (12 KiB), as verified
with `aarch64-linux-gnu-readelf -SW`. Capture it **during this Fullerene boot,
before watchdog recovery/Android**. Also capture DWC3 GCTL, DCFG, DCTL, DSTS,
GEVNTADRLO/HI0, GEVNTSIZ0, GEVNTCOUNT0, DEPCMD0/1 and the event/TRB buffers at the
addresses recorded in the trace. A debug read of a stalled MMIO window can itself
fail; collect RAM first. Decode the RAM dump with:

```sh
python3 contrib/debug/decode_bramble_usb_trace.py usb-trace.bin
```

Correlate with `journalctl -kf` and passive usbmon. This distinguishes a DCTL/count
access that never returns, nonzero count with missing DMA, USB Reset/Connect Done
without SETUP, and a retired SETUP followed by a failed response. A wire analyzer
can establish bus packets/electrical behavior, but does not replace the RAM trace
for proving what Fullerene serviced. Until this evidence exists, root cause is
unproven and native enumeration remains incomplete.
