# Real Hardware Compatibility

For a compact, context-efficient status of the active Bramble investigation,
start with [CONTEXT_STATUS.md](CONTEXT_STATUS.md). The Bramble section below
is a legacy cross-platform summary. The compact AArch64 contract is in
[HARDWARE_aarch64.md](HARDWARE_aarch64.md); the complete [AArch64 ledger](../evidence/bramble/HARDWARE_aarch64_FULL.md.gz)
is preserved outside the default `docs/` context surface.

## ESP32-2432S028 / Sparkle IoT XH-32S (bring-up)

This ESP32-class board is a new Xtensa platform profile, not an ESP-IDF
application. The Xtensa port is split into each Fullerene crate's
`src/arch/xtensa/esp32/` tree; there is no separate `fullerene-esp32` crate.
Flasks is the build/flash/monitor task runner.

The current port initializes UART, disables both watchdog mechanisms during
early boot, and reaches a stable cooperative scheduler. The ESP32 ELF and image
generation path builds, and `esptool image-info` accepts the image. This is not
evidence of a working desktop. The following remain bring-up items and may fail
explicitly rather than pretending success:

- timer preemption and exception-frame context switching
- physical confirmation of SPI LCD protocol/backlight behaviour
- I2C resistive-touch controller and calibration
- SDMMC host and FAT mounting
- persistent settings and interactive shell integration

Board pin defaults currently used by the profile are unverified against traces:

| Device | Pins |
| --- | --- |
| LCD SPI | SCLK 14, MOSI 13, DC 2, CS 15, RST 12, backlight 21 |
| Touch SPI | CLK 25, MOSI 32, MISO 39, CS 33, PENIRQ 36 (external pull-up required) |
| SDMMC | CLK 18, CMD 23, DATA0 19 |

Do not treat these values as hardware confirmation. Probe traces, chip IDs, and
runtime responses before relying on them; update this table only with verified
evidence.

### Display transport, dirty regions, and touch events (2026-10-11)

The LCD moved from bit-banged GPIO to the ESP32's HSPI (SPI2) peripheral, the
dirty region now travels from the compositor to the transfer, and the touch
controller produces events instead of raw samples.

**Registers.** Every offset and bit position comes from ESP-IDF v5.3 rather
than from memory: `reg_base.h` (`DR_REG_SPI2_BASE = 0x3ff64000`,
`DR_REG_DPORT_BASE = 0x3ff00000`), `spi_reg.h`, `dport_reg.h`
(`DPORT_PERIP_CLK_EN_REG` at 0xc0 with `DPORT_SPI2_CLK_EN` on bit 6),
`io_mux_reg.h` (GPIO13 = HSPID and GPIO14 = HSPICLK on MCU_SEL 1) and
`hal/esp32/include/hal/spi_ll.h` for the transaction shape
(`usr_mosi_dbitlen = bitlen - 1`, little-endian W0..W15 staging, poll
`SPI_CMD.USR` back to zero). The clock-divider search is a port of
`spi_ll_master_cal_clock`, with one deliberate change: the requested frequency
is treated as a ceiling, because a panel's timing limit is a spec and not a
suggestion.

**Layout.**

| Piece | Where |
| --- | --- |
| HSPI transport, chunking, pacing | `nitrogen/src/arch/xtensa/esp32/spi.rs` |
| Panel driver, rectangle flush | `nitrogen/src/arch/xtensa/esp32/display.rs` |
| Shared rectangle arithmetic | `nitrogen/src/geometry.rs` |
| Divider arithmetic | `nitrogen/src/spi_clock.rs` |
| Pointer events and debounce | `nitrogen/src/input.rs` |
| XPT2046 sampler to events | `nitrogen/src/arch/xtensa/esp32/touchscreen.rs` |
| Dirty-region owner | `lattice/src/arch/xtensa/esp32/backend.rs` |
| Wiring, drawn latency probe | `fullerene-kernel/src/arch/xtensa/esp32/runtime.rs` |

**Why the flush no longer stalls input.** The peripheral shifts each 64-byte
chunk without the CPU, and the driver calls back into the input path between
chunks. That callback is a single pad read when the pen is idle and a full
conversion when it is down, so the sampling rate is set by the chunk size
rather than by the frame duration.

**Timebase.** `time.rs` now programs the timer prescaler to 80 for a 1 MHz
count instead of writing the value 1, which ESP-IDF documents as invalid
("0 and 1 are not valid") while the tick accounting assumed a rate that
follows from neither reading. One tick is one microsecond by construction, and
the drawing-latency numbers are measured against it.

