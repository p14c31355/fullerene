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
| Next checks | Capture the mock-UTMI clock source/rate and `GUSB2PHYCFG` at key boundaries | Pending | Restoring `USBTRDTIM=9` did not remove `-71`, so the error shift follows 16-bit PHYIF; the 60 MHz source A/B was also negative. Add read-only state evidence before another PHY/clock A/B |

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
