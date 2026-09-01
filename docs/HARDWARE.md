# Real Hardware Compatibility

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

The AArch64/USB handoff hardware contract, ABL/XBL audit, physical A/B
results, host journal, and recovery state are maintained in
[HARDWARE_aarch64.md](HARDWARE_aarch64.md).

| Item | Current result | Status | Notes |
| --- | --- | --- | --- |
| Target | Pixel 4a 5G / Bramble / `26191JECB00076` | Confirmed | Unlocked; `fastboot boot` only |
| USB2 attach | Fullerene reaches HS attach | Partially successful | The physical pull-up boundary is crossed |
| Enumeration | `device descriptor read/64, error -110` | Not reached | `idVendor=1234` has not appeared |
| Latest A/B | Host-visible `STARTTRANSFER` completion gate was inconclusive | Inconclusive / diagnostic | Run `tmp/fullerene-bramble-loop.910234.0` reached HS attach at `18:24:21`, timed out with descriptor `-110` at `18:24:26`, and recovered as Android SuperSpeed `18d1:4ee7` at `18:24:47`; no `idVendor=1234` appeared |
| Latest A/B | Mock-UTMI 60 MHz source selection was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.922967.0` did not produce a new Fullerene attach and returned to Android SuperSpeed; no `idVendor=1234` appeared |
| Latest A/B | 16-bit PHYIF with `USBTRDTIM=9` still produced `-71` | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.925509.0` reached HS attach at `18:35:52`, produced repeated protocol errors `-71`, and recovered to Android SuperSpeed at `18:36:18`; no `idVendor=1234` appeared |
| Latest A/B | Android controller clock-branch rearm was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.953690.0` reached HS attach at `18:59:20`, timed out with descriptor `-110` at `18:59:25`, and recovered to Android SuperSpeed `18d1:4ee7` at `18:59:47`; no `idVendor=1234` appeared |
| Latest A/B | 500 µs clock-stabilization delay was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.959665.0` reached HS attach at `19:03:57`, timed out with descriptor `-110` at `19:04:02`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:04:23`; no `idVendor=1234` appeared |
| Latest A/B | 20 ms clock-stabilization delay was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.961672.0` reached HS attach at `19:05:26`, timed out with descriptor `-110` at `19:05:31`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:05:52`; no `idVendor=1234` appeared |
| Latest A/B | 20 ms delay without controller-branch rearm was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.967959.0` reached HS attach at `19:10:14`, timed out with descriptor `-110` at `19:10:20`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:10:41`; no `idVendor=1234` appeared |
| Latest A/B | Short first Device Descriptor DATA-IN was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.971596.0` capped the first 64-byte Device Descriptor response at 8 bytes, reached HS attach at `19:12:07`, still timed out with descriptor `-110` at `19:12:12`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:12:32`; no `idVendor=1234` appeared |
| Latest A/B | 60 MHz UTMI with all Android controller branches rearmed was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.986345.0` reached HS attach at `19:22:01`, timed out with descriptor `-110` at `19:22:06`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:22:27`; no `idVendor=1234` appeared |
| Latest A/B | Clearing `GUSB2PHYCFG.U2_FREECLK_EXISTS` was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.989630.0` reached HS attach at `19:24:02`, timed out with descriptor `-110` at `19:24:07`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:24:27`; no `idVendor=1234` appeared |
| Latest A/B | EP0 IN TX FIFO depth correction was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.992481.0` reached HS attach at `19:25:39`, timed out with descriptor `-110` at `19:25:44`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:26:05`; no `idVendor=1234` appeared |
| Latest A/B | Full SuperSpeed handoff was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.996112.0` produced no Fullerene attach; Android SuperSpeed `18d1:4ee7` recovered at `19:28:03`; no `idVendor=1234` appeared |
| Latest A/B | 16-bit PHYIF plus 60 MHz UTMI with controller-branch rearm was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.998824.0` reached HS attach at `19:29:16`, then produced repeated descriptor protocol errors `-71`; Android SuperSpeed `18d1:4ee7` recovered at `19:29:42`; no `idVendor=1234` appeared |
| Latest A/B | XBL-style `NORMAL` TRB for EP0 IN data was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1004534.0` reached HS attach at `19:33:21`, timed out with descriptor `-110` at `19:33:26`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:33:47`; no `idVendor=1234` appeared |
| Latest A/B | Android msm reset-state order was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1007502.0` reached HS attach at `19:35:17`, timed out with descriptor `-110` at `19:35:22`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:35:43`; no `idVendor=1234` appeared |
| Source correction | Direct USB2 handoff now applies Qualcomm DWC3 reference-clock calibration | Implemented / awaiting physical A/B | The direct reuse path was missing the Android msm `update_dwc3_ref_clock()` callback after UTMI-as-PIPE selection; this writes only the source-defined `GUCTL.REFCLKPER` and conditional `GFLADJ` fields |
| Latest A/B | Direct-path DWC3 reference-clock calibration was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1016340.0` reached HS attach at `19:40:45`, timed out with descriptor `-110` at `19:40:51`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:41:11`; no `idVendor=1234` appeared |
| Control | Normal platform initialization did not produce a Fullerene attach | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1018984.0` showed the Fastboot disconnect at `19:42:12` but no Fullerene USB attach or `idVendor=1234`; the direct handoff remains the only attach-reaching path |
| Latest A/B | `ENBLSLPM` enabled on the direct USB2 handoff was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1027688.0` reached HS attach at `19:47:27`, timed out with descriptor `-110` at `19:47:32`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:47:53`; no `idVendor=1234` appeared |
| Latest A/B | Re-publishing the EP0 event ring immediately before Run/Stop was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1034437.0` reached HS attach at `19:52:25`, timed out with descriptor `-110` at `19:52:31`, and recovered to Android SuperSpeed `18d1:4ee7` at `19:52:51`; no `idVendor=1234` appeared |
| Latest A/B | Android msm controller block-reset clock boundary was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1050399.0` applied the `iface/core/sleep/utmi` branch stop, `core_reset` 1 ms assert/deassert, and 10 ms settle before DWC3 rebuild; HS attach at `20:03:25`, descriptor `-110` at `20:03:30`, and Android SuperSpeed `18d1:4ee7` at `20:03:51`; no `idVendor=1234` appeared |
| Latest A/B | Android-sized 4096-byte EP0 event buffer was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1054512.0` used a 4096-byte event buffer with the same direct USB2 handoff; HS attach at `20:06:09`, descriptor `-110` at `20:06:14`, and Android SuperSpeed `18d1:4ee7` at `20:06:35`; no `idVendor=1234` appeared |
| Latest A/B | Skipping the explicit QUSB2 PHY block-reset pulse was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1071095.0` reached HS attach at `20:18:34`, timed out with descriptor `-110` at `20:18:39`, and recovered to Android SuperSpeed `18d1:4ee7` at `20:19:00`; no `idVendor=1234` appeared |
| Latest A/B | Forcing the DWC3 usb31 USB2 retry policy was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1078114.0` reached HS attach at `20:23:13`, timed out with descriptor `-110` at `20:23:18`, and recovered to Android SuperSpeed `18d1:4ee7` at `20:23:39`; no `idVendor=1234` appeared |
| Latest A/B | Add the Android DT HS PHY `ref_clk_src` via the Lito RPMh `xo.lvl` ARC vote (`0x3`) | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1098280.0` reached HS attach at `20:37:15`, timed out with descriptor `-110` at `20:37:21`, and recovered to Android SuperSpeed `18d1:4ee7` at `20:37:42`; no `idVendor=1234` appeared |
| Latest A/B | Current ref-clock vote plus explicit 16-bit PHYIF reproduced `-71` | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1109167.0` reached HS attach at `20:45:00`, produced repeated descriptor/address protocol errors `-71` through `20:45:02`, and recovered to Android SuperSpeed; no `idVendor=1234` appeared. Restore the Bramble 8-bit UTMI baseline for the next control |
| Source audit | Linux `-EPROTO` is not a unique CRC/bitstuff signal | Source audit | Linux documents `-EPROTO` as bitstuff, no response within bus turn-around, or another host-controller error; the 16-bit PHYIF correlation remains a leading framing hypothesis, not a wire-level proof |
| Latest A/B | Android Run/Stop event-ring re-publication on the 8-bit baseline was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1114716.0` reached HS attach at `20:48:07`, returned descriptor `-110` at `20:48:12`, and recovered to Android SuperSpeed at `20:48:33`; no `idVendor=1234` appeared |
| Next A/B | Repeat the Android gadget-start sequence at Run/Stop | Pending / diagnostic | Re-run event-ring publication plus `DEPSTARTCFG`, all 32 transfer resources, both EP0 configs, and initial SETUP arm with `--gadget-restart-at-runstop` |
| Latest A/B | Full Android gadget-start replay at Run/Stop removed the Fullerene attach | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1117988.0` produced no `usb 1-9` HS attach and recovered only to Android SuperSpeed at `20:50:23`; no `idVendor=1234` appeared |
| Next A/B | Gate initial EP0 SETUP arm on Connect Done | Pending / diagnostic | Use `--start-at-connect-done` with the corrected 8-bit baseline and compare the first descriptor boundary |
| Latest A/B | Connect Done-gated initial EP0 SETUP arm was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1122440.0` reached HS attach at `20:52:27`, returned descriptor `-110` at `20:52:32`, and recovered to Android SuperSpeed at `20:52:53`; no `idVendor=1234` appeared |
| Next A/B | Gate initial EP0 SETUP arm on USB Reset | Pending / diagnostic | Use `--start-after-reset` with the corrected 8-bit attach-reaching baseline |
| Latest A/B | USB Reset-gated initial EP0 SETUP arm was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1125421.0` reached HS attach at `20:53:56`, returned descriptor `-110` at `20:54:01`, and recovered to Android SuperSpeed at `20:54:22`; no `idVendor=1234` appeared |
| Next readout | Check whether the host SETUP packet reaches `handle_setup()` | Pending / diagnostic | Use `--signal-early-drop 3`; a pull-up drop means SETUP DMA arrived, while unchanged `-110` places the boundary before software SETUP processing |
| Next A/B | Use XBL-observed DMA object addresses together | Pending / diagnostic | Combine `--xbl-event-dma --xbl-stock-ep0-dma` on the current 8-bit attach-reaching path to test the event ring, SETUP buffer, and initial TRB address space as one DMA differential |
| Latest A/B | XBL-observed event ring, SETUP buffer, and initial TRB addresses together were negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1131854.0` reached HS attach at `20:57:24`, returned descriptor `-110` at `20:57:29`, and recovered to Android SuperSpeed at `20:57:51`; no `idVendor=1234` appeared |
| Next A/B | Flush residual Fastboot EP0 control state before arming | Pending / diagnostic | Use `--ep0-stall-flush` on the 8-bit attach-reaching baseline |
| Next readout | Check USB2 SOF frame liveness before EP0 SETUP delivery | Pending / diagnostic | Use `--signal-early-drop 5`; a drop proves changing DSTS SOF frames, while unchanged `-110` leaves the link-reception boundary unproven |
| Latest A/B | DSTS SOF-frame liveness readout was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1138361.0` reached HS attach at `21:00:19`, returned descriptor `-110` at `21:00:24`, and recovered to Android SuperSpeed at `21:00:45`; no `1234` appeared and no SOF-triggered drop occurred |
| Next A/B | Compare 60 MHz mock-UTMI with SOF liveness | Pending / diagnostic | Use `FULLERENE_AARCH64_USB_UTMI_60MHZ=1 --signal-early-drop 5` while retaining the current ref-clock vote |
| Latest A/B | Corrected SETUP DMA arrival latch saw no host SETUP payload | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1128902.0` reached HS attach at `20:55:50`, returned descriptor `-110` at `20:55:56`, and recovered to Android SuperSpeed `18d1:4ee7` at `20:56:17`; `handle_setup()` saw no non-zero SETUP payload |
| Latest A/B | EP0 stall flush was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1134711.0` reached HS attach at `20:58:51`, returned descriptor `-110` at `20:58:56`, and recovered to Android SuperSpeed `18d1:4ee7` at `20:59:18`; the residual-stall flush did not change the boundary |
| Latest A/B | 60 MHz mock-UTMI with SOF liveness was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1140933.0` reached HS attach at `21:01:57`, returned descriptor `-110` at `21:02:02`, and recovered to Android SuperSpeed `18d1:4ee7` at `21:02:23`; no `1234` appeared and no SOF-triggered drop occurred |
| Next A/B | Initialize the HS PHY before DWC3 CSFTRST | Pending / diagnostic | Use `FULLERENE_AARCH64_USB_HSPHY_BEFORE_RESET=1` on the 8-bit attach-reaching direct handoff to match Android msm's external-PHY-before-core-reset ordering |
| Latest A/B | HS PHY-before-core-reset ordering was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1149688.0` reached HS attach at `21:08:30`, returned descriptor `-110` at `21:08:35`, and recovered to Android SuperSpeed `18d1:4ee7` at `21:08:55`; no `idVendor=1234` appeared |
| Next A/B | Reproduce `-71` with SOF liveness readout | Pending / diagnostic | Use `FULLERENE_AARCH64_USB_PHYIF_16BIT=1 --signal-early-drop 5` with `USBTRDTIM=9`; classify whether the protocol-error branch has changing DSTS SOF frames before EP0 response |
| Latest A/B | 16-bit PHYIF reproduced `-71` with no SOF liveness | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1153733.0` reached HS attach at `21:11:07`, returned repeated descriptor and setup-address errors `-71` through `21:11:10`, and recovered to Android SuperSpeed `18d1:4ee7` at `21:11:33`; no `1234` appeared and no SOF-triggered drop occurred |
| Next A/B | Apply Android's HS PHY termination-tune default | Pending / diagnostic | Bramble's DT lacks `qcom,no-rext-present` and RCAL properties; add the source-equivalent `RTUNE_SEL=1` write at HS PHY offset `0xb4`, bit 0 |
| Latest A/B | Android HS PHY termination-tune default was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1159018.0` reached HS attach at `21:14:29`, returned descriptor `-110` at `21:14:34`, and recovered to Android SuperSpeed `18d1:4ee7` at `21:14:55`; no `idVendor=1234` appeared |
| Next diagnostic | Publish compact DWC3/QSCRATCH/UTMI boundary snapshots | Pending / diagnostic | Read the actual `GUSB2PHYCFG`, QSCRATCH session bits, UTMI source/branch words, `GUSB3PIPECTL`, DSTS, and HS PHY controls at takeover, reset, Run/Stop, and Connect Done before another packet-format A/B |
| Next checks | Capture the mock-UTMI clock source/rate and `GUSB2PHYCFG` at key boundaries | Pending | Android msm requests the UTMI clock at 19.2 MHz, while the Lito GCC source table advertises a 60 MHz mock-UTMI option; add read-only register evidence before another PHY/clock A/B. The detailed table records this source audit |
| Next A/B | Apply Android's HS performance-state core clock to the direct USB2 handoff (`--usb-core-hs-clock`) | Pending / diagnostic | The Bramble DT exposes `qcom,core-clk-rate-hs = 66666667`; the current attach-reaching handoff selects the nominal 133.333333 MHz state. Change only the DWC3 core clock vote/rate and retain the 8-bit UTMI/TRDTIM9 baseline |
| Latest A/B | Android HS performance-state core clock was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1189897.0` used the 66.666667 MHz HS core-clock vote with the unchanged 8-bit UTMI/TRDTIM9 setup; HS attach at `21:35:28`, descriptor `-110` at `21:35:33`, and Android SuperSpeed recovery at `21:35:54`; no `idVendor=1234` appeared |
| Next A/B | Pair the diagnostic 16-bit UTMI interface with Android's 16-bit `USBTRDTIM=5` value | Pending / diagnostic | Repeat the protocol-error branch with `FULLERENE_AARCH64_USB_PHYIF_16BIT=1 FULLERENE_AARCH64_USB_USBTRDTIM=5`; keep the ref-clock vote, core-clock state, DMA, and endpoint layout unchanged to test only the PHYIF/TRDTIM contract |
| Latest A/B | Matched 16-bit UTMI/TRDTIM settings were negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1193516.0` reached HS attach at `21:37:25`, produced repeated descriptor `-71` errors and setup-address failures through `21:37:27`, and recovered to Android SuperSpeed at `21:37:51`; matching `PHYIF=16-bit` with `USBTRDTIM=5` did not produce a valid EP0 response and no `idVendor=1234` appeared |
| Latest A/B | Matched 16-bit UTMI/TRDTIM settings were negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1193516.0` reached HS attach at `21:37:25`, produced repeated descriptor `-71` errors and setup-address failures through `21:37:27`, and recovered to Android SuperSpeed at `21:37:51`; matching `PHYIF=16-bit` with `USBTRDTIM=5` did not produce a valid EP0 response and no `idVendor=1234` appeared |
| Next A/B | Add a 20 ms settle delay after enabling the UTMI clock (`--clock-stable-delay-us 20000`) | Pending / diagnostic | Use the corrected 8-bit UTMI/TRDTIM9 baseline and change only the post-clock-enable wait; this tests whether PLL/RCG settling, rather than packet framing, explains the EP0 boundary |
| Latest A/B | 20 ms UTMI clock-settle delay was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1196977.0` reached HS attach at `21:39:39`, returned descriptor `-110` at `21:39:45`, and recovered to Android SuperSpeed at `21:40:05`; no `idVendor=1234` appeared, so an immediate post-clock PLL/RCG settling delay is not sufficient |
| Next A/B | Apply Android msm's HS Connect Done USB2 policy (`--android-hs-lpm`) | Pending / diagnostic | At HS Connect Done, set `DCFG.LPM_CAP`, clear `DCTL.L1_HIBER_EN`, and restore the DT `HIRD_THRES=0x10` value exactly where Android does; this is a DWC3 link-policy test, not packet wrapping |
| Latest A/B | Android msm HS Connect Done USB2 policy was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1223833.0` reached HS attach at `21:59:11`, returned descriptor `-110` at `21:59:17`, and recovered to Android SuperSpeed `18d1:4ee7` at `21:59:37`; `DCFG.LPM_CAP`/HIRD policy did not move the pre-EP0 boundary and no `idVendor=1234` appeared |
| Next readout | Capture host USB2 control transactions with `/dev/usbmon1` | Pending / diagnostic | Run the unchanged 8-bit baseline while reading the bus-1 usbmon binary stream through the `usbmon` group; correlate control-URB status, length, and setup bytes with the first HS attach and descriptor failure |
| Latest readout | usbmon captured the first 8-bit Device Descriptor failure | Diagnostic evidence | Run `tmp/fullerene-bramble-loop.1229271.0` attached HS at `22:02:31`; usbmon recorded `GET_DESCRIPTOR(Device)` (`80 06 01 00 00 00 40 00`) submitted at `22:02:36.579408` and completed at `22:02:36.811699` with `status=-71` and zero data. The dmesg summary later showed `-110`; this establishes a host-controller protocol-error completion before the final timeout, but not the wire-level CRC/bitstuff cause |
| Next readout | Capture usbmon on the reproducible 16-bit `-71` branch | Pending / diagnostic | Repeat the same bus-1 capture with `FULLERENE_AARCH64_USB_PHYIF_16BIT=1 FULLERENE_AARCH64_USB_USBTRDTIM=5`; compare descriptor completion status, data length, and retry cadence against the 8-bit capture |
| Latest readout | usbmon confirmed the immediate 16-bit protocol-error branch | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1234879.0` attached HS at `22:06:35`; each `GET_DESCRIPTOR(Device)` completion returned `status=-71` with zero data, with three failures from `22:06:35.758404` to `.758784` and repeated groups thereafter. The 16-bit path is an earlier/faster host-controller failure than the 8-bit path; no `idVendor=1234` appeared |
| Source correction | Restore Android msm's `dwc3_set_mode(DEVICE)` controller-mode tail | Source correction applied | The direct handoff selected `PRTCAPDIR=DEVICE` but omitted the second GCTL write: `U2RSTECN=1`, `SOFITPSYNC=0`, `PWRDNSCALE=2`, and `U2EXIT_LFPS=1`. This is a controller-mode A/B against the usbmon-confirmed pre-EP0 protocol-error boundary, not packet wrapping |
| Latest A/B | Android msm `dwc3_set_mode(DEVICE)` controller-mode tail was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1248371.0` reached HS attach at `22:16:47`; usbmon still completed `GET_DESCRIPTOR(Device)` with zero data and `-71` at `22:16:52.729609`–`.729833`; Android SuperSpeed `18d1:4ee7` recovered at `22:17:13`; no `idVendor=1234` appeared |
| Next diagnostic | Compare the complete Android msm HS PHY init against the direct handoff | Pending / source-guided | Re-check the femto PHY's write order and field semantics around `CFG0`, `UTMI_CTRL5`, `COMMON0/1/2`, `CTRL1/2`, `RTUNE_SEL`, and `REFCLK_CTRL`; select only a still-unmatched source-defined field for the next A/B |
| Next readout | Time-encode the live DWC3 USB2 link state on the 16-bit `-71` branch (`--signal-cmd-gate lnkraw --observe-secs 16`) | Pending / diagnostic | Stop immediately after the normal ~15 s Fullerene attach delay, while the host is still retrying the first descriptor; use the existing DCTL stop/disconnect readout to distinguish a live link-FSM state from a pre-transaction wedge |
| Latest readout | 16-bit `-71` branch with one-second `lnkraw` observation was inconclusive | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1167343.0` reached HS attach at `21:20:27`, produced repeated descriptor/address `-71` errors through `21:20:29`, and recovered to Android SuperSpeed at `21:20:53`; the one-second observation ended before the normal ~15 s attach delay, no host-visible DCTL disconnect was observed, and no `idVendor=1234` appeared |
| Latest readout | 16-bit `-71` branch with twenty-second `lnkraw` observation was inconclusive | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1171830.0` reached HS attach at `21:23:17`, produced repeated descriptor/address `-71` errors through `21:23:19`, and recovered to Android SuperSpeed at `21:23:42`; the gate stop occurred after the host had already abandoned enumeration, so no exact DSTS link state is claimed and no `idVendor=1234` appeared |
| Latest readout | 16-bit `-71` branch with sixteen-second `lnkraw` observation was inconclusive | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1176736.0` reached HS attach at `21:26:10`, produced repeated descriptor/address `-71` errors through `21:26:12`, and recovered to Android SuperSpeed at `21:26:36`; no host-visible DCTL disconnect was observed, the timing channel did not yield an exact DSTS state, and no `idVendor=1234` appeared |
| Next readout | Inspect the local UP1A.231105.001.B2 Factory bootloader package for ABL/XBL USB register or error-path evidence | Pending / read-only | The package matches the connected Bramble's bootloader/radio identifiers. This is static binary inspection only; no flashing or boot action is part of this step |
| 2026-09-01 | Factory ABL HS-PHY source audit | Source evidence | The bootloader's `usb_shared_hs_phy_init` clears `UTMI_CTRL5.ATE_RESET`, `TEST1[6,4]`, `COMMON0.VATESTENB`, and `TEST0[7:0]`, then waits 20 us after the suspend-N handoff and another 20 us after clearing `CFG0.CMN_CTRL_OVERRIDE_EN`; the current Linux-derived path did not perform this cleanup |
| Next A/B | Apply Factory ABL's additional QUSB2 HS-PHY cleanup (`--abl-shared-hs-phy`) | Pending hardware run | Keep the attach-reaching 8-bit handoff unchanged and add only the ABL-observed test/ATE cleanup plus two 20 us settle delays; usbmon will classify the first `GET_DESCRIPTOR(Device)` completion |
| Latest A/B | Factory ABL HS-PHY cleanup and settle ordering was negative | Negative / diagnostic | Run `tmp/fullerene-bramble-loop.1280378.0` reached HS attach at `22:41:11`; usbmon still showed the Device Descriptor URB with zero data and `-71` completions at `22:41:16.746493` and `22:41:17.094606`–`.094846`; Android SuperSpeed `18d1:4ee7` recovered at `22:41:37`; no `idVendor=1234` appeared |
| 2026-09-01 | Factory ABL/XBL Protocol Error path audit | Source audit | ABL's `Protocol Error` and `CRC Error` strings are entries in the generic UEFI status-name table used by the common status formatter, not evidence of a USB wire-error branch. The USB-specific path instead contains a bounded DWC3 `DEPCMD` busy wait at `0x0a60c80c`, `ep_cmd_write failed, cmd_type`, EP0 `endxfer`, and `recover ctrl state machine`; the RUMI-only `UTMI MMCM` messages are a separate debug branch. The ABL HS-PHY cleanup A/B was already negative, so payload wrapping is not the next control |
| Next readout | Capture the Fullerene pre-EP0 DWC3 command/state boundary against the ABL endpoint-command path | Pending / diagnostic | Keep the proven 8-bit attach path and use the retained trace/usbmon evidence to distinguish `DEPCMD`/EP0 ownership failure from a lower USB2 PHY framing failure; do not treat host `-71` alone as proof of CRC or bitstuff corruption |
| 1301400.0 | Use Factory ABL's observed narrow DWC3 device-event mask (`--abl-devten`, `DEVTEN=0x47`) on the attach-reaching 8-bit handoff | HS attach at `22:56:26`; `GET_DESCRIPTOR(Device)` was submitted to unaddressed `dev=0` at `22:56:32.418396` and completed with zero data and `status=-71` at `.418531`, `.418624`, and `.418745`; Android SuperSpeed `18d1:4ee7` recovered at `22:56:53`; no `1234` attach | Negative / matching ABL's `DEVTEN` mask did not move the first EP0 protocol-error boundary. The usbmon binary stream shows immediate xHCI completion errors, but no wire-level CRC/bitstuff diagnosis; packet wrapping is not implicated by this A/B | Artifact SHA `45a1dd99ec63a43183691715e75cb4608579ada30a3b1a962a078d0fb947c62`; usbmon `tmp/fullerene-bramble-loop.1301400.0/usbmon-bus1.bin` SHA `913d0f1a3a3a8f11505aaf616e53d8f3250bce45ecaa17c1f9ffcb4b2c0e9614`; QEMU preflight and `fastboot boot` were accepted; no flash/erase operation was attempted |
| 1315217.0 / 1319391.0 / 1322401.0 | Use existing `mrad`, `pub`, and `dstat` diagnostic gates around the first EP0 transaction | `mrad`: first descriptor completion was `-2` after about 5.48 s, then retries were immediate `-71`; `pub` did not yield an exact progress code; `dstat` produced no host-visible extra DCTL reattach cycle. All runs reached HS attach and recovered to Android SuperSpeed; no `1234` attach | Negative / inconclusive readout. The gates did not expose a trustworthy internal progress value, and the `-71` retry cluster still follows the same first-descriptor boundary. Keep these as timing/readout controls, not as evidence that packets need wrapping | Raw usbmon SHA: `8871bcd9d6ab8559d9c5b5571616205f419518932b8fdf955dbd64a8afd7b668` (`mrad`), `40526e1e2b3e0f4392ba52b53973409730ed9e95b2b7159d0a466e5db1fbdaa3` (`pub`), `fb40c365c0c8be7cf87e8f324357dff2870a8d83b273321902a246979224a287` (`dstat`); each run returned by watchdog; no flash/erase operation was attempted |
| Next control | Instrument the Fullerene EP0 `DEPCMD`/event ownership boundary before the first descriptor | Pending diagnostic | Capture the local DWC3 device-event status, EP0 command busy/complete state, and event-ring consumer state immediately before and after the first `GET_DESCRIPTOR`; compare with ABL's endpoint-command busy poll. Keep the 8-bit PHY and response TRB unchanged |
| Next readout | Stop at the first observed EP0 SETUP (`--signal-cmd-gate setup-cut`) | Pending diagnostic | This is a timing-sensitive protocol boundary test: a host-visible disconnect means SETUP DMA/processing reached Fullerene, while the unchanged `-110`/`-71` sequence means the failure is earlier in USB2 RX, DWC3 event delivery, or EP0 ownership. It does not identify CRC versus bitstuff error by itself |
| 1355633.0 | Stop at the first observed EP0 SETUP (`--signal-cmd-gate setup-cut`) | HS attach at `23:36:55`; no diagnostic disconnect; descriptor read timed out with `-110` at `23:37:00`, and Android SuperSpeed `18d1:4ee7` recovered at `23:37:20`; no `1234` attach | Negative boundary readout. The SETUP latch was not reached before the host's descriptor timeout under the direct DCTL stop test, so payload wrapping cannot be the current lever. usbmon shows the descriptor submit at `23:36:55.078377`, `-2` at `23:37:00.106461`, then `-71` retries at `.717188` and `.717306`, all with zero data | Artifact SHA `0b97288476e15818b3c33374912ffe5383b650fa8ae0b6c5573c0be7702796c9`; usbmon `tmp/fullerene-bramble-loop.1355633.0/usbmon-bus1.bin` SHA `7daf8f1e0fb032798646875a8be36d9feac6d1d625f0cef2fcaa504780e9acbd`; `fastboot boot` only, no flash/erase |
| Next readout | Stop at the first consumed DWC3 event (`--signal-cmd-gate event-cut`) | Pending diagnostic | This precedes SETUP parsing: a diagnostic disconnect would prove event-ring delivery and move the suspect to EP0 ownership/command handling; unchanged `-110` would move it earlier to DWC3 event delivery or USB2 link reception |
| 1362989.0 | Stop at the first consumed DWC3 event (`--signal-cmd-gate event-cut`) | HS attach at `23:42:19`; no diagnostic disconnect; descriptor read timed out with `-110` at `23:42:24`; Android SuperSpeed `18d1:4ee7` recovered at `23:42:45`; no `1234` attach | Negative boundary readout. The event-delivery latch was not reached before the host timeout, so the response TRB and packet wrapping remain downstream of the observed failure. usbmon recorded the descriptor submit at `23:42:19.657436`, `-2` at `23:42:24.715453`, then zero-length `-71` retries at `.295467`, `.295586`, and `.295817` in the next retry burst | Artifact SHA `d9736efba957825cf6eec402b3e588a1bb82df0c28bff528c81ac12658d69cb0`; usbmon `tmp/fullerene-bramble-loop.1362989.0/usbmon-bus1.bin` SHA `c5548dbd800f0074b052972b814650a8a4fca0b92534f98fc20ccd215ad7e45d`; `fastboot boot` only, no flash/erase |
| Next control | Audit/fix DWC3 event-ring producer/consumer ownership before EP0 SETUP | Pending source-guided A/B | Compare event-ring base/size/count publication and consumer acknowledgement against Android/ABL; retain the 8-bit PHY and descriptor response unchanged, because no software SETUP boundary was reached |

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