**Verification status (measured on the board, 2026-10-11).** All three targets
build (xtensa, x86_64-uefi, aarch64-ane) and the host suite covers the rectangle
arithmetic, the divider search, the debounce state machine and the event queue.
The device reports:

```text
PANEL TEST BARS                 four bars painted straight from the driver
TIMER PROBE counter=advancing
UART RULER bytes=1024 expected_us=88064 timer_us=45220 cycles=3617536 cpu_mhz=79
TOUCH SELFTEST REPORTING
  spi_hz=8000000 cpu_mhz_measured=79 clock=1us_timer
  surface_fill_us=4725     (304x176 px)
  full_flush_us=207274     bytes=153600
  window_flush_us=144702   bytes=107008
  marker_flush_us=468      bytes=288
  pace_calls_per_full_frame=2400 input_samples_during_flush=26
```

Two things to read from that. The full frame transfers 153,600 bytes in 207 ms,
which is the 8 MHz shift clock plus about 25% of per-chunk overhead - so the
divider arithmetic is right and the ratio between a frame and a marker is the
dirty-region path working. And input kept sampling 26 times *during* that
transfer, which is the whole point of the paced flush.

The UART ruler is not a time reference, incidentally: it writes into a FIFO and
returns before the wire has drained, so it times the push, not the burst. It is
kept because it cross-checks the cycle counter against the timer, and those two
agree: 3,617,536 cycles over the 45,220 us the timer reports is 80 MHz, which is
this chip's running CPU clock (not the 240 MHz in the chip description).

### Touch: what the panel actually answered

The pin table above is the profile's assumption. It is correct for this board -
every IO_MUX offset was checked against `io_mux_reg.h` - but the *protocol* was
wrong in three ways, each found on hardware:

- The command bytes. A sweep of sixteen candidates with a finger held on the
  panel (`touchscan`, or the `touchscan` shell command) showed channel 101
  answering as 1024/1025 and channel 001 as 791 with the low bits at 00, while
  the same channels with the low bits at 01 returned a constant near 50 and
  never moved. The commands are `0xd0` for X and `0x90` for Y on this unit.
- The frame length. The conversion frame is sixteen clocks - a leading zero, the
  twelve result bits, three trailing zeros - read as a word and shifted down by
  three. Taking twelve clocks straight after the command byte loses the low bit:
  the panel centre measured 1024 where mid-scale is 2048.
- The channel pairing. The conversion after a channel change belongs to the
  previous channel, so an axis needs its own settling conversion first. The
  earlier code alternated X and Y and averaged them together, which mixed the
  two axes.

Contact discrimination was also replaced: `z1 - z2` separates cleanly (nothing
touching reads within a few counts of zero, a finger pulls them about 475
apart), where the previous `z1 + 4095 - z2` sat near 4092 idle and 4570 pressed
and so fired on an untouched panel.

A fifth fault was in the shell rather than the panel: the boot self-test saw the
idle axes never change and concluded the channel was silent, then suppressed
every event. Contact is decided by the pressure discriminator, which is a direct
measurement; requiring idle axis variation as well was an inference too far, and
it is gone. The self-test still runs, as a diagnostic line.

With those fixed, `read()` reports stable mid-scale axes, the tracker emits
Down/Move/Up with coordinates that follow the finger, and a dense burst reads
14,000 samples per second.

The axes turned out to be **crossed** on this unit: moving the finger
horizontally changes the raw Y reading. That is what made bottom-left and
top-right appear exchanged, and it is not something a per-corner guess can see
reliably. The fix is an on-screen four-point calibration (`touchcal4`): the
display draws a crosshair, the operator taps it, the device records the raw
reading for that *known* screen position, and the resolver in
`nitrogen/src/touch_calibration.rs` derives the swap, the mirrors and the ranges
from the four pairs. On this panel it reported:

```text
TOUCH CAL4 x_min=403 x_max=3429 y_min=602 y_max=3201 mirror_x=0 mirror_y=0 swap_xy=1
```

Since the four targets sit twenty pixels inside the panel, those spans cover
279x199 screen pixels rather than the full 320x239, so the baked-in defaults
extrapolate to the panel edges at the same counts per pixel (186..3646 and
341..3462). With that mapping installed, taps land on the taskbar button that was
actually pressed and the desktop switches application - the end-to-end check.

Contact also gained hysteresis: a finger leaving the glass crosses a single
threshold several times on the way out, and the shell saw eight Down/Up pairs in
twenty milliseconds. The press floor is 250 and the release level 180.

Shell commands: `touchscan` (sweep every control byte), `touchcal` (dense burst),
`touchcal4` (on-screen four-point calibration).

Two pre-existing build breakages were fixed to get here, both unrelated to the
display work: `solvent` re-exported `runtime_context` on Xtensa without a cfg
gate, `blackbox.rs` required 64-bit atomics that Xtensa does not have, and
`petroleum` declared `graphics::x86_64::uefi` without re-exporting it, which
broke the `bellows` PC build.

## InsydeH2O Firmware (June 2026)

Running on real hardware with InsydeH2O UEFI firmware required three fixes:

1. **Do not call `SetMode()`**: InsydeH2O's GOP implementation changes `frame_buffer_base` and/or invalidates `pixels_per_scan_line` after `SetMode()`, causing "backlight only" (no display output). The bootloader now uses the current mode as-is.

2. **BGR/RGB byte-order in `rgb888_to_pixel_format()`**: The color conversion function had its byte-order arguments reversed for BGR vs RGB pixel formats. For BGR hardware (common on Intel GOP), `rgb_pixel(r,g,b)` produces the correct LE memory layout `[b,g,r,0]` which BGR interprets as B=b, G=g, R=r. The fix corrects the mapping: BGR/PixelBitMask formats use `rgb_pixel(r,g,b)` while RGB formats use `rgb_pixel(b,g,r)`.

3. **Skip `safe_map_page` WC remap on real hardware**: The kernel's `safe_map_page` (via `map_page_4k_l1`) attempts to split the boot-phase 2MB/1GB huge-page WB mapping into 4KB WC pages for the framebuffer. On InsydeH2O this operation breaks the mapping entirely, making the framebuffer inaccessible. The fix relies on the existing boot-phase huge-page identity mapping (WB via PAT/MTRR), which is already functional and confirmed working via direct `write_volatile` tests.

The same rule applies to PCI device MMIO. `map_mmio_region` first verifies the
start and end of the requested higher-half direct mapping and reuses it when it
already resolves to the BAR. This keeps USB xHCI and the RTS5249 reader from
splitting a working boot huge page immediately before their first register
access. Only an actually missing or non-direct mapping is created dynamically.

## Intel Wildcat Point-LP USB (8086:9cb1 / 8086:9ca6)

The target machine exposes an xHCI controller at `00:14.0` and an EHCI
companion at `00:1d.0`. Before the first xHCI BAR0 load, Fullerene now:

1. moves the endpoint to D0;
2. enables the Intel USB3 terminations (`USB3_PSSEN`, config `0xd8`), then
   routes the firmware-declared USB2 ports (`XUSB2PR`, config `0xd0`) to xHCI;
3. disables standard ASPM and enables PCI memory decoding/bus mastering;
4. verifies the existing BAR0 direct mapping and reads the capability header as
   a 32-bit register;
5. performs the xHCI BIOS/OS ownership handoff, disables legacy SMIs, and
   waits for `USBSTS.CNR` to clear before operational-register access.

Boot registers the USB service without touching either controller BAR. The
sequence above starts only on the explicit `usb_rescan` command. Desktop and
File Manager polling never activates a deferred controller, so an uncompleted
PCIe read cannot block boot, rendering, or input dispatch. xHCI is initialized
before its companion; after Intel routing is confirmed and xHCI is active,
Fullerene does not access the unsupported EHCI companion. EHCI-only systems
still use the EHCI path, which is initialized once and never restarted by
polling.

The runtime interrupter register set begins at `RTSOFF + 0x20` (after
`MFINDEX`); using `RTSOFF` directly writes the wrong registers. Capability,
operational, runtime, doorbell, and extended-capability offsets are rejected if
they exceed the mapped BAR window.

`core::ptr::read_volatile`, an MMIO wrapper, inline assembly, and an external
xHCI crate all ultimately issue the same non-posted CPU load. None can impose a
software timeout on a PCIe transaction that never completes. The removed
`detect_abort_read_u32` helper only classified an all-ones value *after* a load
completed and therefore did not prevent a hang. Fullerene instead performs
configuration-space preflight before the first MMIO access; later watchdog
recovery remains a platform mechanism, not a replacement read primitive.

Real-hardware validation is still required for the complete controller reset,
port enumeration, and mass-storage path on this machine.

## AHCI SATA storage and rescan behavior

AHCI devices are matched by PCI class `0x01`, subclass `0x06` rather than by a
vendor/device allowlist. The kernel enables PCI memory decoding, maps BAR5 as
the HBA register window, performs an HBA reset, reads `GHC` and `PI`, and then
probes each implemented port. `PI` is a bit mask: a message such as
`ahci0 (1 ports)` means that one HBA port is implemented, not that one disk was
identified.

For each implemented port, the driver waits up to one second for
`PxSSTS.DET == 0x3` (`device present, PHY established`). It then stops the
command engine, allocates the command list/FIS/command table/DMA buffer,
starts FIS reception and command processing, and sends ATA `IDENTIFY DEVICE`
(`0xEC`). A port is published only after IDENTIFY reports a supported sector
size and a non-zero sector count. Successful disks are registered as
`/dev/sataNpN`; the port number is the AHCI port number, so a disk on HBA port
3 may appear as `/dev/sata0p3`.

`ahci_init` is safe to call repeatedly. If the controller already exists, it
re-probes implemented ports that have not yet completed IDENTIFY and then
refreshes the kernel block-device registry. This covers SATA links that become
ready after boot and prevents a controller-only initialization from leaving a
stale empty `/dev` view. The installer also refreshes its target list when
moving from Welcome to disk selection.

The following log sequence distinguishes controller readiness from disk
readiness:

```text
AHCI: HBA BAR5=... GHC=... PI=...
AHCI port 3: ATA disk (... bytes/sector, ... sectors)
AHCI: registered /dev/sata0p3 (... bytes/sector, ... sectors)
AHCI init: ahci0 ready; 1 ATA disk(s) registered
```

If the final port message is `no PHY ... (SSTS=..., SERR=...)`, the HBA was
found but no SATA link reached the PHY-ready state. In that case no AHCI block
device is registered and the installer correctly reports that no target is
available. The BAR5, `GHC`, `PI`, `PxSSTS`, and `PxSERR` values should be
captured before investigating the SATA cable/connector, firmware storage mode,
or a platform-specific link-reset requirement.

## Intel iwlwifi 7265-family PCI probe

The supported Intel IDs are `8086:095b`, `8086:095a`, `8086:08b1`, and
`8086:08b2`. Wi-Fi initialization is deferred to Solvent's service tick, so a
driver probe cannot block the kernel's boot sequence indefinitely. The phases
are PCI discovery, MMIO setup, DMA allocation, firmware upload/alive polling,
and post-alive initialization commands.

The PCI probe now preserves the firmware-assigned BAR0. It uses
`read_bar_info()` and maps an 8 KiB register window; it does not use
`get_bar_info()`, whose all-ones BAR size probe is unsafe for a live endpoint on
the affected hardware. The upstream bridge is taken from the same scan rather
than triggering a second full bus walk. PCI config-lock acquisition is bounded
so an abandoned transaction cannot permanently spin the CPU.

The affected real machine has now been observed to advance past
`step: start pci_probe` and reach `step: mmio_poll_mac`, confirming that the
non-destructive PCI/BAR probe fixes the original hang. The post-reset CSR
sequence sets both `MAC_ACCESS_REQ` and `INIT_DONE`, as required before
`MAC_CLOCK_READY` can become set; the recovery path uses the same bits. The
diagnostic marker `mmio_mac_clock_wait` means the CSR read completed but the
clock was not ready, while `mmio_read_mac` confirms that this stage passed.
The target has now advanced beyond this stage to firmware upload, but the
outer runtime timeout previously stopped it while waiting for firmware alive.
The runtime loader now selects only the `SEC_RT` image sections, skips the
firmware section separator, reports each loaded section through
`FH_UCODE_LOAD_STATUS`, follows the GP1 mailbox clear protocol, detects
7265D using CSR `HW_REV` before selecting firmware, and gives the bounded
firmware-candidate sequence enough time to finish. Physical
validation of this follow-up build is still required, so the support level
remains Alpha.

### Klog Live validation without a serial console

On the real machine, open the network menu once to request deferred iwlwifi
initialization and the first scan. Open `KLog Live` from the same desktop and
keep it visible while the scan runs. The three goal checks have these log
markers:

- Startup audio: `Sound: startup PCM playback complete` and, for HDA,
  `HDA: controller ready`.
- BusyBox: `BUSYBOX-DIAG: launch complete`, followed by
  `BUSYBOX-DIAG: terminal owner exited ... code=0 terminal closed`; the
  BusyBox terminal must return to the Fullerene shell after `exit`.
- Wi-Fi: `iwlwifi: AP FOUND ssid=...`, followed by
  `iwlwifi: scan complete (N APs found)`. When a known test AP is available,
  require `N > 0`; otherwise successful firmware readiness and scan completion
  are sufficient.

For a real-hardware USB rescan, open `KLog Live` before running `usb_rescan`.
The command returns after queue acceptance; controller activation and
enumeration continue in the scheduler-owned device phase. The stable
`[USB-RESCAN]` markers identify the last completed boundary:

```text
[USB-RESCAN] queue accepted
[USB-RESCAN] activate: USBContext::enable begin
[USB-RESCAN] activate: USBContext::enable returned
[USB-RESCAN] poll: controller poll begin
[USB-RESCAN] poll: controller poll returned
```

If the last marker is `queue accepted`, the request has not reached the device
phase yet. If it is `USBContext::enable begin`, the PCI/xHCI activation path is
stuck. If it is `poll: controller poll begin`, root-port or device enumeration
is in progress. These markers are emitted to the kernel log and taskbar ring;
they are not synchronously mirrored to the serial stream. Sealant still bounds
the MMIO region and permissions, while the NMI watchdog is the mechanism that
can recover from a non-posted PCIe read that never completes; Sealant alone
cannot cancel such a hardware transaction.

These markers are in the persistent kernel log, so serial capture is not
required. A scan that reaches `scan complete (0 APs found)` is valid when no
known test AP is expected; firmware initialization/readiness and scan
completion are the health criteria in that case.

The Realtek RTS5249 reader (`10ec:5249`) is matched by vendor/device identity,
because PCI class `0xff` is a real vendor-specific class rather than a driver
wildcard. Boot registers the reader without accessing its device registers.
The explicit `sd_rescan` command is the first BAR0 MMIO boundary; a successfully
initialized SDXC then appears dynamically as `/dev/sd0` without being mounted.
Later `sd_rescan` calls are idempotent while that device is registered or
mounted; they do not reset the live card with CMD0/ACMD41.
Mount FAT or exFAT media with, for example,
`mount /dev/sd0 /mnt/sdcard`. The mount point is any absolute VFS directory;
`mount` creates it when absent. Refresh an already-open File Manager to add the
mounted drive to its sidebar.
This keeps an uncompleted PCIe load out of the boot path. AHCI is attached at
boot through its kernel adapter, runs ATA IDENTIFY, and publishes usable SATA
disks as `/dev/sataNpN`. NVMe remains initialization-only until its block
adapter is complete; it is not used as an installation target yet.

PCI resource assignment preserves every non-zero firmware BAR without issuing
the destructive all-ones size probe. Fullerene probes and assigns only a BAR
whose address is genuinely zero. This matters for firmware-initialized USB and
card-reader endpoints whose state must survive until their explicit rescan.

Reference: Linux [`drivers/usb/host/pci-quirks.c`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/pci-quirks.c)
and [`drivers/usb/host/xhci-ext-caps.h`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/xhci-ext-caps.h).

## Intel Alder Lake-N xHCI (8086:54ed)

The Chuwi GemiBook XPro (N150) exposes a single xHCI controller at
`00:14.0` with no EHCI companion. Fullerene's USB stack handles this
configuration directly: the Intel USB2/USB3 port-routing quirk is
skipped when no EHCI companion is present, and the xHCI is initialised
through the standard HCRST → configure → start → init-ports sequence.

Extended capabilities (Supported Protocol, Legacy Support) are dumped
during init so USB2/USB3 port classification is visible in the kernel
log. Port protocol parsing assigns the correct reset type (warm reset
for USB3, regular reset for USB2) per port.

### USB hub class driver

The GemiBook XPro has an internal USB 2.0 hub (WCH CH334R,
`1a86:8091`) on a root port. External USB mass-storage devices may be
behind this hub. Fullerene's xHCI stack now enumerates devices behind
external hubs:

1. After Address Device, the device class is probed via a control
   transfer (GET_DESCRIPTOR(device)).
2. If the device is a hub (class `0x09`), SET_CONFIGURATION is issued,
   the Hub Class Descriptor is read for `bNbrPorts`, and a Configure
   Endpoint command updates the slot context with `Hub=1` and
   `NumberOfPorts`.
3. Each downstream port is polled via Get Port Status. Connected ports
   are reset via Set Port Feature (PORT_RESET). After reset, the child
   device is addressed with the correct Route String and Parent Hub
   Slot ID in the slot context.
4. Mass-storage devices behind the hub are enumerated through the
   standard BOT/SCSI path and registered as `/dev/usbN`.

This allows `usb_rescan` to discover Ventoy USB mass-storage devices
whether they are on a root port or behind the internal hub.

### GemiBook XPro USB rescan checklist

The Linux capture for the N150 identifies the machine's xHCI controller as
`00:14.0`, Intel `8086:54ed`, and the I2C-HID touch device as
`AMR13992:00 36B6:C001`. A separate older GemiBook capture contains the
KIOXIA Ventoy device (`30de:6544`, `TransMemory`) on a SuperSpeed xHCI port;
Linux reaches `usb-storage`, SCSI, and an attached removable disk for that
device. These are useful reference identities, but the N150 run itself must
still be diagnosed from its Fullerene Klog.

Before testing, open `Klog Live`, run `usb_rescan`, wait for the scheduler to
finish enumeration, and then run `usb_info`. For a successful root-port
storage path, the important sequence is:

```text
[USB-RESCAN] queue accepted
[USB-RESCAN] poll: controller poll begin
USB: device N descriptor vid=.... pid=....
USB: found BOT mass-storage interface N
USB: device N BOT reset complete ...
USB: xHCI mass-storage device ready ...
USB: registered /dev/usb0 ...
```

Interpret the last line as follows:

- `controllers: xhci poll ports returned` but no device descriptor: port or
  Address Device discovery did not produce a usable candidate.
- `xhci mass-storage enumeration returned` followed by
  `xhci non-hub unsupported device`: configuration parsing did not find a BOT
  or UAS bulk pair; this is not a reason to issue hub requests.
- `xhci hub enumeration begin`: only expected when the descriptor contains a
  real Hub interface (`class 0x09`). Continue with `hub descriptor`, `hub get
  port status`, and downstream-device markers.
- `xhci storage finish begin`: mass-storage enumeration succeeded; inspect
  `bulk out configure`, `bulk in configure`, and `read capacity` next.
- `poll: complete (no device)` followed by `queue retry` through the retry
  count: enumeration
  returned without a disk, so `usb_info` will correctly report no registered
  storage even though the controller is active.

The adjacent state line is especially useful on the N150:

```text
[USB-RESCAN] poll: state enabled=true disks=0 xhci0:ports=11 devices=1 done=0x00000004 [port=3 addr=0 class=00 parent=false]
```

`devices=0` means root-port detection did not leave a candidate. `devices=1`
means the port saw a device and the failure is in Address Device, descriptor,
BOT/UAS, or hub classification. A failed non-hub candidate is retried with
the port state reset; it must not turn the remaining retry attempts into a
misleading stream of `no device` messages.

For the HID touchpad, a physical right-button press should add these Klog Live
edges while the pointer is over the desktop:

```text
[I2C-HID] pointer buttons changed: 0x00->0x02
[I2C-HID] pointer buttons changed: 0x02->0x00
```

The first edge is `MouseDown(Right)` and routes to the desktop or Explorer
context menu; the second is `MouseUp(Right)`. A report ID 6 mouse packet with
button bit `0x02` is used for the GemiBook's physical right button, while
digitizer contact remains the left-button/tap path.

## Google Pixel 4a 5G (Bramble) USB handoff

For the current Bramble state and next investigation, read
[CONTEXT_STATUS.md](CONTEXT_STATUS.md). This section is a compact
cross-platform pointer; the historical Bramble table is preserved in the
[compressed full Bramble summary ledger](../evidence/bramble/HARDWARE_BRAMBLE_SUMMARY_FULL.md.gz).

| Item | Current result | Status |
| --- | --- | --- |
| Target | Pixel 4a 5G / Bramble / `26191JECB00076` | Confirmed; unlocked; `fastboot boot` only |
| USB2 attach | Fullerene can reach HS attach on the direct path | Partial; no valid Fullerene descriptor |
| Enumeration | Address-0 Device Descriptor receives zero-payload `-110`/`-71` failures | Not reached; no Fullerene-owned `1234:0001` |
| Recovery | Android/Fastboot return and absent-device safety gates are recorded | No persistent device mutation |

Use the compact status file for planning. Use the [compact AArch64 notes](HARDWARE_aarch64.md)
for the hardware contract, and retrieve the full ledgers by Run ID or topic
when exact commands, timestamps, hashes, or source-audit details are needed.

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
