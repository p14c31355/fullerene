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

### Current hardware state and test boundary

The target is a Google Pixel 4a 5G (Bramble, Qualcomm SM7250/Lito platform).
The confirmed device identity is:

```text
product: bramble
serialno: 26191JECB00076
```

The device is unlocked and is normally left in Fastboot before a hardware
test. The current test boundary is deliberately narrow: generated images are
sent with `fastboot boot` only. No Fullerene USB experiment in this section
writes a boot, vendor_boot, DTBO, or other partition.

The stock Android `boot.img` used as the temporary template was retained under
`/tmp` and is audited before a generated image is handed to Fastboot. The
audit verifies the Android v3 header, kernel payload and page padding, while
preserving the stock ramdisk and trailing data. Image, Image.lz4, and the
QEMU DWC3/EP0 protocol model are also checked before the handoff.

### Starting symptom

At the beginning of this investigation, the host saw the USB2 attach and
pull-up, but the first control transfer did not complete:

```text
usb 1-1: new high-speed USB device
usb 1-1: device descriptor read/64, error -110
```

This placed the failure after physical attach and before a usable EP0
descriptor exchange. The shared QEMU DWC3/EP0 self-test passed, so descriptor
format alone was not treated as the primary suspect. The investigation was
then moved toward the Bramble platform layer: USB2/QMP PHY state, Type-C role
ownership, PDC/DWC3 IRQ routing, DMA/SMMU access, GSI/event handling, and only
then endpoint descriptors.

### Factory image recovery and Fastboot metadata comparison

During the investigation, a Factory image was used once to clean up the A-slot
state. This was a recovery/baseline operation, separate from the subsequent
Fullerene tests. The captured `fastboot getvar all` output before and after
that operation shows the following.

The main device and bootloader identity did not change:

| Field | Before | After |
| --- | --- | --- |
| product / serial | `bramble` / `26191JECB00076` | unchanged |
| bootloader | `b5-0.6-10489838` | unchanged |
| baseband | `g7250-00264-230619-B-10346159` | unchanged |
| secure boot / secure | `PRODUCTION` / `yes` | unchanged |
| hardware revision | `MP1.0` | unchanged |
| current slot | `b` | `b` |
| unlocked | `yes` | `yes` |
| storage | Micron `MT128GASAO4U21`, rev `0302` | unchanged |
| Citadel firmware | `0.0.5/chunk_ab9914055-b41f6e4` | unchanged |

The changes were concentrated in slot metadata and A/B logical partition
metadata:

| Field | Before | After |
| --- | --- | --- |
| `slot-retry-count:a` | `2` | `0` |
| `slot-unbootable:a` | `yes` | `no` |
| `slot-successful:a` | `yes` | `no` |
| `slot-retry-count:b` | `0` | `2` |
| `slot-unbootable:b` | `no` | `no` |
| `slot-successful:b` | `yes` | `yes` |
| `battery-voltage` | `4442` | `4449` |
| `partition-size:product_b` | `0xA3732000` | `0xA3735000` |
| `partition-size:vendor_b` | `0x2D0B8000` | `0x2D0BA000` |

The post-recovery output additionally exposed the following A-side entries,
with zero sizes for three of them:

```text
partition-type:system_a:raw       partition-size:system_a:0x135F000
partition-type:product_a:raw      partition-size:product_a:0x0
partition-type:system_ext_a:raw   partition-size:system_ext_a:0x0
partition-type:vendor_a:raw       partition-size:vendor_a:0x0
```

Conversely, the pre-recovery output contained these B-side COW metadata
entries, which were absent afterward:

```text
system_b-cow, product_b-cow, system_ext_b-cow, vendor_b-cow
```

These values establish that the Factory image changed slot bookkeeping and
dynamic-partition metadata. They do not, by themselves, prove which complete
system image is bootable or explain the EP0 timeout. The important USB
baseline facts remained the same: the device stayed on slot `b`, remained
unlocked, and retained the same bootloader, baseband, storage, and hardware
identity.

### Implementation and experiment timeline

1. The initial implementation added structural audits for AArch64 `Image`,
   Rust LZ4 decoding/checksum validation for `Image.lz4`, and Android v3
   `boot.img` kernel, padding, ramdisk, and tail validation. A direct stock
   boot path was audited separately from generated images.

2. The QEMU path gained a shared DWC3/EP0 self-test covering endpoint
   configuration, SETUP/DATA/STATUS TRBs, event encoding, EP0 re-arm,
   descriptors, `SET_ADDRESS`, and `SET_CONFIGURATION`. It passed, but QEMU
   does not model the SM7250 PHY, Qualcomm Type-C glue, PDC, or Apps SMMU.

3. The Bramble platform contract was expanded toward the official Android
   device tree: DWC3 at the Bramble base, USB2/QMP PHY resources, clocks,
   resets, GDSC, Type-C/PMIC state, Apps-SMMU stream ID `0xe0`, the
   `0x90000000..0xF0000000` DMA pool, GSI offsets, bus/performance resources,
   PDC pin ranges, and GIC SPI routes. The code now keeps platform IRQs
   separate from DWC3 event-ring consumption.

4. An IRQ-only DWC3 probe left a host-visible `18d1:4ee0` device present until
   the intentional roughly 120-second recovery watchdog. At the time this was
   treated as evidence of a working gadget handoff. A later identity check
   corrected that interpretation: `18d1:4ee0` is the bootloader Fastboot
   identity, while the Rust `Ep0Simulator` descriptor is `1234:0001`.
   Therefore these observations establish only that the bootloader USB
   session/controller boundary can remain visible; they do not prove that
   Fullerene reached its own EP0 descriptor path. Adding the power-event IRQ
   did not cause an early failure either.

5. Enabling the Type-C parent IRQ, even with deferred PMIC handling, caused
   early disconnects in roughly 8--19 seconds. Writing the PMIC role/sink-only
   state during a live Fastboot handoff also caused disconnects in roughly
   15--24 seconds. The handoff path was therefore changed to observe Type-C
   state without taking ownership of the live role-switch state or issuing
   unsafe PMIC writes.

6. The official comparison then exposed a second ownership mistake. The
   Android/Linux Qualcomm glue registers the DP/DM/SS PHY interrupts as
   wakeup interrupts with `IRQF_NO_AUTOEN`; they are enabled for the relevant
   suspend/wakeup path, not as continuously active runtime interrupts for a
   device-mode gadget. The Linux Qualcomm PDC irqchip also always uses
   `IRQ_ENABLE_BANK` at offset `0x10`; a speculative PDC-version branch that
   used an `IRQ_CFG` enable bit was removed.

7. A PDC-enabled comparison image, including the PDC-to-parent-SPI mapping,
   parent trigger types, and the official enable-bank correction, still
   disconnected after about 35--38 seconds. The normal Bramble device-mode
   path was then changed to leave PDC pins and their parent SPIs disabled,
   matching the official `NO_AUTOEN` behavior. The dedicated PDC probe remains
   available for isolated comparison, but is no longer the normal gadget
   route.

8. The resulting no-PDC runtime image still left `18d1:4ee0` visible at
   SuperSpeed for about 95 seconds and then reset at the expected watchdog
   boundary. No `-110` appeared in that run. The duration is useful evidence
   about the handoff/reset boundary and is a substantial improvement over the
   35--38 second PDC-enabled disconnect, but the later descriptor identity
   check means it cannot be called Fullerene gadget enumeration. The PDC
   ownership mismatch remains a plausible cause of the earlier disconnect,
   but this experiment did not yet prove kernel-side EP0 progress.

9. The probe watchdog was then changed to distinguish “no host ever reached
   EP0” from a valid idle descriptor-only gadget. Once an EP0 DATA or STATUS
   transfer is successfully started, the probe records progress and enters a
   stable WFE loop instead of resetting merely because the host becomes
   quiet. The dedicated probe was rebuilt and booted with:

   ```bash
   cargo run -q -p flasks -- build --arch aarch64 --platform bramble \
     --usb-gadget-handoff-probe \
     --boot-template /tmp/fullerene-stock-template.Uvg3m2/boot.img \
     --boot-output /tmp/fullerene-bramble-stable-probe.img
   fastboot -s 26191JECB00076 boot /tmp/fullerene-bramble-stable-probe.img
   ```

   The host observed an `18d1:4ee0` SuperSpeed device at approximately
   10:09:13 and completed bootloader-style device identification at 10:09:15.
   The device remained present until approximately 10:20:16, or about eleven
   minutes. Because the identity was the bootloader Fastboot identity rather
   than the Rust `1234:0001` identity, this run does not confirm the
   EP0-progress-to-stable transition. It remains useful as a long-lived
   handoff/reset observation, but the post-boot kernel gadget boundary was
   not reached or was not made visible.

10. The retry-enabled probe was then booted again with `fastboot boot` (artifact
    `/tmp/fullerene-bramble-stable-probe-retry.img`, SHA-256
    `8b49f46dfd40686d696008832fd1fd368ea1ed51248bcfa700c3ed4ad5d3584f`).
    The host again observed `18d1:4ee0`; a full `lsusb -v` read was possible
    in one interval and the device remained present for at least 50 seconds.
    However, `18d1:4ee0`, `Google`, `Pixel 4a (5G)`, and the Fastboot string
    identify the bootloader, not the Rust gadget descriptor (`1234:0001`).
    The later automated run made the boundary explicit: the old
    `18d1:4ee0` device was still present immediately after the boot command,
    then disconnected at approximately 10:49:03, and no `1234:0001` device
    appeared. The earlier post-boot `fastboot getvar` success therefore
    measured the bootloader, not Fullerene's gadget.

11. The host verification harness was corrected to require this identity
    transition. It now checks Fastboot `product=bramble` before the handoff,
    waits for `18d1:4ee0` to disappear after `fastboot boot`, and accepts
    post-boot enumeration only as `1234:0001`, followed by descriptor capture
    and a hold interval. A missing transition is recorded as a failure with
    kernel/build/boot logs rather than being reported as USB success. The
    harness is `cargo run -q -p flasks --bin bramble-usb -- loop`; it never
    invokes `flash`, `erase`, or reboot.

12. A linker-layout audit found that the probe's 2 KiB-aligned exception
    vector table was in the same `.text.boot` section as `_start`. That moved
    the first boot instruction to `0x80080800`, while the generated Image and
    relocation bootstrap use `0x80080040` as the Bramble payload base. The
    vectors are now emitted into the dedicated `.text.exception_vectors`
    section. The rebuilt probe reports entry point `0x80080040`, keeps the
    vectors separately aligned at `0x80084800`, and passes the Bramble boot
    audit. This is the next candidate fix for the missing `1234:0001`
    post-boot identity; it still requires a fresh physical Fastboot run.

13. The new harness also ran the same section-fixed probe with
    `--boot-uncompressed`. The build and boot-image audit passed, but Bramble
    rejected the temporary image with `FAILError verifying the received
    boot.img: Unsupported`. This isolates the current device path to the
    generated `Image.lz4` form; the uncompressed variant is not accepted by
    this bootloader and is not a viable hardware comparison on its own.

14. A compile-time SMMU differential was added as
    `--usb-gadget-handoff-no-smmu`, with the host harness exposed as
    `cargo run -q -p flasks --bin bramble-usb -- loop --no-smmu`. This path does not read or modify
    the Apps-SMMU registers and relies on the Fastboot-owned physical=IOVA
    bypass while keeping Fullerene's DMA objects inside the declared Bramble
    pool. The build and Android boot-image audit passed (`kernel=56221` bytes).
    The physical run still produced neither `1234:0001` nor a replacement
    bootloader device within 30 seconds; the host saw only the expected
    Fastboot USB disconnect. Because the then-current probe disabled its
    recovery timer immediately after init, that run left the phone in a
    no-USB state and required manual recovery.

15. The probe recovery boundary was corrected after that run. INTID 30 is the
    EL1 physical-timer PPI, not a Qualcomm USB resource, so the IRQ handler now
    handles it explicitly and resets through the existing PS_HOLD/PSCI path.
    The timer remains armed until the first EP0 DATA or STATUS transfer and is
    disabled only after that progress point. The harness correspondingly waits
    for watchdog recovery after an enumeration timeout. This improves
    unattended iteration safety but does not yet prove USB enumeration on
    hardware; a fresh compressed-image run is still required after the phone
    is returned to Fastboot.

16. The handoff harness now captures Fastboot `getvar all` before the handoff,
    records boot-to-enumeration timing and the host's USB topology, and can
    require an actual SuperSpeed link with
    `cargo run -q -p flasks --bin bramble-usb -- loop --super-speed`. The corresponding
    `--usb-gadget-handoff-super-speed-probe` build variant preserves the
    Fastboot-owned clock/rail domain, resets only the DWC3 device state, then
    reinitializes the QMP combo PHY before publishing the event ring and
    512-byte EP0 configuration. Its compressed Bramble artifact passed the
    QEMU preflight and Android v3 boot-image audit. No physical SuperSpeed
    Fullerene identity has been claimed yet because the host has not observed
    `1234:0001` after the bootloader disconnect.

17. The same harness now exposes the existing Qualcomm IRQ comparison routes
    (`power`, `typec`, `typec-role`, `pdc`, and `smmu`) through `--irq-route`.
    The selected route is passed only to the AArch64 probe build, so each
    comparison still uses the audited stock template and `fastboot boot`.
    This makes the Linux-style split between DWC3 event-ring IRQs, Type-C
    threaded work, power events, PDC parents, and SMMU faults reproducible
    from the host without editing build environment variables by hand.

18. The IRQ route matrix was rebuilt locally before another hardware attempt.
    `power`, `typec`, `typec-role`, `pdc`, and `smmu` all passed the QEMU
    preflight, AArch64 probe build, Image.lz4 generation, and Bramble boot
    audit. The `typec` and `typec-role` variants are larger because they carry
    the PMIC role/parent-IRQ path; none of these compile results is evidence
    of real-device enumeration until the host observes `1234:0001`.

19. The SuperSpeed assertion was tightened to resolve the Linux USB Bus/Device
    number for `1234:0001` and match that device's `lsusb -t` node, rather than
    accepting a `5000M` root hub elsewhere on the host. This prevents a false
    SuperSpeed PASS when Fullerene is actually attached at USB2 or absent.

20. A static address audit found that the standalone probe's assembly GIC
    redistributor address was `0x017a6000`, while the Bramble platform contract
    is `0x17a60000`. The missing hexadecimal digit could prevent both the
    physical-timer recovery PPI and USB SPI path from being initialized. The
    probe now derives and loads the full platform `GICR_BASE` value; a new
    compressed-image hardware run is required before attributing any remaining
    failure to PHY, SMMU, or EP0.

21. The gadget-probe handoff now establishes an explicit DMA ownership
    boundary. After the DWC3 device reset has stopped the Fastboot-owned
    controller, it clears and reseeds the Fullerene USB DMA allocator before
    publishing the new event ring, endpoint tables, and TRBs. This prevents
    stale Fastboot DMA contents or allocator state from being reused by the
    probe. The handoff also reapplies the active USB2 Femto-PHY settings after
    that reset, matching the ordering used by the normal platform path. The
    change passed the Fullerene kernel tests, Flasks tests, formatting and
    diff checks, QEMU preflight, AArch64 Image.lz4 generation, and Bramble
    boot-image audit. It has not yet been exercised on a physical Bramble;
    no `1234:0001` enumeration is claimed.

22. The Fastboot gadget probe now connects the existing read-only PM8150B
    Type-C observer to its real entry path. Before DWC3 endpoint setup it
    records the platform-powered state, discovers the Type-C APID, reads the
    current device/host role and cable orientation, and retains that state for
    deferred role polling. It does not rewrite PMIC mode or VBUS registers;
    the optional Type-C IRQ variants continue to use the separate explicit
    role-programming path. A successful observation supplies the
    `Powered -> Attached -> Running` runtime transition used by the normal
    Qualcomm gadget path; a later attach event now performs the same
    `Powered -> Attached` transition during deferred polling. An SPMI read
    failure remains non-fatal for the DWC3 diagnostic. The change passed the
    Fullerene kernel tests, Flasks
    tests, formatting and diff checks, QEMU preflight, AArch64 Image.lz4
    generation, and Bramble boot-image audit. It still has no physical
    `1234:0001` enumeration result.

23. Three post-change physical runs were completed from the same audited
    stock template. The standard USB2 gadget probe (artifact SHA-256
    `ef28c1ee7a356657178c24ed102b2dfe336ed0b5cba31fdc6b37d12fac00b476`)
    passed `fastboot boot`, but produced no `1234:0001`: the host saw a
    temporary `18d1:4ee7` SuperSpeed ADB/charging identity and then the
    bootloader's `18d1:4ee0` again. The no-SMMU differential (artifact SHA-256
    `e974076614e886837ac603eec8d7a989d5045858c1689d3da6d29b577c96a43f`)
    showed the same missing Fullerene identity; after that run ADB confirmed
    stock Android on slot `_b` (`google/bramble/bramble:14/...`) with the
    vendor 4.19 kernel. The SuperSpeed/QMP probe (artifact SHA-256
    `5a04f0a57a1a7da5face8b6ba5a9bd4b233095360485e44a94759055ec5864e1`)
    disconnected the bootloader but produced neither `1234:0001` nor a
    Fastboot/ADB replacement during the recovery window. These runs show that
    SMMU bypass and QMP reinitialization alone do not yet reach the Fullerene
    descriptor path. The harness now records the `18d1:4ee7` fallback
    immediately, including its USB descriptor and read-only ADB state, instead
    of misclassifying it as an unexplained timeout. The later Type-C IRQ
    comparison exposed a timing gap in that detector, which is now closed for
    both the initial enumeration window and watchdog-recovery window.

24. The Type-C IRQ comparison was then run from a manually restored Fastboot
    screen with `--irq-route typec`, using the same no-flash `fastboot boot`
    path. Its audited artifact SHA-256 was
    `75a8d7d85bd212da8cea6f0c94d47ab57ef86dcd8f7d00be74cf992dd2b77f2d`.
    The image was accepted and the bootloader disappeared, but the Fullerene
    identity never appeared. Host USB logs showed the same sequence as the
    standard probe: `18d1:4ee7` at SuperSpeed after about 40 seconds, followed
    by `18d1:4ee0` after the recovery/reset path. Thus adding the explicit
    PMIC Type-C role programming and Type-C parent IRQ did not yet move the
    failure into the Fullerene descriptor/EP0 path. The harness was updated
    to classify a late `18d1:4ee7` as stock Android fallback even when it
    appears during the 75-second recovery window.

25. The PDC IRQ comparison was run next from Fastboot, enabling the Android
    DT's DP-HS, SuperSpeed, and DM-HS PDC parent routes (`--irq-route pdc`).
    Its audited artifact SHA-256 was
    `d8a7392963666fa9ab08a1c8b1dd92af710fe76f11c4956031261ec41e2204d9`.
    The image was accepted and the bootloader disconnected, but no
    `1234:0001` device appeared. The host observed `18d1:4ee7` as a SuperSpeed
    ADB-capable stock fallback, and read-only ADB reported slot `_b`, the
    Google Android 14 fingerprint, and the vendor 4.19 kernel. This places
    the PDC route below the current missing boundary: adding the documented
    auxiliary PHY IRQs still does not make the Fullerene EP0 device visible.

26. A valid `--no-core-reset` A/B was then repeated after registering its
    Cargo environment variable with `rerun-if-env-changed` (the earlier run
    before that registration is discarded as an invalid comparison). The
    rebuilt artifact SHA-256 was
    `62235fedcdd82bdcc3cd0c1f6d3352ecfb1ed23e4b54c2c15b5deba6328adda1`.
    Omitting DWC3 CSFTRST while retaining the halted-controller boundary did
    not produce `1234:0001`; the host again saw `18d1:4ee7` and ADB confirmed
    the same stock Android 14/vendor-4.19 fallback. This rules out the core
    reset alone as the sufficient cause of the current missing attach and
    shifts the next probe toward the pre-EP0 DWC3/PHY state, with retained
    trace retrieval still required to identify the exact failing stage.

27. The next comparison implemented the Android/Linux DWC3 core-global setup
    at the post-reset boundary. The probe now clears inherited `SCALEDOWN` and
    `DISSCRAMBLE` state, preserves the Lito DT's `disable-clk-gating` setting,
    and sets `U2RSTECN` only when the runtime `GSNPSID` identifies a DWC3
    revision older than 1.90a. The revision-dependent write is recorded in
    the retained USB trace and is not applied to an unrecognised core. The
    change passed formatting, diff, shell syntax, 91 Fullerene kernel tests,
    QEMU preflight, Image.lz4 generation, and Bramble boot-image audit. Two
    physical `fastboot boot` runs from Fastboot accepted the audited image but
    still produced no `1234:0001`; both fell back to stock Android
    (`bootreason=watchdog`, `18d1:4ee7`) after the bootloader disconnect. This
    does not advance the observed boundary into EP0 and leaves retained-trace
    retrieval as the next diagnostic requirement.

28. The same revision was exercised with the separate `--usb-pullup-probe`,
    which intentionally omits EP0/DMA setup and tests only the physical
    reconnect path. Its compressed Bramble artifact passed QEMU preflight and
    the boot-image audit, and `fastboot boot` was accepted, but the host saw
    no new USB attach at all during the 70-second observation window. The
    handset did not return as either Fastboot or ADB afterward, so this probe
    required manual recovery to the red-triangle Fastboot screen. This is a
    stronger pre-EP0 failure signal than the gadget probe's stock Android
    fallback; it does not indicate a descriptor failure.

29. That pullup-only run also exposed a harness bug: successful pullup
    initialization stopped the assembly recovery timer before entering its
    intentionally indefinite polling loop. If the physical handoff then
    produced no attach, the temporary image could strand the handset instead
    of returning it to Fastboot. The timer is now kept armed for
    `--usb-pullup-probe`; the gadget-handoff probe retains its existing EP0
    progress gate, while unrelated modes keep their prior behavior. The
    harness now exposes this exact diagnostic as `--pullup-only`, so its
    build/audit/boot/log path is reproducible without a hand-written command.
    The rebuilt local artifact is
    `/tmp/fullerene-bramble-pullup-watchdog.img` (SHA-256
    `7d936adf8459545e26ff981f0c19464ee484b4b1dac643f79997525fa87497eb`).
    Formatting, diff checks, Flasks tests, and all 91 Fullerene kernel tests
    pass after this change; the physical rerun is pending a host-visible
    Fastboot device.

30. The corrected `--pullup-only` path was then exercised from a host-visible
    Fastboot session. The audited compressed image was accepted by `fastboot
    boot` (artifact SHA-256 `7d936adf8459545e26ff981f0c19464ee484b4b1dac643f79997525fa87497eb`),
    and the host recorded the expected `18d1:4ee0` disconnect at 12:35:53,
    but no `1234:0001`, Android fallback, or Fastboot device returned during
    the 75-second recovery window. Thus the newly retained IRQ watchdog was
    not observable on this hardware run. A second recovery layer now checks
    `CNTPCT_EL0` from the pullup polling loop and invokes the same reset path
    after 60 seconds; its rebuilt artifact is
    `/tmp/fullerene-bramble-pullup-pollwatchdog.img` (SHA-256
    `27cae9838b41340c8f6f59a0feed62bc31680b3186f5366ac103cd55dc4d9413`).
    The latter has passed QEMU preflight, the Bramble boot audit, formatting,
    diff checks, Flasks tests, and all 91 Fullerene kernel tests, but still
    needs a fresh physical run after manual Fastboot recovery.

31. The retained trace now has a host-visible path when EP0 does enumerate.
    The device descriptor advertises serial-string index 3; the hardware EP0
    path fills that UTF-16 string as `FUTR-<trace-head>-<last-event>`, and the
    harness records the value alongside `lsusb -v`. This does not replace the
    raw 256-entry trace or solve a pre-attach failure, but it makes the first
    committed EP0 boundary observable without UART and keeps the diagnostic
    in the same automated enumeration loop. The retained-record reader now
    validates the `FUTR` magic and trace version before exposing a cursor, so
    stale or uninitialised RAM cannot masquerade as evidence. The protocol
    regression test passes with 92 Fullerene kernel tests total; the rebuilt
    pullup/trace artifact passed QEMU preflight and the Bramble boot audit
    (`kernel=56341`, SHA-256
    `a19d6e3d8ae00cce850e215236737d376dcd990505d3eb2b034d9e0cc2783637`).
    The host-side trace string still needs a physical gadget-enumeration run;
    the current host session is not exposing the handset as Fastboot.

32. The pullup-only recovery loop was corrected to remain strictly a physical
    probe. It no longer calls `usb::poll()`, because that would read and
    potentially consume stale Fastboot event-ring state even though this mode
    intentionally never owns DWC3 events, TRBs, DMA, or SMMU state. Recovery
    now uses only the architectural counter and the existing reset deadline.
    The rebuilt artifact passed QEMU preflight and the Bramble boot audit
    (`kernel=56341`, SHA-256
    `511e25847cf9c59677106aae8111a0c6ce1c82715ee5c6dbfadecbe317f2b67d`).
    The first physical run of this corrected version is pending a host-visible
    Fastboot USB device.

33. The USB2-only Qualcomm UTMI/PIPE clock handoff was tightened to match the
    upstream glue's timing contract. The three register stages remain in the
    same order, but each transition now uses the architectural timer for a
    100-us minimum delay; the previous 100,000-NOP approximation could become
    shorter than Linux's `usleep_range(100, 1000)` on a fast boot CPU. The
    rebuilt pullup-only image passed QEMU preflight and the Bramble boot audit
    (`kernel=60454`, SHA-256
    `2afbd10454892e1c566fbfab3153d9b943c6ff087420b79c63a4c517d90a5d0c`).
    The normal `--usb-gadget-handoff-probe` variant was rebuilt with the same
    change and also passed QEMU preflight and the Bramble boot audit (`kernel=64566`,
    SHA-256 `2e7262410c4cb8e398f0f5d0aae8d3bbe1fd54f664e2b52c2db63d98667dd350`).
    Kernel/Flasks tests, formatting, and diff checks pass. A physical run is
    still pending because the manually selected Fastboot screen is currently
    not visible to the host as `18d1:4ee0`.

34. The DWC3 reset timing was also made frequency-independent. The 50-ms
    post-`CSFTRST` handoff delay and the 100-ms PHY-reset release delay (plus
    the 1-ms settling delay) now use the architectural counter instead of
    fixed NOP counts. This keeps the Android/Linux reset timing contract
    stable across CPU frequencies. The rebuilt pullup-only artifact passed
    QEMU preflight and the Bramble boot audit (`kernel=60454`, SHA-256
    `38de47eef40282f7fee68ff99af5f32e4ff00ca45f69da1978ae60026bfa9da5`).
    Kernel/Flasks tests, formatting, and diff checks pass. No physical boot
    was issued because the host still cannot see the manually selected
    Fastboot device.

35. The DWC3 Run/Stop handoff wait was aligned with the upstream gadget path.
    Instead of CPU-dependent NOP counts, both the halt-before-configuration
    and running-after-connect checks now poll `DSTS.DEVCTRLHLT` every 1 ms for
    up to 2 seconds, matching Linux's 2,000-iteration timeout. This prevents
    a fast CPU from issuing `DEPSTARTCFG` or EP0 traffic while the controller
    is still draining the Fastboot session. Pullup-only and normal
    gadget-handoff images both passed QEMU preflight and the Bramble boot
    audit: pullup SHA-256
    `a071eef2a404cad4f97c0fab44bd5914b13f006aa8d3a45661783d9c1758ee48`,
    normal gadget SHA-256
    `0565b8571ec24a62769928f4676daae7f0bae85cd48c5dc2a8e4dbeb7d3f2e4f`.
    Kernel/Flasks tests, formatting, and diff checks pass. Physical execution
    remains pending host-visible Fastboot USB.

36. The explicit SuperSpeed handoff variant was rebuilt after the timing
    changes. It uses the QMP combo-PHY path, the same DWC3 Run/Stop wait, the
    Apps-SMMU/GSI setup, and the retained EP0 trace, and passed QEMU preflight
    plus the Bramble boot audit (`kernel=64566`, SHA-256
    `b4133e9012c4718783531d3b60a38805bb666a362cf0e02e7c0f6e24d1a0f2ae`).
    It is ready for the next host-visible Fastboot run; no physical boot has
    been issued in this state because `fastboot devices` remains empty.

37. The non-endpoint portion of Linux's `__dwc3_gadget_start()` was added to
    the normal Fullerene gadget paths. After the DCFG speed/address fields are
    selected, the handoff now clears the correct `GRXTHRCFG.PKTCNTSEL` bit for
    the detected DWC3 IP, derives `DCFG.NUMP` from `GHWPARAMS0.MDWIDTH` and
    `GHWPARAMS7.RAM2_DEPTH`, caps it at 16, and sets `DCFG.IGNSTRMPP`.
    Bramble's `0x5533...` controller therefore uses the legacy DWC3 threshold
    bit; DWC_usb31/32 IDs use their separate threshold bit. The calculation is
    saturating so invalid or unexpectedly small hardware parameters cannot
    wrap into a large NUMP value. Pullup-only mode remains unchanged and does
    not read or modify these gadget-controller registers. The pure calculation
    regression is covered by the 92 Fullerene kernel tests. Both updated
    artifacts passed QEMU preflight and the Bramble boot audit:
    USB2 gadget SHA-256
    `0b4619e7cc08a9f8d5102d5ebb1f089cbbf121a4848ab7c75a6c2b4e57bb19e7`,
    SuperSpeed gadget SHA-256
    `0c9ce4f2c8422ab5b75dbaaca104de6941b34e62ba477645892f7c50b73881fa`.
    The host-side check still found no Fastboot USB device, so neither image
    was sent to the phone.

38. The DWC3 device-event mask was brought closer to Linux's gadget start
    path. The three gadget paths now also enable erratic-error, command-
    complete, and event-overflow notifications (DEVTEN bits 9--11), while the
    existing event consumer records them in the retained trace as controller
    errors. This makes an event-ring/controller failure distinguishable from
    an EP0 descriptor failure when the host sees the device. USB2 gadget,
    SuperSpeed gadget, and normal Bramble images all passed QEMU preflight and
    the Bramble boot audit: `kernel=64566` for both probe variants and
    `kernel=93438` for the normal image. Their SHA-256 values are respectively
    `1a4bcb29afe4eb9f113fc150803a93f1505c799d4ca2cef4ba8ec5be6e52513f`,
    `8b8a2d518af83de2319f243a9921214e260325e263e91f2f24fadbd640f3f4fb`, and
    `4e6a70c43a751e461d273e7afd036280525765517736f880eb771d6d1b5d3b30`.
    Kernel tests (93), Flasks tests (17+3), formatting, and diff checks pass.
    No physical boot was issued because the host still exposes no Fastboot
    device.

39. The event-ring IRQ boundary was aligned with Linux's event-buffer handler.
    When `GEVNTCOUNT` reports entries, Fullerene now sets
    `GEVNTSIZ.INTMASK` before consuming them, acknowledges the consumed count,
    and only then clears the mask. This prevents an IRQ re-entry from racing
    the retained-ring cursor and event acknowledgement while preserving the
    existing polling fallback. The corresponding DWC3 mask/count constants
    have regression tests. USB2 gadget, SuperSpeed gadget, and normal Bramble
    images passed QEMU preflight and the Bramble boot audit; their SHA-256
    values are `c94cc35dc1e09fa96ee4bab4a3201f851f1408ae134c29ca837611dd50c34162`,
    `541d87897c764b40d179cb4366ef44440bc2d3562cabee3b639688371f73bf2f`, and
    `91bac411aeb2f04f0994cccf8882759ec5cb0c39a18e7576cde88d3534250e19`.
    Kernel tests (94), Flasks tests (17+3), formatting, and diff checks pass.
    No physical boot was issued because the host still exposes no Fastboot
    device.

40. DWC3 reset and runtime-PM waits were made frequency-independent and
    generation-aware. `CSFTRST` now follows Linux's 1-us polling for the
    legacy DWC3 IP used by Bramble, while DWC_usb31 1.90a+ and DWC_usb32 use
    the 20-ms cadence; the legacy 50-ms post-reset delay is applied only to
    DWC_usb31 1.80a and earlier. Runtime suspend/resume now reuses the same
    2-second `DSTS.DEVCTRLHLT` Run/Stop wait and releases the GSI doorbell on
    a failed suspend transition. This removes the remaining CPU-speed-
    dependent reset/runtime loops without changing the Type-C or PHY reset
    ordering. USB2 gadget, SuperSpeed gadget, and normal Bramble images all
    passed QEMU preflight and the Bramble boot audit; their SHA-256 values are
    `e4e3c75742c5135dee0073529174b53821a1cb4f1ca000468b3671bd185d0f1a`,
    `ebc47a9543398db9c47f2f2e78fb3f944d791e45d80e40c97f32ba25a8de29ac`, and
    `7f8f69e9bd853c5f8864a7a435c6f59f2a02b9ee70a5187806443488beae03db`.
    Kernel tests (94), Flasks tests (17+3), formatting, and diff checks pass.
    No physical boot was issued because the host still exposes no Fastboot
    device.

41. The reset/runtime implementation was then tightened against the exact
    Linux generation split, and endpoint-command polling was bounded in the
    same units as Linux. Legacy DWC3 keeps the 1-us CSFTRST cadence; DWC_usb31
    1.90a+ and DWC_usb32 use the 20-ms cadence; only DWC_usb31 <=1.80a gets
    the additional 50-ms settling delay. Run/Stop transitions use the
    existing device-halted/running wait helpers, and a failed suspend releases
    the GSI doorbell before returning. `send_ep_command_result()` now uses a
    constant 5000-iteration command-completion bound instead of a
    CPU-frequency-dependent delay. USB2 gadget, SuperSpeed gadget, and normal
    Bramble images all passed QEMU preflight and the Bramble boot audit; their
    SHA-256 values are `a4106670763f16f146032bde99bf96a4cbab1d4d0d66a453f5740e9e195c3e17`,
    `9b2d13b0c6dbb92fbe8918d2d1a9fa8a09fe83954968ec36edd00ce6f83363ba`, and
    `95d9fa46e84f2faa204845e87aeb4458e50fe1218b234ca6e3dabdb9c20f2c65`.
    Kernel tests (94), Flasks tests (17+3), formatting, and diff checks pass.
    No physical boot was issued because the host still exposes no Fastboot
    device.

42. The remaining Run/Stop boundary was aligned with Linux's
    `dwc3_gadget_run_stop()`: USB2 `SUSPHY` and `ENBLSLPM` are saved and
    cleared before every controller start/stop, `DSTS.DEVCTRLHLT` is polled,
    and the saved PHY low-power state is restored afterward. The common helper
    now covers the USB2 pull-up probe, EP0 handoff, normal gadget start, and
    runtime suspend/resume paths. This closes a platform-level race that was
    previously handled only inside endpoint-command submission. The new
    artifacts passed QEMU preflight and the Bramble boot audit; their SHA-256
    values are `d88ca31c3c6235201275d9699593a367beb24a25b56809f085c5553bd520b221`,
    `590c9633dd6aafa3813a36f0d1403b02534be8bb85cd041b24ca95243313aebf`, and
    `75d9c7744d3e859244f62a650aa5bcdf3f80cae5b725b2b57f36dcaa6b0612db`.
    Kernel tests (94), Flasks tests (17+3), formatting, diff checks, and the
    USB loop script syntax check pass. No physical boot was issued because
    the host still exposes no Fastboot device.

43. The Type-C parent-IRQ path was corrected to match the Linux threaded
    role-switch boundary. Deferred handling now rereads PM8150B role and
    orientation, applies attach/detach/host transitions to the DWC3 session,
    and only then acknowledges the SPMI child and parent summary. Type-C
    detach/host transitions also revoke data transfers before clearing
    Run/Stop. The new USB2 gadget, SuperSpeed gadget, and normal Bramble
    artifacts passed QEMU preflight and the Bramble boot audit; their SHA-256
    values are `bc9b46ecb733056f0412c650fe9cebb4429432d2451dfbf8bc38da6f7390d25b`,
    `f9757f421628e6109dd7fb256d0ee5698a47daff60927ada20aa4ac040d35bd9`, and
    `1426bb3600a12248d1ad8eaed39d1077427dff72f6008833a1c1251bc4a37d61`.
    Kernel tests (94), Flasks tests (17+3), formatting, diff checks, and the
    USB loop script syntax check pass. No physical boot was issued because
    the host still exposes no Fastboot device.

44. A five-way IRQ-route differential was built from the Type-C and Run/Stop
    changes so the next physical attempt can compare platform delivery without
    changing the kernel implementation. The `power`, `typec`, `typec-role`,
    `pdc`, and `smmu` variants all passed QEMU preflight, AArch64 `Image.lz4`
    audit, and the Bramble boot-image audit. Their SHA-256 values are
    respectively `bd3bcd03417c43ec483ad43def3bc724f9ec9bb301492b0644732e09b4197b5b`,
    `db5a53803d6994cebecf08cc1dc161687a2633bbd1527bc11d5dcb42daf4e82d`,
    `bf5fa3db9b5a499367eba5d13f7336e8de8b0df19870548d1e5617ffc7f22583`,
    `e4596c46e6232ca0d9c7af940798883e92c1fe1e46a7baaa0cbd8241edffc92c`, and
    `855962042777106e37dd67236e29487dcfb63fe217587682aa21729e35bd99f2`.
    The host-side check after manually returning the phone to Fastboot still
    found no `18d1:4ee0` bootloader device and no `1234:0001` Fullerene device;
    consequently no physical boot, flash, or erase operation was performed.

45. The Rust `bramble-usb matrix` command now wraps the single-attempt loop and tries
    the bounded `power`, `typec`, `typec-role`, `pdc`, and `smmu` routes in
    sequence. It advances only after the probe's own watchdog has returned the
    handset to host-visible Fastboot, stops on the first successful gadget, and
    performs no reboot, flash, or erase. `--dry-run` was verified for route
    selection and both scripts pass `bash -n`; the matrix remains pending a
    host-visible Fastboot device.

46. USB descriptors are now speed-profiled instead of being enabled for every
    gadget probe. USB2 handoff builds advertise USB 2.0 with a 64-byte EP0 and
    a USB 2.0-only BOS; the SuperSpeed handoff build advertises USB 3.0 with a
    512-byte EP0, SS endpoint companions, and the SuperSpeed BOS capability.
    This matches the Linux gadget split between USB2 and SuperSpeed descriptor
    tables and prevents a USB2-only diagnostic from falsely claiming SS. The
    USB2 and SuperSpeed artifacts both passed QEMU preflight and the Bramble
    boot audit; their SHA-256 values are
    `a5f1511ecb787e6c6db6188fb79a2dab8d94cb94237c2b4815eb7f8fbc51c9f4` and
    `b8906dd76fec64ade01019c2f1fd29cd9244ef4487f6776b60aa5732c03f201d`.
    Kernel tests (94), Flasks tests (17+3), formatting, and diff checks pass.
    Physical execution remains pending host-visible Fastboot.

47. The retained USB trace is now available through a vendor IN control
    request (`bmRequestType=0xc0`, `bRequest=0x5a`) in addition to the short
    descriptor status string. Each 512-byte page contains a 16-byte little-
    endian header followed by up to fifteen 32-byte records, allowing the
    host to recover the post-mortem EP0 sequence without UART or a physical
    RAM reader. The Rust `bramble-usb trace` command reads and decodes those
    pages when `1234:0001` is visible. The USB2 and SuperSpeed trace-enabled artifacts passed QEMU
    preflight and the Bramble boot audit; their SHA-256 values are
    `fdb9aff5737dadb9cbccd659fffbf3ab0c026e01ab4b0950c82fc72806abd77a` and
    `013df6d82546e5ba0935cee9cf81a299ab1ac3e79d10fb039e4285e052a94140`.
    Kernel tests (94), Flasks tests (17+3), formatting, and diff checks pass.
    The USB2 physical run was accepted by Fastboot but fell back to Android
    (`18d1:4ee7`); the SuperSpeed run reset back through Fastboot without
    producing `1234:0001`. No flash or erase operation was performed.

48. The host experiment harness was moved from untracked shell files into the
    tracked Rust binary `flasks/src/bin/bramble-usb.rs`. `loop` preserves the
    build/audit/`fastboot boot`/identity-transition/hold workflow, while
    `matrix` preserves the bounded `power`, `typec`, `typec-role`, `pdc`, and
    `smmu` route sequence. The removed shell wrappers are no longer required;
    dry-run loop and matrix selection both pass, and `cargo check -p flasks
    --bin bramble-usb` passes. The current hardware evidence remains the
    failed USB2 and SuperSpeed transitions recorded above.

49. The Rust harness was exercised on the host-visible Bramble Fastboot
    device (`26191JECB00076`) after the migration. It captured `getvar all`,
    built the compressed USB2 probe with QEMU preflight and the Bramble boot
    audit, and sent it with `fastboot boot`; artifact SHA-256 was
    `fdb9aff5737dadb9cbccd659fffbf3ab0c026e01ab4b0950c82fc72806abd77a`.
    The bootloader disconnected, no `1234:0001` device appeared, and the
    phone returned as ADB-capable stock Android `18d1:4ee7`. The complete
    run directory is `/tmp/fullerene-bramble-loop.779353.0`; no flash or
    erase operation was used.

50. The handoff probe was changed so a stale pre-reset `DSTS.DEVCTRLHLT`
    readback is retained in the USB trace but does not prevent the subsequent
    DWC3 device reset. The new artifact passed QEMU preflight and the Bramble
    boot audit, then was RAM-booted with SHA-256
    `b1d808297f0cba4fa16a8e55027d865cf4ef1e5d168104abd576b6f779df735b`.
    It still produced no `1234:0001` and returned to stock Android; logs are
    under `/tmp/fullerene-bramble-loop.787803.0`.

51. Two non-destructive differentials were run from the same Fastboot state:
    `--no-core-reset` (artifact
    `1bdca2d8f0404b6a2a5a5ef19ae04518dcda564052dc8e4e291abfea746454c4`,
    `/tmp/fullerene-bramble-loop.791105.0`) and `--no-smmu` (artifact
    `b6ea6dc678e4d24f0d92edbb0f576bb89a1fb8b00264f909c15c5f17ba555684`,
    `/tmp/fullerene-bramble-loop.792840.0`). Both passed image audits and
    returned to `18d1:4ee7` without producing the Fullerene identity. This
    rules out either CSFTRST alone or the newly installed SMMU mapping as a
    sufficient explanation.

52. The final Run/Stop status readback was made non-fatal after EP0 resources
    and the first SETUP TRB are queued, and the Rust harness gained a
    `--bare-pullup` mode. The full probe artifact
    `43ea6f57d4a54067f2d4d36c396d564514bda5f5ff0e7554f5adcae7d1ce4ebe`
    (`/tmp/fullerene-bramble-loop.795888.0`) still did not enumerate. The
    bare physical artifact
    `43cd2104007e20c245bedfb7516cfe3dea6eb51818cb6f17aba0c5199f7793d7`
    (`/tmp/fullerene-bramble-loop.799151.0`) produced the decisive host log:
    USB2 attach followed by `device descriptor read/64, error -110`, then
    stock Android. Thus the PHY/session/pull-up boundary is alive while the
    full EP0 handoff fails before a host-visible device identity.

53. Linux's `dwc3_hs_phy_setup()` comparison showed that the reset path was
    missing the DWC3-side UTMI interface and turnaround timing programming.
    Fullerene now reapplies UTMI 8-bit and `USBTRDTIM=9` after controller
    setup. The resulting standard artifact
    `0341b4b264f374d08bba9e67c3a4f233153b53b6cd1c27f8a0a1f600fbf82b11`
    (`/tmp/fullerene-bramble-loop.805374.0`) passed all local gates but still
    returned to `18d1:4ee7`; this difference is implemented but not yet
    sufficient on Bramble.

54. The Rust matrix runner now restores Fastboot between failed cases by
    issuing only `adb reboot bootloader` when Android fallback is available.
    No partition flash or erase is used. Current local validation remains
    kernel tests 94, Flasks tests 17+3 plus the Rust USB binary tests 2,
    formatting, and `git diff --check`.

55. The Rust stage probes narrowed the physical handoff boundary without
    requiring UART. With Apps-SMMU programming disabled, stage 4
    (`DEPSTARTCFG`) and stage 9 (EP0 OUT `SETEPCONFIG`) both produced the
    expected USB2 attach followed by host `device descriptor read/64,
    error -110`; their artifacts were
    `6ae41f3b757bc4be1eb72a2273889315975403cf66e469fa0f0018d5f7e640d1`
    (`/tmp/fullerene-bramble-loop.846166.0`) and
    `848f6b5bdbc8c6dcd1956aae77489f927ab916012ab9518f339bf75dad83b6d8`
    (`/tmp/fullerene-bramble-loop.867662.0`). This confirms that physical
    attach survives through `DEPSTARTCFG` and the first EP0 configuration;
    it does not yet prove that an EP0 transfer completed.

56. Stage 10, which stops immediately after EP0 OUT
    `SETTRANSFRESOURCE`, also reached the physical pull-up path and produced
    the same descriptor timeout. Its artifact was
    `ffdb5048cdd6026934f77165f60bca6f22a4f5f0990a2f8df0990f8355a9ca78`
    (`/tmp/fullerene-bramble-loop.869459.0`). Because the stage-10 marker is
    after the command and the probe otherwise fails closed, this is evidence
    that the command completed well enough for the stage probe; the remaining
    failure is later than this resource command or in the normal SETUP/event
    path.

57. The SMMU-preserving normal probe was repeated after correcting the DWC3
    `DEPCMDPAR0/1/2` offsets and adding the Linux-style ordering/barrier. The
    artifact `6f95de2b1a57f37c50b3f9094dde87b0bcd5d458943a0ea3e63914c3215280c5`
    (`/tmp/fullerene-bramble-loop.880872.0`) still produced USB2 attach and
    `device descriptor read/64, error -110`, then stock Android
    `18d1:4ee7`. The corrected endpoint-command path therefore did not by
    itself resolve EP0 enumeration.

58. Two resource-order differentials were tested against the Android msm
    implementation: skipping `SETTRANSFRESOURCE` entirely produced no
    Fullerene USB2 attach (`2c3b43671d08da75aa700885cf2063361eb486bb0b96d98613cad33e157f99f2`,
    `/tmp/fullerene-bramble-loop.875845.0`), and allocating resources for all
    hardware endpoints before `SETEPCONFIG` also produced no Fullerene attach
    (`0100af2d104d884032a3062ca895faeb73cdb59ebf104c889074bec08fecd0fa`,
    `/tmp/fullerene-bramble-loop.891092.0`). Neither differential is a fix;
    the latter also shows that copying Android's resource ordering alone is
    insufficient.

59. The current evidence separates the surviving boundary from the missing
    one: Fastboot-to-USB2 PHY/session pull-up, DWC3 reset, `DEPSTARTCFG`, EP0
    OUT configuration, and EP0 OUT transfer-resource allocation can each be
    reached in a controlled probe. A complete Fullerene identity has still
    not appeared. The next Rust-only probe is therefore the boundary after
    `start_setup()` and before/after Run/Stop, followed by retained-trace
    correlation if the custom identity becomes readable; descriptor contents
    remain a lower priority than SETUP TRB DMA, event-ring ownership, and
    SMMU/IOVA visibility.

60. The shell-free Rust harness gained `--reuse-fastboot-dma` as a focused
    ownership differential. It captures the DWC3 event page still selected by
    Fastboot and places the event ring, SETUP buffer, EP0 TRB, and response
    buffer inside that page. A complete run
    (`/tmp/fullerene-bramble-loop.933221.0`, artifact
    `3a8e664777ee1c558ee1bce9b9b70f9827c80810fe45a7c25587829156e6aede`)
    still produced no Fullerene USB2 identity and returned to stock Android.
    The stage-5 differential
    (`/tmp/fullerene-bramble-loop.935532.0`) did preserve the physical USB2
    attach before the expected descriptor timeout, showing that reusing the
    firmware-selected page is viable through endpoint configuration/resource
    setup. The stage-6 run
    (`/tmp/fullerene-bramble-loop.940511.0`) produced no USB2 attach after
    the first EP0 `STARTTRANSFER` boundary. Stage-probe failure handling was
    then corrected so stages 6 and later no longer re-run the bare pull-up
    initializer; they repeat only the Run/Stop boundary. This keeps the next
    result attributable to EP0 STARTTRANSFER/DMA/event ownership rather than
    to a second reset-like probe path. The latest attempted rerun did not
    reach `fastboot boot` because the handset was no longer visible as
    Fastboot, so it yielded no additional hardware evidence.

61. The handoff boundary was split further in Rust. Stage 11 stops after the
    SETUP buffer/TRB has been written and cache-cleaned but before issuing
    `STARTTRANSFER`; `/tmp/fullerene-bramble-loop.959826.0` produced USB2
    attach followed by the expected descriptor timeout. Stage 12 stops after
    `STARTTRANSFER` returns and before the final Run/Stop transition;
    `/tmp/fullerene-bramble-loop.968987.0` produced no Fullerene USB2 attach
    and returned to stock Android `18d1:4ee7`. Both runs passed the QEMU
    preflight, Image/LZ4 and boot-image audits, and used only `fastboot boot`.
    This is the strongest current hardware localization: publishing the TRB
    is survivable, while arming it through DWC3 `STARTTRANSFER` either fails
    its command/DMA acceptance or leaves the controller unable to advertise
    the USB2 pull-up. The next implementation work should expose and correct
    that command/DMA contract before changing descriptors or higher-level
    gadget callbacks.

62. The official Linux ordering was applied one step further in the Rust
    handoff. Linux begins consuming the DWC3 event ring immediately after
    `dwc3_ep0_out_start()` and before restoring USB2 low-power handling and
    asserting Run/Stop. Fullerene now performs the equivalent bounded EP0-only
    event-ring drain after the initial SETUP `STARTTRANSFER`, without invoking
    Type-C, power, or SMMU service from that diagnostic boundary. The build and
    QEMU protocol self-test remained passing.

    The corresponding stage-12 run
    (`/tmp/fullerene-bramble-loop.1010949.0`) still did not expose the
    Fullerene `1234:0001` identity. The host saw Fastboot `18d1:4ee0`
    disconnect and then the automatic stock Android fallback
    `18d1:4ee7`; no custom descriptor was readable. Therefore the one-shot
    event drain is not sufficient to resolve the real-device failure. The
    surviving evidence remains: stage 11 reaches the physical USB2 attach
    and times out at EP0, while the stage-12 boundary after arming the first
    SETUP transfer still needs a more direct command/DMA/event result.

63. The official register comparison found and corrected a concrete DWC3
    platform-register mistake: Fullerene had treated `0xc360` as `GUCTL1`,
    while the DWC3 global register map places `GUCTL1` at `0xc11c`. The former
    address is in the FIFO-register area. The same comparison corrected EP0
    event-buffer initialization to acknowledge the current `GEVNTCOUNT` value
    by writing it back, matching Linux's `dwc3_event_buffers_setup()`;
    writing zero does not consume stale events. Both changes are Rust-only and
    pass the local test suite.

    The first hardware run after the `GUCTL1` correction
    (`/tmp/fullerene-bramble-loop.1034451.0`) still exposed no Fullerene
    identity. The next run with event-count write-back
    (`/tmp/fullerene-bramble-loop.1038246.0`) had the same result. Finally,
    the probe-only post-`STARTTRANSFER` `SUSPHY` restore was removed to match
    the Android downstream Bramble path rather than mainline's later
    `dwc3_enable_susphy(true)` boundary; `/tmp/fullerene-bramble-loop.1041875.0`
    also showed no Fullerene attach and recovered to Android `18d1:4ee7`.
    These three official-order differences are therefore implemented or
    experimentally rejected as the immediate cause. The reproducible boundary
    remains the first DWC3 `STARTTRANSFER`/TRB-DMA transition, before a usable
    USB2 identity appears.

64. Linux's `dwc3_gadget_dctl_write_safe()` comparison found one more missing
    handoff detail. Fullerene now clears the DCTL link-state request field for
    both the Fastboot-to-Fullerene `CSFTRST` write and the Run/Stop write,
    preventing a stale bootloader link-state command from being carried into
    the new device session. The resulting stage-12 artifact
    (`/tmp/fullerene-bramble-loop.1047509.0`) still produced no Fullerene
    attach and recovered to Android `18d1:4ee7`. This closes the DCTL safety
    difference without changing the current hardware localization: the
    surviving USB2 attach is still before the first `STARTTRANSFER`/TRB fetch.

65. The 2026-08-30 session first re-calibrated the host side. `adb reboot`
    returns the handset to a host-visible Android device after 19 s and
    reports `bootreason=reboot`; every probe run instead reported
    `bootreason=watchdog`, so the reset that ends each attempt is a real
    unpettable bite at roughly attach+7-8 s (kernel entry +10 s), matching the
    previously unidentified biter. The bite-to-Android time (19 s) turns the
    journal timestamps into a coarse clock: Android always re-appeared at
    attach+26-29 s across all runs below. The signal-gate readout was
    re-verified dead in the current build: `--signal-cmd-gate always`
    (unconditionally true), `setup`, and the guaranteed-false hex gate all
    returned at 37-39 s with identical journal shape, because the TRUE path's
    `park_for_seconds(0)` PSCI reset and the FALSE path's 90 s park are both
    cut by the same unpettable bite, and neither the DCTL Run/Stop stop nor
    the QSCRATCH drop publishes a host-visible disconnect. The sixth-session
    gate "negatives" therefore cannot distinguish not-evaluated from false.
    All runs remained non-destructive `fastboot boot`; no flash or erase was
    used.

66. The EP0 data/status Start Transfer retry window was extended from
    50x200 us (~35 ms) to a 2.5 s time-based window
    (`retry_start_transfer`), on the documented observation that the host
    keeps issuing IN tokens and tolerates the NAKs until its own 5 s control
    timeout, so a late data-phase arm would still complete the first
    descriptor read inside the window. The artifact (SHA-256 prefix
    `bdcf882eb4ff2f9b`) still produced the standard HS attach, first
    descriptor `error -110`, and the watchdog recovery to Android. The same
    window with the msm-4.19 DWC_usb31 revocation contract repeated between
    attempts - `ENDTRANSFER(CMDIOC|HIPRI_FORCERM)`, the 100 us settle, and a
    fresh `SETTRANSFRESOURCE` - also produced no change (SHA-256 prefix
    `b3eec7bfde2563a2`). This bounds the "zombie transfer owns the EP1
    resource" explanation together with the pure command-wedge explanation.

67. A new gated differential `--usb-gadget-handoff-ep0-stall-flush` added the
    Linux `dwc3_ep0_stall_and_restart()` EP0 `SETSTALL` flush before the
    direct Run/Stop boundary. With the flush bundled with a pre-Run/Stop
    halted-boundary SETUP arm, the physical attach disappeared entirely
    (SHA-256 prefix `5cda236be121ec2b`), reproducing the documented
    pre-link-ON Start Transfer wedge even after the flush. With the flush
    alone the attach returned and the first descriptor read still timed out
    (SHA-256 prefix `d6c882c57142f3cc`). The flush-only form is kept as the
    default of that cfg; the halted-boundary arm was removed from the cfg.
    A no-`--u0-arm-probe` control (SHA-256 prefix `b8aa49222ed74446`) ruled
    out the post-failure destructive endpoint rebuild as the cause of the
    -110.

68. Re-reading the sixteenth-session evidence with the direction split in
    mind: the EP0 OUT (SETUP) Start Transfer provably succeeds - the arm
    lands at U0 and the host's SETUP is ACKed - while only the EP0 IN
    (data/status) direction fails. With 16-bit `PHYIF=1` the data phase
    demonstrably reached the wire (the host reported `-71` protocol errors
    instead of a timeout), so the data-phase arm itself is capable of
    publishing a transfer; in 8-bit UTMI the payload never completes a
    host-valid packet. `lito-usb.dtsi` does not set
    `snps,phyif-utmi-16-bit`, so 8-bit remains the reference width and the
    16-bit `-71` readings are wrong-width garbage rather than a better
    baseline. The next lever is therefore EP1-specific state at the first
    data-phase arm (its endpoint context, FIFO, or interrupter view versus
    EP0 OUT), not further global retry timing.

The relevant official references are:

- [Android Lito USB device tree](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/tags/android-11.0.0_r0.56/qcom/lito-usb.dtsi), including the DWC3 IRQ order, DMA pool, clocks, GSI offsets, and bus resources.
- [Qualcomm PDC irqchip](https://android.googlesource.com/kernel/common/+/ff0000fe82f45/drivers/irqchip/qcom-pdc.c), including `IRQ_ENABLE_BANK`, PDC trigger conversion, parent SPI configuration, and pending-state clearing.
- [Linux Qualcomm DWC3 glue](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/dwc3-qcom.c), including the wakeup-only PHY IRQ setup and `IRQF_NO_AUTOEN` behavior.
- [Linux DWC3 gadget path](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/gadget.c), including the Run/Stop wait and `__dwc3_gadget_start()` receive-FIFO/NUMP defaults.
- [Android Qualcomm PMIC Type-C driver](https://android.googlesource.com/kernel/common/+/c13159a588818/drivers/usb/typec/qcom-pmic-typec.c), which performs PMIC initialization at probe and handles the child interrupt through a threaded path.
- [postmarketOS installation targets](https://postmarketos.org/install/); Bramble was not listed as an official device-specific target in this comparison.

### Current status and next boundary

The current result is not yet a proven Fullerene USB gadget on the real
Bramble. Several runs observed the bootloader's `18d1:4ee0` device, but no
run has yet produced the kernel's expected `1234:0001` identity after the
bootloader disappears. The next hardware evidence should distinguish these
boundaries explicitly and come from the retained RAM trace around:

```text
SETUP received
descriptor queued
IN complete
STATUS OUT
EP0 rearm
```

The immediate implementation boundary is now the corrected Image
entry/section layout, the DWC3-reset DMA ownership boundary, USB2 PHY
reapplication, the non-destructive Type-C observer, the
SMMU-preserving differential, the post-reset DWC3 global setup, and the
Linux-equivalent gadget-start receive-FIFO defaults. The latest bare-pullup
comparison places the remaining failure after physical attach and before a
usable Fullerene EP0 identity; the next diagnostic is retained-trace
correlation at each EP command boundary if the kernel now reaches the USB
identity transition. If it does,
continue with DWC3 link-status,
suspend/hibernation, QMP/Type-C runtime state, clock and reset sequencing,
GSI/event handling, DMA/SMMU fault visibility, and the Linux-equivalent EP0
command/error/re-arm paths. All further Bramble tests continue to use the
audited stock template and `fastboot boot`; partition flashing is not part of
this workflow.

### Signal-probe diagnostics and the Apps-SMMU stream state (this session)

Because the retained RAM trace can only be read back through an enumerated
Fullerene gadget, and the host journal drops disconnect lines of
never-enumerated devices, this session added a Rust-only host-visible
diagnostic channel to the direct gadget handoff. The kernel can now publish
one-bit readouts through the physical pull-up itself
(`--signal-probe` and its variants, all Rust, no scripts):

- `--signal-early-drop CODE` drops the pull-up permanently inside the
  handoff when a polled condition (1 = GEVNTCOUNT delivered a record,
  2 = the armed EP0 SETUP TRB was retired over DMA, 3 = the SETUP payload
  was DMAed, 5 = SOF frame numbers advance, 9 = unconditional control) is
  observed; the host's `-110` line disappearing is the readout.
- `--smmu-gate TYPE` publishes the pull-up only when the Apps-SMMU stream's
  S2CR type equals TYPE (0 = fault, 1 = bypass, 2 = translate), so the
  attach itself names the stream state.
- `--dma-adopt-smmu` walks the bootloader's stage-1 page tables read-only
  and relocates the EP0 DMA objects (event ring, SETUP buffer, TRBs,
  response) into a page the live context already maps: the CPU addresses
  the page physically, DWC3 is published the corresponding IOVA
  (`dma_iova_for()` splits the CPU/DMA views). The walk never writes the
  SMMU.

Device A/B results with these probes (all `fastboot boot` only, all runs
recovered automatically, no flash/erase):

1. The unconditional post-connect pull-up drop (`--signal-early-drop 9`)
   did not remove the host `-110`: the Qualcomm session overrides
   (QSCRATCH `LANE0_PWR_PRESENT`, `UTMI_OTG_VBUS_VALID`,
   `SW_SESSVLD_SEL`) do not gate the physical attach, and a post-attach
   software drop did not become host-visible.
2. A plain Android `adb reboot` produces no `usb 1-9` line at all, so the
   observed `usb 1-9: new high-speed USB device` + `device descriptor
   read/64, error -110` is genuinely the Fullerene handoff's attach, not a
   bootloader transient.
3. The SMMU-enabled route (`configure_dwc3_smmu()` without `--no-smmu`)
   still produces no attach; with the global-fault SCR0 interrupt-enable
   write removed (secure-side bits can reject non-secure writes), the
   route still fails, which points at the rejected S2CR/context-bank
   writes rather than the SCR0.
4. `--smmu-gate 0`, `--smmu-gate 1`, and `--smmu-gate 2` all suppress the
   attach: from non-secure state the DWC3 stream's S2CR matches no SMR
   (`smmu_stream_s2cr_type()` = 255). Either the Apps SMMU leaves the
   stream unmatched (global bypass at ABL or a secure-owned/RAZ register
   view), or TZ owns the stream configuration.
5. `--dma-adopt-smmu` (with the any-mapping fallback) also suppressed the
   attach, confirming adoption could not find a usable TRANSLATE context.

Consequences for the next step: if the stream is globally bypassed
(SMMUEN=0) the CPU-address-as-IOVA assumption holds and the DMA failure
must instead live in the controller/link domain; if the stream is
secure-owned FAULT/TRANSLATE, no non-secure software can currently make
DWC3 DMA work and the only paths are (a) a register group the scan has
not read yet (secure-page aliases), or (b) leaving the SMMU question to a
bootloader-side change, which is out of scope for `fastboot boot` images.
The immediate next experiments are therefore: (i) encode the raw ID0/ID1
SMR count and the first valid SMR word through additional gate values so
the scan's blindness is separated from a genuine no-match, and (ii) if
the SMMU is genuinely bypassed, re-run the event-ring/EP0 A/B with
usbmon on the host to observe whether the device ACKs the SETUP
transaction at all (packet-level liveness beyond the descriptor timeout).

### Root cause 1 found and fixed: the .usb_dma window was not RAM (follow-up session)

The SMMU ladder gates were later corrected (the hex gate parser accepted
every ladder value) and run to completion; the decisive new instrument,
however, was a CPU readback gate over the DMA objects themselves:

- `--signal-ram-gate` writes a pattern to every object the controller will
  DMA (event ring, EP0 TRBs, SETUP buffer, response), evicts it from the
  CPU cache, and reads it back. With the stock 0x9b800000 .usb_dma origin
  the readback FAILED: the region is not backed by usable RAM on this
  handset. Every DMA object written there - the event ring, the SETUP TRBs,
  the setup packet buffer, and the retained trace - silently vanished. This
  explains the entire dead-era symptom set: a dead event ring, a dead EP0,
  an unreadable trace, and a controller that looked wedged.
- Relocating .usb_dma to 0x90000000 (the vendor DT's USB DMA pool start,
  `iova_base`) passed the readback gate, and a new pre-connect event-DMA
  probe (`--signal-dma-probe`: arm a real SETUP transfer, ENDTRANSFER it
  with CMDIOC - the Linux stop-active-transfer pattern) then showed
  GEVNTCOUNT incrementing AND the event word physically landing in DRAM
  (`--signal-evt-data-gate 1`). The linker default for Bramble is now
  0x90000000 (FULLERENE_AARCH64_USB_DMA_ORIGIN still overrides per run).
  The retained trace is now written to working RAM and survives warm
  resets, which re-enables post-mortem analysis.

With DMA writes proven working, the remaining failure was localized
precisely with a new trace-harvest gate: the harvest reads the previous
attempt's retained-trace records at the start of the next attempt (the
trace survives the in-boot DMA clear; Android cannot destroy it inside one
boot), and gates the attach on the raw DEPCMD register values:

- DEPSTARTCFG returns status 0, XferRscIdx 0 (textbook).
- SETTRANSFRESOURCE for both EP0 directions returns status 0 with
  resource index 1 (textbook allocation).
- STARTTRANSFER completes (not a timeout) with DEPCMD status 1, which
  Linux maps to DEPEVT_TRANSFER_NO_RESOURCE - "No resource" - in BOTH the
  Linux and the Android msm resource-allocation orders, with a fresh
  re-allocation immediately before the command, with the SMMU disabled
  (readback-verified sCR0.SMMUEN=0/WACFG=00), with a catch-all bypass SMR
  (readback-verified), with CSFTRST skipped (--no-core-reset), with the
  extended command timeout, and with GCTL.RAMCLKSEL captured as 0 (the
  same value Fastboot ran with; the capture/reapply paths remain in the
  code as they are required after USB resets per the Linux comment).
- A Linux-exact power-option fix was also applied: DSBLCLKGTNG is no
  longer set unconditionally; for GHWPARAMS1.EN_PWROPT_CLK cores Linux
  keeps clock gating ENABLED in device mode. This did not change the
  STARTTRANSFER outcome either.

Current state: attach, chirp, and DMA writes all work; the sole remaining
blocker is STARTTRANSFER reporting "No resource" against an endpoint whose
resource allocation command returned index 1. The next hypotheses, in
order: (a) obtain the Synopsys databook DEPCMD status table to confirm the
exact meaning of status 1 for Start Transfer on this core revision, (b)
read XBL/ABL's working DWC3 device init (the edk2 UsbDeviceDwc3 sources)
for a setup step the Linux-derived flow lacks - in particular anything
that touches the endpoint-context/transfer-resource RAM clock domain, and
(c) probe whether the internal endpoint RAM is the dead element by
finding a command whose success is observable without DMA (e.g. comparing
event content for a resource-related error across allocations). All
diagnostics remain non-destructive Rust gates under
`cargo run -q -p flasks --bin bramble-usb -- loop --direct-handoff ...
--dma-origin 0x90000000 ...`; flash/erase are still never used.

### Continuation: host-visible EP0 progress, probe-kill bugs, and the SET_ADDRESS milestone (third session)

Fixes and instruments added since the previous section (all Rust, no
scripts, `fastboot boot` only):

- `ep0` XferComplete dispatch now matches Linux exactly
  (`dwc3_ep0_xfer_complete` ignores the event status and dispatches purely
  by ep0state; our SETUP TRB sets LST, so healthy completions report
  status 0x8, which the old `status != 0` recovery path was eating).
- The retained trace cursor is reset once per boot
  (`trace_reset_head_for_boot()`): between two `fastboot boot` runs
  Android scribbles the trace page, and a surviving header made the
  in-boot harvest gates count the PREVIOUS run's records (this
  invalidated several earlier gate readings).
- The Apps-SMMU stream ladder was corrected and re-run: the DWC3 stream
  (0xe0) matches NO SMR and the SMMU is INACTIVE (ladder 250: unmatched +
  SMMUEN=0/CLIENTPD=1), i.e. the platform is in global bypass and the
  "no resource" failure is NOT SMMU translation. Non-secure SMR/S2CR
  installs read back verified but do not change behavior; sCR0
  SMMUEN/WACFG writes read back verified but also do not change
  behavior.
- The poll loop now keeps an armed EP0 SETUP TRB
  (`try_arm_setup()` with a bounded retry cooldown), because the core
  rejects Start Transfer while the link is not ON — including during the
  host's bus reset — and because the first SETUP token lands ~1 ms after
  that reset ends.
- Two probe-kill bugs were found and fixed: (1) the signal probe ran its
  diagnostic pull-up toggles and recovery reboot even when the handoff
  had succeeded, silently killing a live enumeration (the host sees
  -ENODEV, which it does not log); it now only runs when the handoff
  failed. (2) in `--no-smmu` mode `poll()` no longer reads the (inactive,
  clock-gated) SMMU fault registers, whose later-in-session reads can
  fault the CPU with an asynchronous external abort.
- The RPMh/interconnect votes are re-asserted at handoff entry:
  Fastboot's votes die with its exit and the USB clock branch collapses
  ~25 s later, which manifests as the handset rebooting mid-enumeration.
- The direct path now programs DCFG.SPEED to a value the transfer engine
  can actually use (HighSpeed for a USB2-only handoff); the proven
  fallback path always did this.

Device evidence (clean per-boot trace):

1. The host ACCEPTS the Fullerene device descriptor: the trace contains a
   SET_ADDRESS (bRequest 5) SETUP after the descriptor read, and then the
   ADDRESSED read/all GET_DESCRIPTOR — the endpoint pipeline
   demonstrably reaches the third control transfer.
2. Despite that, the host's FIRST descriptor read still times out
   (`device descriptor read/64, error -110`) and the device is never
   registered in lsusb: the first SETUP is lost while no SETUP TRB is
   armed (during/between the bus reset and the arm), the host's retry
   then succeeds, and the enumeration breaks at a later, silent stage
   (-ENODEV is suppressed by hub.c).
3. STARTTRANSFER outcomes remain split: some complete with DEPCMD
   status 1 ("No resource"), some time out with CMDACT stuck (which also
   wedges Run/Stop). The proven-working fallback sequence
   (`init_usb2_gadget_reuse_fastboot_ep0`) programs DCFG_HIGHSPEED and
   arms its SETUP TRB while the core is halted, and ITS Start Transfer
   has repeatedly succeeded — the direct path's equivalents have not.
4. The handset still reboots ~26 s after attach in current runs; the
   RPMh re-vote did not remove it, so another reset source (assembly
   60 s recovery timer vs the fail-path stage delay vs a remaining
   abort) is still in play and is the next thing to bisect.

Next steps: (i) run the fallback sequence as the PRIMARY handoff for the
probe build (it is the only sequence whose Start Transfer has worked on
this handset) and re-test enumeration end to end; (ii) if the first-read
timeout persists, keep the armed SETUP TRB across the bus reset (skip
ENDXFER/rearm for EP0 when a reset arrives while the SETUP transfer is
idle-armed) and re-test; (iii) bisect the ~26 s reboot by arming the
assembly recovery timer with a distinctive delay. flash/erase are still
never used; every run recovers automatically.

### Fourth session: gate infrastructure rework, watchdog elimination, and the template recovery

The stock boot template was restored after a host reboot wiped /tmp: the
factory image from the February session was still under
`~/ダウンロード/bramble-up1a.231105.001.b2-factory-46a218d9/`, and its
`boot.img` (valid `ANDROID!` header, same build the handset runs) was copied
back to `/tmp/fullerene-stock-template.Uvg3m2/boot.img`. Neither
`fastboot fetch` (unsupported by this bootloader and fastbootd) nor web
searches were needed. The device was never touched for the recovery.

The diagnostic gate infrastructure was reworked after a structural flaw
surfaced: gates evaluated inside the handoff read attempt 1's still-empty
trace and parked before any data existed. The gates now run in the signal
probe AFTER a 10 s observation window during which the enumeration flows
normally; `cmd_gate_condition_met()` evaluates this run's harvest, true
continues into the normal poll loop (the attach stays up), false drops the
pull-up and resets through the bounded 90 s park (`park_after_gate_failure`,
which replaces the earlier unbounded park that once stranded the handset for
~5 minutes - it recovers to fastboot by itself, but the bound makes the
timing predictable).

Watchdog elimination work (bootreason=watchdog on every probe reboot,
~17 s after probe entry, independent of USB attach, MMIO quiet windows, and
GIC state):

- The APSS watchdog at 0x17c10000 was read at probe entry through BOTH
  register layouts (kpss RST=0x04/EN=0x08 and apcs-timer
  RST=0x38/EN=0x40): the kpss EN reads 0, and the `wdt-armed`/`wdt-off`
  gates confirmed the APSS WDT is NOT armed at probe entry.
- Re-arming the APSS WDT with a 100 s bark/bite did not move the ~17 s
  reboot, and petting both layouts (RST write of 1, the downstream
  watchdog_v2 convention) did not prevent it.
- The secure-watchdog deactivate SCM call
  (0x02000407 = SIP | SVC_BOOT<<10 | SEC_WDOG_DIS, arginfo=1, args[0]=1)
  was added at probe entry and did not change the reboot either.
- The reuse of `SCM_SVC_SEC_WDOG_DIS` and both register layouts is kept in
  the code as diagnostics; the biting watchdog remains unidentified (PON /
  PMIC watchdog or an XBL-armed instance are the remaining candidates).

EP0 fixes applied this session (all verified against the Linux sources):

1. `rearm_setup()` is no longer punitive: a failed Start Transfer on this
   core means the link is not ON yet, never a broken endpoint; the old
   DALEPENA-clear path killed EP0 exactly when the host's post-reset
   descriptor read arrived.
2. The default mode no longer issues the pre-Run/Stop Start Transfer at
   all: on this core it wedges the endpoint command engine and the later
   Run/Stop never publishes the pull-up (default mode produced no attach
   until this change).
3. A freshly DMAed SETUP packet now overrides any in-flight EP0 phase
   (the Linux `setup_packet_pending` equivalent): the setup buffer is
   zeroed after latching, and an EP0 completion with a non-zero setup
   buffer forces the Setup phase, so hosts that abort stalled control
   transfers with a new SETUP no longer lose the request.
4. The USB-reset handler no longer issues ENDXFER for the in-flight EP0
   transfers (the bus reset already flushed them; the ENDXFER-then-rearm
   race answered "No Resource" to every post-reset re-arm).
5. A tight SETUP-arm window (200 us retries, 100 ms bound) runs right
   after Run/Stop, and the poll-loop guard keeps retrying afterwards.
6. `gicv3::init` now disables EVERY SPI and PPI the bootloader left
   enabled (GICD_TYPER-driven ICENABLER sweep plus the redistributor
   words) before enabling only the recovery timer; ABL's own DWC3/PMIC/PDC
   SPIs were still live and fired into the probe's vectors on the host's
   first bus reset.
7. The `.usb_dma`/`.usb_trace` 2 MiB window is mapped Device memory in the
   early MMU (no cache maintenance needed or allowed there), and the
   APSS watchdog page (0x17c10000) is covered by the Device MMIO range.

Gate results with the reworked infrastructure: `armed` = TRUE (the
poll-guard's deferred Start Transfer succeeds while live), `arm-first` and
`setup-first` both passed in the same run (the harvest spans multiple
attempts, so those two race gates are ambiguous and need newest-pair
semantics next session).

Remaining blockers, unchanged: the host's FIRST descriptor read still
times out (-110) even though the arm provably precedes the first captured
SETUP, and the ~17 s watchdog reboot still kills every run. The
enumeration pipeline itself is proven through SET_ADDRESS and the
addressed read-all. Next session's highest-value steps: (i) newest-pair
arm/setup race gates plus a per-arm DSTS.USBLNKST capture to see the link
state at the moment the arm succeeds; (ii) try arming the SETUP TRB
without any link-state gating (raw Start Transfer the moment Run/Stop
returns and after every reset event, tolerating "No Resource" failures);
(iii) identify the biting watchdog via the PON reset-reason register
(PM8150 SPMI read at probe entry) - the APSS WDT and the secure watchdog
are already ruled out. flash/erase are still never used; every run
recovers automatically.

### Fifth session: upstream DWC3 comparison and post-Run/Stop DMA probe

The remaining direct-handoff code was compared against the Android Bramble
4.19 DWC3 sources. The upstream Qualcomm glue uses a 19.2 MHz USB2 reference
clock (`REFCLKPER=52`, the matching GFLADJ value, and the USB2 UTMI/PIPE
clock selection); Fullerene already matches that sequence. A 60 MHz UTMI
clock differential was nevertheless built and tested with no change. The
upstream EP0 TRB flags and transfer parameter layout also match the local
implementation. Endpoint configuration was tightened to match Linux's
control/data notification bits and DWC3 endpoint-type encoding, and an
Android resource-order A/B was added. Neither change moved the hardware
boundary.

The following additional physical comparisons used the same unlocked Bramble
and the non-destructive `fastboot boot` harness. Every image passed the QEMU
protocol preflight and Bramble boot-image audit; none produced `1234:0001`:

| Run log | Differential | Host result |
| --- | --- | --- |
| `fullerene-bramble-loop.4136212.0` | initialize HSPHY before the DWC3 reset | HS attach, `device descriptor read/64, error -110`, Android at about +38 s |
| `fullerene-bramble-loop.4092377.0` | Linux-matching EP0 configuration | HS attach, `-110`, Android at about +38 s |
| `fullerene-bramble-loop.4099412.0` | Android endpoint resource order | same `-110` result |
| `fullerene-bramble-loop.4105056.0` | pre-arm/direct timing comparison | same `-110` result |
| `fullerene-bramble-loop.4115347.0` | original event-DMA probe before Run/Stop | no Fullerene HS identity; the probe can suppress the pull-up, so this is not a conclusive DMA result |
| `fullerene-bramble-loop.4120074.0` | alternate SWDD function ID | same HS attach, `-110`, Android fallback |
| `fullerene-bramble-loop.4124450.0` | skip the SWDD disable SMC | same HS attach, `-110`, Android fallback |

A post-Run/Stop event-DMA A/B was then added. It waits for an unhalted U0
link, issues an EP0 SETUP `STARTTRANSFER`, ends it, and checks the event
count/ring. Run `fullerene-bramble-loop.4148792.0` produced the same HS
attach, `-110`, and stock Android `18d1:4ee7` fallback. Since the first
version returned only a boolean, the probe was extended with a retained trace
record and `post` / `post-record` host gates. Runs
`fullerene-bramble-loop.4173931.0` and
`fullerene-bramble-loop.4177785.0` still showed no Fullerene identity or
gate-TRUE disconnect.

The trace encoding was then corrected so event-ring delivery is recorded
explicitly rather than inferred from a possibly-zero event word. The corrected
run, `fullerene-bramble-loop.4183274.0`, recorded:

```text
usb 1-9: new high-speed USB device number 108 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 24 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The record-presence gate again did not publish a positive result. This leaves
the current evidence at a host-visible HS attach followed by a dead or
unreachable Fullerene EP0 path. It does not yet identify whether the direct
tail returned before the post-probe block or which individual DMA command
failed. The optional trace/gate instrumentation is diagnostic only and is
not a USB success criterion.

The verified stopping point remains unchanged: no exact
`New USB device found, idVendor=1234` line has appeared in `kernel.log`.
Recent runs returned to stock Android automatically; no partition write,
erase, or manual phone operation was used.

### Sixth session: Run/Stop event-ring boundary and latest direct/reuse A/Bs

The next direct-path A/B re-published the EP0 event-buffer address, size,
and consumed count immediately before the final DWC3 Run/Stop transition,
matching the event-buffer part of Android msm-4.19's
`dwc3_gadget_run_stop(true)`. It deliberately did not reset endpoints or
alter the pull-up outside the existing Run/Stop transition. The result was
unchanged:

| Run log | Differential | Host result |
| --- | --- | --- |
| `fullerene-bramble-loop.12076.0` | skip the `DSTS.USBLNKST` gate when retrying EP0 `STARTTRANSFER` | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.21331.0` | reuse Fastboot's event-ring DMA page as the handoff baseline | no Fullerene HS attach in this attempt; Android `18d1:4ee7` fallback appeared on SuperSpeed |
| `fullerene-bramble-loop.28952.0` | re-publish the EP0 event ring immediately before Run/Stop | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.37696.0` | Android msm-style EP0 gadget restart immediately before Run/Stop; an incomplete restart suppressed the final transition | no Fullerene HS attach in this attempt; Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.41752.0` | keep the final Run/Stop transition active even when the Android-style restart command sequence is incomplete | no Fullerene HS attach; Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.44815.0` | repeat the same Run/Stop-continuing full-restart A/B after rebuild | no Fullerene HS attach; Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.48966.0` | start direct-path EP0 with the Linux/Android 512-byte initial packet state | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.56892.0` | set `DCFG.IGNSTRMPP` in the direct gadget-start defaults | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.60992.0` | arm the initial EP0 SETUP only from the USB Reset path | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.64159.0` | `rescue2`: soft-reset and rebuild the EP0 tail during the host's descriptor retry window | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.67089.0` | `diag`: re-drive the trace-selected EP0 stage during the pending descriptor retry | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.72495.0` | `diag` with a 14 s observation window, so the rescue runs after HS attach and before the watchdog bucket | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.76482.0` | return to the non-direct fallback handoff with `--no-smmu` and no EP0 signal probe | no Fullerene HS attach; Android `18d1:4ee7` fallback on SuperSpeed |
| `fullerene-bramble-loop.87784.0` | restore USB2 `GUSB2PHYCFG.SUSPHY` immediately before the direct Run/Stop boundary | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.94140.0` | honor `--start-after-connect` by deferring the initial EP0 `STARTTRANSFER` until after Run/Stop | HS attach, `device descriptor read/64, error -110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.99451.0` | reallocate both EP0 transfer resources after USB Reset while deferring the initial arm until reset | HS attach, `-110`, Android `18d1:4ee7` fallback; Android briefly re-enumerated twice |
| `fullerene-bramble-loop.103092.0` | rebuild both EP0 endpoint contexts after USB Reset while deferring the initial arm until reset | HS attach, `-110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.108278.0` | repeat `--start-after-connect` with the corrected deferred initial arm and `--start-ungated` | HS attach, `-110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.114100.0` | arm the initial EP0 SETUP only from Connect Done after USB Reset | HS attach, `-110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.118036.0` | combine Connect Done-only SETUP arm with EP0 transfer-resource reallocation after USB Reset | HS attach, `-110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.125161.0` | apply Linux/Android EP0 `SETEPCONFIG(MODIFY)` for an already-armed SETUP at Connect Done | HS attach, `-110`, Android `18d1:4ee7` fallback |
| `fullerene-bramble-loop.128470.0` | current timing path with Android-style all-endpoint transfer-resource allocation before EP0 configuration | HS attach, `-110`, Android `18d1:4ee7` fallback |

The `28952` host journal was:

```text
usb 1-9: new high-speed USB device number 110 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 30 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The Run/Stop event-ring re-publication therefore does not cross the EP0
boundary. The exact `New USB device found, idVendor=1234` line is still
absent; all listed runs used only non-destructive `fastboot boot` and
returned to Android automatically.

The first full gadget-restart attempt did not reach the physical attach
boundary because its diagnostic wrapper stopped when the repeated endpoint
sequence was incomplete. Android's corresponding restart helper is void and
the caller still proceeds to Run/Stop, so the next A/B keeps the final
transition active even when the repeated command sequence reports failure.

The two revised full-restart runs (`41752` and `44815`) still produced no
Fullerene high-speed attach, so continuing Run/Stop did not rescue the
restart sequence on this controller. The separate `ep0-initial-512` run
(`48966`) restored the usual high-speed attach but retained the same first
descriptor timeout and Android fallback. The exact
`New USB device found, idVendor=1234` line remains absent. These runs used
only non-destructive `fastboot boot`; no partition write, erase, or manual
phone operation was used.

The `dcfg-ignstrmpp` A/B (`56892`) likewise restored the usual high-speed
attach but did not change the EP0 result:

```text
usb 1-9: new high-speed USB device number 112 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 40 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The phone reported `bootreason=watchdog` and returned to stock Android
automatically; the exact `New USB device found, idVendor=1234` line remains
absent. No partition write, erase, or manual phone operation was used.

The `start-after-reset` timing A/B (`60992`) also left the host at the same
first-descriptor timeout:

```text
usb 1-9: new high-speed USB device number 113 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 42 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Deferring the first SETUP arm to USB Reset therefore does not cross the EP0
boundary. The phone again reported `bootreason=watchdog` and recovered to
stock Android automatically.

The `rescue2` mid-window recovery (`64159`) also did not change the result:
the host still timed out on the first descriptor read before Android
recovered. This rules out a single late EP0 tail rebuild as a sufficient
repair for the current direct handoff.

The trace-selected `diag` rescue (`67089`) also retained the same host
boundary. Re-driving the selected SETUP/DATA/STATUS stage during the pending
descriptor retry did not produce `1234:0001`; the phone again recovered to
stock Android with `bootreason=watchdog`.

The corrected timing test (`72495`) waited 14 seconds before invoking the
same rescue, placing it after the observed HS attach and before the usual
watchdog recovery. The host result remained unchanged:

```text
usb 1-9: new high-speed USB device number 117 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 51 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The exact `New USB device found, idVendor=1234` line remains absent.

As a control, `fullerene-bramble-loop.76482.0` repeated the ordinary
non-direct fallback path (`--no-smmu`, without `--direct-handoff` or the EP0
signal probe). It did not produce a Fullerene USB2 attach; the host instead
saw stock Android SuperSpeed twice:

```text
usb 2-1: new SuperSpeed USB device number 53 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
usb 2-1: new SuperSpeed USB device number 54 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

This confirms that the latest fallback control does not cross the USB2 EP0
boundary either. The phone again reported `bootreason=watchdog` and returned
to Android automatically; no partition write, erase, or manual phone
operation was used.

The USB2-SUSPHY A/B (`87784`) restored the usual high-speed attach but did
not change the first descriptor result:

```text
usb 1-9: new high-speed USB device number 118 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 56 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The phone reported `bootreason=watchdog` and recovered to stock Android
automatically. The exact `New USB device found, idVendor=1234` line remains
absent; no partition write, erase, or manual phone operation was used.

The corrected `start-after-connect` run (`94140`) still reached the same
host boundary:

```text
usb 1-9: new high-speed USB device number 119 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 58 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Moving the initial EP0 arm across Run/Stop therefore did not change the
descriptor timeout. The phone reported `bootreason=watchdog` and recovered
to stock Android automatically; the exact `New USB device found,
idVendor=1234` line remains absent.

The `start-after-reset + reset-resource` A/B (`99451`) also did not cross
the EP0 boundary. The host log contained the usual first timeout and then
two short Android SuperSpeed enumerations:

```text
usb 1-9: new high-speed USB device number 120 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 60 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
usb 2-1: USB disconnect, device number 60
usb 2-1: new SuperSpeed USB device number 61 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The phone again reported `bootreason=watchdog`; no partition write, erase,
or manual phone operation was used.

The `start-after-reset + reset-endpoints` A/B (`103092`) also retained the
same first descriptor timeout:

```text
usb 1-9: new high-speed USB device number 121 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 63 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Rebuilding the EP0 endpoint contexts after USB Reset was therefore
insufficient. The phone reported `bootreason=watchdog` and recovered to
Android automatically; the exact `New USB device found, idVendor=1234`
line remains absent.

The corrected timing plus `start-ungated` A/B (`108278`) also retained the
same first descriptor timeout:

```text
usb 1-9: new high-speed USB device number 122 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 66 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Allowing the initial EP0 arm to proceed without waiting for the link to
reach U0 therefore did not cross the USB2 EP0 boundary. The phone reported
`bootreason=watchdog` and recovered to Android automatically; no partition
write, erase, or manual phone operation was used.

The `start-at-connect-done` A/B (`114100`) also retained the same first
descriptor timeout:

```text
usb 1-9: new high-speed USB device number 123 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 68 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Arming SETUP only from Connect Done therefore did not cross the USB2 EP0
boundary. The phone reported `bootreason=watchdog` and recovered to Android
automatically; no partition write, erase, or manual phone operation was used.

The combined `start-at-connect-done + reset-resource` A/B (`118036`) also
retained the same first descriptor timeout:

```text
usb 1-9: new high-speed USB device number 124 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 70 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Combining post-reset resource reallocation with the Connect Done arm did not
cross the USB2 EP0 boundary. The phone reported `bootreason=watchdog` and
recovered to Android automatically; no partition write, erase, or manual
phone operation was used.

The Connect Done EP0 MODIFY correction (`125161`) also retained the same
first descriptor timeout:

```text
usb 1-9: new high-speed USB device number 125 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 72 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Updating the armed EP0 contexts to the negotiated USB2 packet size at
Connect Done was therefore insufficient. The phone reported
`bootreason=watchdog` and recovered to Android automatically; no partition
write, erase, or manual phone operation was used.

The Android-style all-endpoint resource-order A/B (`128470`) also retained
the same first descriptor timeout:

```text
usb 1-9: new high-speed USB device number 126 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 74 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The resource allocation order therefore remains insufficient on this
controller. The phone reported `bootreason=watchdog` and recovered to
Android automatically; no partition write, erase, or manual phone operation
was used.

The `setup` signal-gate observation (`131656`) was used to test whether the
host's first control SETUP reached the Fullerene EP0 event/DMA path. The gate
did not fire: there was no Fullerene-side disconnect readout during the 14
second observation window, and the host retained the same boundary:

```text
usb 1-9: new high-speed USB device number 127 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 76 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The gate result therefore provides no evidence that `TRACE_SETUP_RECEIVED`
was reached in this attempt; the failure remains before a successful EP0
SETUP/data response. The phone reported `bootreason=watchdog` and recovered
to Android automatically. This was a non-destructive `fastboot boot` run;
no partition write, erase, or manual phone operation was used.

The `armed` signal-gate observation (`136674`) likewise did not produce the
arm-success marker. The host still saw the same USB2 descriptor timeout and
then the stock Android SuperSpeed device:

```text
usb 1-9: new high-speed USB device number 5 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 78 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The `stop-first` A/B (`145115`) added a Run/Stop stop before rebuilding the
post-handoff EP0 state. It did not change the boundary:

```text
usb 1-9: new high-speed USB device number 6 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 80 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The `last-done` gate observation (`147942`) did not report a completed
post-handoff Start Transfer record, and retained the same host result:

```text
usb 1-9: new high-speed USB device number 7 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 82 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The `ep0armed` gate observation (`150598`) also did not report a positive
software armed state at the end of the observation window:

```text
usb 1-9: new high-speed USB device number 8 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 84 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The `last-timeout` gate observation (`155676`) also remained negative, so
the current trace does not show either a completed or a timed-out
post-handoff Start Transfer command:

```text
usb 1-9: new high-speed USB device number 9 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 86 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

All four runs reported `bootreason=watchdog` and recovered to Android
automatically. They used only non-destructive `fastboot boot`; no partition
write, erase, or manual phone operation was used.

The subsequent `u0-status*` gate A/Bs (`159403`, `162325`, `164782`,
`166911`, `168899`, and `171036`) exercised the recorded U0-arm outcomes
individually. Because the pull-up/drop and status channels are not host-visible
when the first descriptor read times out, the gate truth value could not be
independently distinguished from the host trace; all six retained the same
boundary. Representative host output was:

```text
usb 1-9: new high-speed USB device number 10 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 88 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The `armstat` readout attempt (`173229`) likewise retained the same USB2
descriptor timeout and Android fallback. Extending the observation window with
the `always` gate (`176046`, 14 seconds) did not produce a host-visible
disconnect or a different attach result:

```text
usb 1-9: new high-speed USB device number 17 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 102 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The temporary `u0stat` pull-up-cycle readout (`180978`) produced no additional
host event, confirming that QSCRATCH pull-up manipulation is not a usable
readout channel for this failure. A second `always` run (`184402`, 10 seconds)
also retained the same result:

```text
usb 1-9: new high-speed USB device number 19 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 106 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

These runs all reported `bootreason=watchdog` and recovered to Android
automatically. They used only non-destructive `fastboot boot`; no partition
write, erase, or manual phone operation was used.

The direct-path BCR-before-reset A/B (`192480`) retained the original boundary:

```text
usb 1-9: new high-speed USB device number 20 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 107 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The completed ENBLSLPM A/B (`211728`, replacing the interrupted `204105`) also
retained the original boundary and recovered normally:

```text
usb 1-9: new high-speed USB device number 27 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 116 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The ENBLSLPM + 16-bit UTMI combination (`215550`) retained the `-71`
protocol-error boundary:

```text
usb 1-9: new high-speed USB device number 28 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 31, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The 16-bit UTMI/PHYIF A/B (`196912`) did change the host error, but not to a
successful device. The host repeatedly retried the HS attach and reported
`-71` protocol errors before power-cycling the port:

```text
usb 1-9: new high-speed USB device number 21 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: new high-speed USB device number 22 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: usb 1-9: device not accepting address 24, error -71
```

This is evidence that the PHY interface setting changes the electrical/protocol
failure boundary, but it did not produce `1234:0001`; the phone still returned
to Android with `bootreason=watchdog`. The DWC3 `PHYIF` field is bit 3 in the
register definition; bit 8 is `ENBLSLPM`, so the A/B uses the actual 16-bit
setting (`PHYIF=1`, `USBTRDTIM=5`).

Finally, the BCR-before-reset plus DCTL-only A/B (`199296`) omitted the DWC3
`GCTL.CORESOFTRESET` and `GUSB2/3PIPECTL.PHYSOFTRST` stages for this DWC_usb31
core, while retaining `DCTL.CSFTRST`. It returned to the original boundary:

```text
usb 1-9: new high-speed USB device number 25 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 111 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The exact `New USB device found, idVendor=1234` line remains absent. The next
bounded area is the event-DMA and remaining secure-watchdog timing differential;
all runs above were non-destructive and required no manual phone operation.

The non-direct `--reuse-fastboot-dma --no-smmu` control (`218144`) produced no
Fullerene HS attach at all before Android fallback:

```text
usb 2-1: new SuperSpeed USB device number 120 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

A no-signal direct control (`220321`) retained the standard boundary, so the
observed `-110` is not specific to signal instrumentation:

```text
usb 1-9: new high-speed USB device number 32 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

Both runs reported `bootreason=watchdog` and returned to stock Android
automatically.

The no-signal 16-bit UTMI control (`223300`) remained at the `-71`
protocol-error boundary rather than changing back to `-110`:

```text
usb 1-9: new high-speed USB device number 33 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 35, error -71
usb 1-9: device not accepting address 36, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The no-signal 16-bit UTMI plus ENBLSLPM control (`224890`) also remained at
that protocol-error boundary:

```text
usb 1-9: new high-speed USB device number 37 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 39, error -71
usb 1-9: device not accepting address 40, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

Both new runs reported `bootreason=watchdog` and returned to stock Android
automatically.

The no-signal direct control with `--start-ungated` (`230808`) retained the
baseline timing boundary rather than changing the result:

```text
usb 1-9: new high-speed USB device number 41 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The run reported `bootreason=watchdog` and returned to stock Android
automatically after about 38 seconds. The exact
`New USB device found, idVendor=1234` line remains absent.

The `--signal-cmd-gate rescue2` full re-arm A/B (`233788`) also retained the
standard direct-handoff boundary:

```text
usb 1-9: new high-speed USB device number 42 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The signal-heartbeat A/B (`237005`) was likewise host-visible only as the
standard `-110` boundary:

```text
usb 1-9: new high-speed USB device number 43 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The first no-signal 16-bit `PHYIF` / `USBTRDTIM=9` A/B (`239748`) showed that
the `-71` boundary follows `PHYIF`, independent of the nominal 16-bit turnaround
value:

```text
usb 1-9: new high-speed USB device number 44 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: new high-speed USB device number 45 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: new high-speed USB device number 46 using xhci_hcd
usb 1-9: new high-speed USB device number 47 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

All three runs completed without manual device operation and reported
`bootreason=watchdog`.

A 16-bit UTMI run with `USBTRDTIM=6` (`242720`) retained the `-71` boundary, so
changing the nominal turnaround from 9 to 6 did not make the descriptor transfer
host-valid:

```text
usb 1-9: new high-speed USB device number 48 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: new high-speed USB device number 49 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: new high-speed USB device number 50 using xhci_hcd
usb 1-9: new high-speed USB device number 51 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

An 8-bit UTMI control with `USBTRDTIM=5` (`245215`) returned to the baseline
`-110` timing boundary, reinforcing that the interface width, rather than this
single timing value, dominates the current failure difference:

```text
usb 1-9: new high-speed USB device number 52 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

Four follow-up turnaround A/Bs further bounded the 16-bit result. With
16-bit UTMI and `USBTRDTIM=7` (`250329`), the host still saw repeated descriptor
and address failures at the `-71` boundary:

```text
usb 1-9: new high-speed USB device number 53 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 55, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The 16-bit, `USBTRDTIM=8` run (`252469`) remained identical in kind:

```text
usb 1-9: new high-speed USB device number 57 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 59, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

Likewise, 16-bit `USBTRDTIM=10` (`255111`) did not produce a descriptor:

```text
usb 1-9: new high-speed USB device number 61 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 63, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

As a control, the 8-bit interface with `USBTRDTIM=10` (`257182`) returned to the
baseline `-110` timing boundary:

```text
usb 1-9: new high-speed USB device number 65 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

All four runs reported `bootreason=watchdog` and returned to Android
automatically. Together with the earlier `USBTRDTIM=5`, `6`, and `9` runs, this
isolates `PHYIF` as the dominant host-visible boundary lever so far; the
nominal turnaround value did not recover a complete enumeration.

Five additional direct-handoff A/Bs bounded the remaining DWC3 convenience flags
under the 16-bit `PHYIF=1` / `USBTRDTIM=5` setting. Each used the same
non-destructive `fastboot boot` path and added exactly one run-specific flag.
The EP0 initial-max-packet A/B (`261846`) with `--ep0-initial-512` stayed at the
protocol-error boundary:

```text
usb 1-9: new high-speed USB device number 66 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
```

The ignore-start-of-frame control (`264084`) with `--dcfg-ignstrmpp`, the
USB2 suspend-PHY control (`266252`) with `--usb2-susphy`, the DWC3
resource-reset control (`267929`) with `--reset-resource`, and the endpoint
reset control (`269574`) with `--reset-endpoints` all retained the same kind of
host log: repeated descriptor reads and address setup failures with error
`-71`, then Android fallback as `18d1:4ee7`. None produced an endpoint 0
descriptor, an address transition, or the exact `1234` identity. All five runs
reported `bootreason=watchdog` and returned to Android automatically after
about 38 seconds. This rules out those five direct flags as the missing 16-bit
enumeration lever; the next useful lever remains a deeper UTMI protocol or
event/EP0-path difference.

The next 16-bit A/B (`277463`) cleared `GUSB2PHYCFG.U2_FREECLK_EXISTS` (bit 30)
through a new `--u2-freeclk-clear` direct-handoff differential. This matched the
upstream `dis_u2_freeclk_exists_quirk` boundary while retaining 16-bit `PHYIF=1`
and `USBTRDTIM=5`; the host still stayed at the descriptor/address protocol-error
boundary:

```text
usb 1-9: new high-speed USB device number 86 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 89, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The run reported `bootreason=watchdog` and returned to Android automatically.
The exact `1234` line was absent from both `kernel.log` and `kernel-final.log`;
all operation remained non-destructive `fastboot boot`, with no manual device
operation.

A 16-bit `USBTRDTIM=11` A/B (`281912`) remained at the same `-71` boundary as
the tested 5-10 values:

```text
usb 1-9: new high-speed USB device number 90 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 93, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The run reported `bootreason=watchdog`; no exact `1234` identity appeared in
either kernel log. It used the same non-destructive boot-only path and no manual
device operation.

The 16-bit `USBTRDTIM=12` A/B (`284038`) also stayed at the `-71` descriptor and
address boundary:

```text
usb 1-9: new high-speed USB device number 94 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 97, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The run reported `bootreason=watchdog`; the exact `1234` line was absent from
both logs. It remained a non-destructive `fastboot boot` experiment with no
manual device operation.

The 16-bit `USBTRDTIM=13` A/B (`286342`) retained the same kind of host log:

```text
usb 1-9: new high-speed USB device number 98 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 101, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

It reported `bootreason=watchdog` and produced no exact `1234` identity in
either kernel log. The experiment again used only non-destructive
`fastboot boot`; no manual device operation occurred.

The 16-bit `USBTRDTIM=14` A/B (`288411`) likewise remained at `-71`:

```text
usb 1-9: new high-speed USB device number 102 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 105, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

It reported `bootreason=watchdog`; neither `kernel.log` nor `kernel-final.log`
contained the exact `1234` line. The boot remained non-destructive and manual
device operation was not used.

The completed 16-bit `USBTRDTIM=15` A/B (`290640`) also remained at the
same descriptor/address protocol-error boundary:

```text
usb 1-9: new high-speed USB device number 106 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 109, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

It reported `bootreason=watchdog`; neither `kernel.log` nor `kernel-final.log`
contained the exact `1234` line. It remained a non-destructive `fastboot boot`
experiment with no manual device operation.

A 16-bit follow-up (`302233`) tested the web-sourced production Bramble/Barbet
QUSB2 override, `(0x6c,0x67), (0x70,0xc8)`, with the default 16-bit
`USBTRDTIM=5` and no additional special flag. It reached only the Android
fallback `18d1:4ee7`, reported `bootreason=watchdog`, and contained no exact
`1234` line in either kernel log. This run must be treated as invalid as a
Bramble-tune A/B because the active table used a third `usize::MAX` sentinel and
the PHY write loop had not yet been taught to skip it; the intended two-entry
production override was therefore corrupted by an extra MMIO write. The sentinel
skip was added after this run.

After adding the sentinel skip, the corrected production-tune 16-bit run
(`306533`) still remained at the `-71` descriptor/address boundary:

```text
usb 1-9: new high-speed USB device number 110 using xhci_hcd
usb 1-9: device descriptor read/64, error -71
usb 1-9: device not accepting address 113, error -71
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

It reported `bootreason=watchdog`; no exact `1234` identity appeared in either
log. Thus, even with the production `(0x6c,0x67), (0x70,0xc8)` QUSB2 values
applied correctly, 16-bit `PHYIF=1` and `USBTRDTIM=5` did not by itself cross
the enumeration boundary.

The corrected production QUSB2 tune was then repeated in 8-bit UTMI mode
(`309384`), preserving the base timing configuration `USBTRDTIM=9` and
`PHYIF=0`. It did not gain an endpoint descriptor; instead, the host saw HS
attach followed by the original timeout boundary:

```text
usb 1-9: new high-speed USB device number 114 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

It returned to Android automatically with `bootreason=watchdog`. Neither kernel
log contained an exact `1234` identity. This confirms that the corrected Bramble
`(0x6c,0x67), (0x70,0xc8)` QUSB2 values do not remove the 8-bit timeout; they
change the failure mode only in combination with 16-bit `PHYIF=1`.

The 8-bit follow-up cleared `GUSB2PHYCFG.U2_FREECLK_EXISTS` while retaining
`USBTRDTIM=9` and the corrected Bramble QUSB2 tune (`320194`). It remained at
the timeout boundary:

```text
usb 1-9: new high-speed USB device number 115 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The run reported `bootreason=watchdog`; no exact `1234` identity appeared in
either log. Thus, clearing the USB2 free-clock bit alone did not alter the
8-bit result.

Two explicit 8-bit `USBTRDTIM` probes also did not move the corrected
production-tune boundary. `USBTRDTIM=5` (`322388`) and `USBTRDTIM=15`
(`324527`) each showed HS attach followed by the original `-110` descriptor
read timeout, with `bootreason=watchdog` and no exact `1234` identity:

```text
usb 1-9: new high-speed USB device number 116 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
```

This bounds both ends of the supported timing field in 8-bit mode; timing alone
is not the missing lever.

A force-set A/B for the msm-4.19 DWC_usb31 1.70a GA workaround,
`GUCTL3.USB20_RETRY_DISABLE`, was then added (`327928`). In 8-bit mode with the
production QUSB2 tune it stayed at the standard `-110` boundary and reported
`bootreason=watchdog`. A force-clear A/B (`330122`) also stayed at `-110`.
Therefore the retry-disable bit does not change the 8-bit descriptor timeout
and can no longer be treated as the missing bit.

The same two register states were tested with 16-bit `PHYIF=1`, production
QUSB2 tune, and default `USBTRDTIM=5`. Force-set (`332269`) and force-clear
(`334220`) both stayed at the known `-71` descriptor/address boundary, with
multiple attach retries and `Device not accepting address ... error -71`. Both
returned to Android as `18d1:4ee7`, reported `bootreason=watchdog`, and
contained no exact `1234` identity. This rules out an exact-revision condition
around that GUCTL3 workaround as the cause of the 16-bit protocol failure.

### USB probe comment archive for source-line reduction

The AArch64 USB probe source was compressed by consolidating repeated
diagnostics into helpers and removing long in-source comments. The earlier
sections retain the actual run logs and A/B results; this section preserves the
non-obvious probe semantics and calibration that used to live next to the code.

#### Entry, relocation, and exception timing

- `usb_probe_hyper_bare` starts the bare pull-up before relocations or any
  prelude. Its attach time therefore measures ABL/XBL-to-entry latency plus the
  bare sequence cost. It deliberately touches only const physical MMIO, BSS
  already zeroed by `_start`, and the architectural counter; the secure WDT
  remains armed as self-recovery.
- The bare probe also skips GIC/timer setup: firmware-owned redistributor
  accesses can fault before a controller-only result is measured.
- The exception vectors remain in their dedicated 2 KiB-aligned section. If
  they stay in `.text.boot`, the alignment shifts `_start` away from the Android
  Image/PIE relocation base.
- The recovery timer is the EL1 physical-timer PPI 30, not a Qualcomm USB IRQ.
  It is handled before platform IRQ filtering; otherwise a failed no-host probe
  can remain in WFE. The 60 s default gives time for Qualcomm MMIO
  synchronization, GENI UART diagnostics, and the DWC3 reset handshake.
- GICR_BASE must be the full 32-bit `0x17a60000`; the truncated value targets
  an unrelated window and leaves the timer and USB SPIs unserviced. The
  standalone probe also forces GICD Enable/Group1Enable because ABL may leave
  the interface off.
- `stall-map` shortens the recovery timer to entry+15 s, after HS attach and
  link ON. One SDIS blip proves PPI delivery. A ~T+36 s return means the PSCI
  SMC reset works; ~T+38 s means the SMC is dead or the PS_HOLD write in the
  unclocked PMIC/SPMI aperture has stalled the CPU.
- PSCI SYSTEM_RESET is attempted before PS_HOLD. PS_HOLD is only the fallback:
  on this board the PMIC/SPMI aperture write can stall the CPU.

#### Probe ownership, Type-C, and retained trace

- The normal entry prepares the PMIC Type-C role. The gadget probe intentionally
  skips that SPMI access so a PMIC aperture fault can be separated from a
  controller fault. USB2 does not consume orientation; the non-destructive
  Type-C observer plus the SPMI skip is also an A/B for the ~11 s pre-attach
  scan delay.
- The Type-C IRQ variants must install the PMIC role/child interrupt contract
  before exposing the parent SPI. Other standalone probes intentionally do not
  discover SPMI.
- Apps-SMMU state is read before a pull-up is published. A secure-owned or
  clock-gated aperture aborts before attach, which is a distinct outcome from
  any timed post-attach drop.
- The ownership handoff retries a few times for a normal run because Fastboot
  teardown can race kernel entry. A gate readout uses one attempt so the gate is
  evaluated before the watchdog recovery window.
- Android scribbles the retained trace between warm resets, so each boot resets
  the trace cursor once before harvesting; otherwise stale headers make gate
  runs count the previous attempt.
- The bare pull-up probe stays below the DMA/trace boundary. A bad retained DRAM
  aperture must not mask a controller-only handoff result.

#### Watchdog and stable-park contracts

- XBL/ABL arms the secure watchdog on `fastboot boot`; disabling it first
  prevents the ~17 s pre-attach reboot. `FULLERENE_USB_SWDD_SKIP=1` isolates the
  cost of the disable SMC itself.
- Extended SMC diagnostics can prevent attach. They are enabled only for the
  dedicated SMC gate runs; USB gates stay on the minimal single attaching SMC.
- The bare pull-up path schedules a 16 s APSS-WDT bite as a second self-recovery
  net. Petting cannot cancel a pending bite; if the secure watchdog bites first,
  the APSS bite is harmless.
- `WDT` (`0x574454`) marks an actual timeout recovery and `STAB`
  (`0x5354_4142`) marks the stable post-EP0-progress park. A direct polling
  handoff serializes USB RESET and the first SETUP; an IRQ probe drains the ring
  only from its IRQ consumer. Trace activity extends the quiet deadline, but the
  direct path has an absolute reset ceiling in case both hardware watchdogs are
  ineffective.
- After the first EP0 DATA/STATUS progress, an idle descriptor-only gadget is
  healthy and enters the stable park. Pullup-only mode never owns the DWC3 event
  ring and does not call `usb::poll()`, because stale Fastboot event state must
  remain untouched; its polling deadline also covers a firmware-masked PPI.

#### EP0 signal-probe protocol

- The signal probe runs only after a failed handoff or when a gate explicitly
  needs this run's trace. On success it must not toggle the pull-up, reset, or
  contaminate live enumeration.
- Its readout channel is the host journal: after a bounded observation window,
  code+1 short drop/re-attach cycles publish a diagnostic value. QSCRATCH
  overrides are used because they can change the physical pull-up even when
  DCTL is ignored.
- Probe-entry `lnk-nib` parks immediately. An early return proves the signal
  branch was entered; a ~T+37-39 s return means the cfg/gate did not reach it.
- The failed-handoff branch re-runs the GIC sweep and reissues the Linux
  soft-connect tail. `u0_arm_recovery` status maps to APSS bite delays:
  1 -> +2 s, 4 -> +6 s, 5/6 -> +10 s, 0/8 -> Run/Stop reached. Status 0/8 may
  emit one link-ON SDIS blip; its presence proves a running core/U0, while its
  absence distinguishes dead EP0 from failed HS training. A T+37-39 s return
  with -110 can also mean the APSS WDT is secure or unwritable.
- Diagnostic gates own every SDIS pair except `forcehs`, which is a real
  enumeration attempt. An unconditional bite tests APSS-WDT writability
  independently of arm status.
- Gate windows are shortened to beat the unknown ~17 s watchdog. A gate run must
  observe the whole window; non-gate runs may short-circuit on the first live
  signal. If no live signal exists, the newest harvested Start Transfer result
  is encoded: 13 cycles means a wedged/timed-out command, and status-nibble+1
  cycles means a clean status-0 start.

#### Signal-gate meanings and state encoding

| Gate | Interpretation / readout |
| --- | --- |
| `gdbstop` | Delay `(state+1)*250 ms`, then stop; the attach-to-disconnect delta names raw `GDBGLTSSM`. |
| `u0stat` | QSCRATCH drop/restore cycles encode `u0_arm_recovery` status; status 0 is one cycle, failures use the sparse status value, and disabled/not-reached uses 16. |
| `armstat` | Status 0 (+16 s bite) means the command retired; status 8 (+1 s) means `CMDACT` remains and the command channel is wedged. |
| `armalive` | A retired EP0 Start Transfer leaves either a pending TRB or a DMA'd SETUP. Early bite proves L1 was alive; late return means a persistent command wedge. |
| `lnkalive` | DSTS states `{8,9,11,14,15}` mean a reset/resume transaction is stuck; `{4,5,6,7,10}` mean the QSCRATCH link-down phantom. The former bites early. |
| `lnk3` | TRUE when a mid-transaction state was seen, separating UTMI-RX-dead from a core stuck in the reset handshake. |
| `sof` | Bit A is a stale halted/Run-Stop readback (raw 16/17); bit B is a SOF frame-number change in 100 ms. SOF stops the core while the host still tracks the port. |
| `lnk57` | State 7 is POLLING/TX alive but RX deaf; state 5 is RX_DETECT/no training. Other states remain in the else bucket. |
| `lnk4`, `lnk6`, `lnk10`, `lnk12`, `lnk13` | Per-state bisections. A matching DSTS state stops the core while the host tracks the device, publishing the journal line. |
| `lnkraw` | Bite delay is `state+1`, naming the exact DSTS state or exposing a moved field. |
| `lnkstate` | Samples every poll, blips/bites on any untested state, and encodes the exact state; a fixed 16 s bite names the last tested state. |
| `gdb`, `gdbforce` | Bite delay is `state+1` for the raw `GDBGLTSSM` link state. |
| `voteflip`, `voteflip2` | Flip session/clock votes for ~1 s, then restore. A disconnect/re-attach pair proves the glue votes own the pull-up; silence proves they are inert. Variant 2 uses raw QSCRATCH writes. |
| `haltbit` | Early return means `DSTS.DEVCTRLHLT` is set despite the host-visible attach. |
| `dctlbit` | Early return means `DCTL.RUN_STOP` is clear; a set bit contradicts the link-state view. |
| `lnkrawdb` | Encodes the raw DSTS word and link nibble without re-reading. |
| `lnkmasklo`, `lnkmaskhi` | Sample DSTS states for ~1 s and encode the low/high 16-bit seen-state mask nibbles in two companion runs. |
| `rescue2` | Forces the full EP0 tail after a device soft reset. Journal enumeration, not a park, is the result. |
| `diag` | Re-drives the trace-selected stuck read/64 stage and keeps servicing EP0. Journal progress is the result. |
| generic gate | Reads this run's trace. TRUE stops the core while the host tracks the device and publishes a disconnect line; FALSE drops the pull-up and parks without a line. |

- DSTS USBLNKST meanings used by these gates: U0=0, U3=3, SS.DISABLED=4,
  RX.DETECT=5, SS.INACTIVE=6, POLLING=7, RECOVERY=8, HRESET=9, COMPLY=10,
  LPBK=11, RESET=14, RESUME=15. Raw 16/17 encode halted or Run/Stop-cleared
  phantom states.
- Success-path `lnk-nib` buckets the raw code as 0-3, 4-7, 8-11, 12-15, and
  16-17 (halted/Run-Stop-cleared), resetting one second later for each bucket.
  A ~T+38-39 s return means the readout never ran.

#### Failed-init and stage readouts

- A failed gadget init does not publish an EP0-less pull-up. An old fallback
  made the host descriptor timeout indistinguishable from a real EP0/DMA
  failure after a complete init.
- The first failing handoff stage is encoded as reset latency so the failed
  boundary remains observable without a misleading device attach.
- Stage probes intentionally publish an EP0-less pull-up only for electrical
  attach; they hold it briefly for xHCI logging and then recover.
- Halt/cold-halt probes park and preserve the selected failed handoff for
  host-side observation rather than immediately resetting.

### USB probe development loop and handoff summary

This summarizes the retired GLM5.3flash handoff note. The preceding sections
remain the source of truth for detailed run logs and A/B results.

#### Mission and safety boundary

- Target: Google Pixel 4a 5G / Bramble / SM7250, serial
  `26191JECB00076`. Use only non-destructive `fastboot boot`; no `fastboot
  flash`, erase, partition write, manual device operation, destructive Git
  reset/checkout, or unauthorized commit.
- The USB task is not complete. The sole success criterion is the exact host
  journal line `New USB device found, idVendor=1234`.
- The harness normally runs `adb reboot bootloader` automatically. Most probe
  runs return to Android in about 38 seconds, often with
  `bootreason=watchdog`.
- If a prior run was interrupted, first inspect the device and working tree
  read-only, then return to the normal harness rather than inventing a manual
  recovery path.

#### Standard development flow

1. Keep each hardware A/B to one explicit cfg, environment, or register
   differential. Preserve unrelated working-tree changes.
2. Validate before booting: run root `cargo fmt --all`, check the harness, and
   `cargo check` the AArch64 USB probe with the representative handoff/signal
   cfg set used by the run.
3. Run the standard non-destructive loop:

   ```bash
   env -u FULLERENE_AARCH64_USB_SIGNAL_DMA_POST_RUNSTOP \
   cargo run -q -p flasks --bin bramble-usb -- loop \
     --direct-handoff --no-smmu --start-after-connect \
     --signal-probe --u0-arm-probe --smmu-disable --skip-typec-spmi \
     --observe-secs 1 --enum-timeout 20 --hold 1 --fastboot-wait 30
   ```

4. Preserve `kernel.log`, `kernel-final.log`, and `boot-reason.txt` from the
   run directory. Record the differential, timestamps, failure signature, and
   Android recovery behavior in this document.
5. Judge success only by the exact `1234` identity line. A diagnostic gate,
   disconnect, readout, attach, or changed error code is not USB success.

#### Evidence and registration rules

- Trust the host journal attach/descriptor/error/return timestamps,
  `boot-reason.txt`, and a successful USB identity. The previously attempted
  SDIS blip, QSCRATCH pull-up drop, PSCI/PS_HOLD, APSS-WDT readout, UART, and
  GDBGLTSSM channels are no longer reliable success indicators on this
  boundary.
- The interrupted ENBLSLPM run `204105` was later completed/replaced by
  `211728`; it retained the original failure boundary rather than reaching
  `1234`.
- 16-bit UTMI (`PHYIF=1`, `USBTRDTIM=5`) moved the host failure from repeated
  `-110` descriptor timeouts to repeated `-71` protocol/address failures. This
  is an important boundary shift, but not enumeration.
- Register naming rule for this DWC3 core: `GUSB2PHYCFG.PHYIF` is bit 3; bit 8
  is `ENBLSLPM`. Do not call an ENBLSLPM experiment a PHYIF experiment.
- Before acting on a stale objective, re-read the fifth and sixth sessions
  above. Several event-DMA and SWDD differentials were already completed.

### Current session: standard direct-handoff baseline (2026-08-30)

The documented standard loop was run against the connected Bramble serial
`26191JECB00076` using only `adb reboot bootloader` for preparation and
`fastboot boot` for the generated image:

```bash
env -u FULLERENE_AARCH64_USB_SIGNAL_DMA_POST_RUNSTOP \
cargo run -q -p flasks --bin bramble-usb -- loop \
  --direct-handoff --no-smmu --start-after-connect \
  --signal-probe --u0-arm-probe --smmu-disable --skip-typec-spmi \
  --observe-secs 1 --enum-timeout 20 --hold 1 --fastboot-wait 30
```

Preflight, AArch64 probe build, Android v3 boot-image audit, and
`fastboot boot` all passed. Run directory:
`tmp/fullerene-bramble-loop.482353.0`.

The host observed the expected Fullerene USB2 attach, but no device identity:

```text
usb 1-9: new high-speed USB device number 14 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

`boot-reason.txt` was `watchdog`; the handset returned to stock Android after
about 38 seconds. No `New USB device found, idVendor=1234` or `1234:0001`
line appeared. This is a failed baseline, not Fullerene enumeration. The
current working tree was clean before the run and no partition write or erase
was used.

The first follow-up A/B added `--ep0-txfifo-fix`, intended to raise a degenerate
EP0 IN `GTXFIFOSIZ(0)` depth to 32 before Run/Stop. Run directory:
`tmp/fullerene-bramble-loop.485760.0`. It produced the same HS attach,
`device descriptor read/64, error -110`, and Android fallback after about 38
seconds. Its artifact SHA-256 was identical to the baseline
(`b3eec7bfde2563a289e452a25b325032f8c3755d9cbe52090a40bc89a042d524`), so
source inspection found that the harness/Flasks option was not wired into
`fullerene-kernel/build.rs`; this run is recorded as an invalid/inert A/B, not
evidence that the FIFO change is ineffective.

After correcting the inert-option finding, the first effective build attempt
(`tmp/fullerene-bramble-loop.490228.0`) stopped before `fastboot boot` because
the new kernel cfg path referenced `GTXFIFOSIZ0` without defining the DWC3
global TX-FIFO register offset (`E0425`). No physical A/B result was produced
by that attempt. The offset definition was added at `0xc300`; the next step is
to rebuild and rerun the same single-differential FIFO test.

With the cfg and register definition fixed, the effective FIFO A/B completed
as `tmp/fullerene-bramble-loop.491547.0`. The artifact changed to SHA-256
`c3f3f6ffc88d904584d628576f82f84cde319bd8658c6a3ed07dc087b659a74d`, and the
boot image was accepted. The host still observed only:

```text
usb 1-9: new high-speed USB device number 16 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The handset returned to Android with `bootreason=watchdog` after about 38
seconds. No `1234:0001` appeared, so raising the EP0 IN FIFO is not sufficient
to cross the descriptor boundary. The next independent candidate from the
latest commit is the 8-byte short-first-device-descriptor response.

The short-packet A/B then ran as `tmp/fullerene-bramble-loop.494146.0` using
`--ep0-short-first-desc` without the FIFO option. The artifact SHA-256 was
`c33abc26161959eb2ae347da8107c649f91fc5843117e0a362be1f95e0c1d886`, and
`fastboot boot` was accepted. The host still reported an HS attach followed by
`device descriptor read/64, error -110`; it later saw only Android fallback
`18d1:4ee7`, with `bootreason=watchdog` after about 39 seconds. The device
never appeared as `1234:0001`. Capping the first descriptor at 8 bytes is
therefore not sufficient; the missing response is earlier than packet length.

The existing `diag` rescue path was then exercised with the same direct-handoff
options and `--signal-cmd-gate diag` (`tmp/fullerene-bramble-loop.501829.0`).
The build and `fastboot boot` were accepted, but the mid-window EP0 rebuild did
not change the host result:

```text
usb 1-9: new high-speed USB device number 18 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

The handset returned through Android with `bootreason=watchdog` after about
38 seconds and no `1234:0001` identity. Re-running the full EP0 tail while the
host was retrying the descriptor is therefore not sufficient; the next
diagnostic must identify whether the first SETUP or the EP0 IN data transfer
ever reaches the controller.

The SETUP-gated variant was then exercised with the same direct-handoff options
and `--signal-cmd-gate setup` (`tmp/fullerene-bramble-loop.504984.0`). The
artifact SHA-256 was
`481195ed8add509d88ceb286b6c075217827140d3cd14f2cf1933ec03785be86`, and
`fastboot boot` was accepted. The host result remained:

```text
usb 1-9: new high-speed USB device number 19 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The handset returned to Android with `bootreason=watchdog` after about
38 seconds. No host-visible gate result or `1234:0001` identity was produced,
so this run does not distinguish an unreached SETUP gate from a false gate;
it leaves the same descriptor-timeout boundary. The documented next step is
to make the previously working fallback handoff the primary path and retest
the complete enumeration flow.

Before the next physical run, `init_usb2_handoff()` was changed so the probe
build tries the existing Fastboot-reuse/gadget handoff first, then retains the
direct handoff as a fallback if ownership setup fails. This is a single
ordering change based on the earlier successful EP0 `STARTTRANSFER` boundary;
no flash, erase, or partition write is involved.

The fallback-primary run completed as
`tmp/fullerene-bramble-loop.513912.0`. The artifact SHA-256 was
`3fefc1465dd9d6fdb27bf121fb381f6f12ad44d07e3f60543ff6470b58663d5f`, and
`fastboot boot` was accepted. The host observed the same incomplete Fullerene
HS attach and descriptor timeout:

```text
usb 1-9: new high-speed USB device number 20 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 99 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The handset returned to Android with `bootreason=watchdog` after 38 seconds;
the run never produced `New USB device found, idVendor=1234` or `1234:0001`.
Making the fallback primary did not cross the descriptor boundary, so the
next change must target the remaining EP0 IN response path rather than handoff
ordering.

The existing `--ep0-stall-flush` differential was then extended to the
Fastboot-reuse primary path. Previously its `SETSTALL` command existed only in
the direct initializer, so it was bypassed after the fallback-primary reorder;
the change is limited to the optional pre-`STARTTRANSFER` flush and does not
alter the default path.

The fallback-primary `--ep0-stall-flush` A/B completed as
`tmp/fullerene-bramble-loop.519930.0`. The artifact SHA-256 was
`721dffbb816ac9a739e99044648a0f8d94dd20c4431f4546704b2d7a6a1d3cbc`, and
`fastboot boot` was accepted. The host still reported:

```text
usb 1-9: new high-speed USB device number 21 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 101 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The handset returned with `bootreason=watchdog`; no `1234:0001` identity was
observed. The optional EP0 stall flush is therefore insufficient at the
current boundary.

The next A/B is the existing `--reuse-fastboot-dma` differential: reuse the
Fastboot event-ring DMA page for the Fullerene EP0 event ring, SETUP/TRB, and
response objects. This changes only the DMA address/mapping source and is
intended to test whether the linker DMA window is the remaining EP0 IN
boundary.

The reuse-DMA A/B completed as `tmp/fullerene-bramble-loop.525949.0`. The
artifact SHA-256 was
`05f05c697cce94dba08124e11cc0a6ca050c90733c052234d5d8a318435fe5a8`, and
`fastboot boot` was accepted. The host still timed out on the Fullerene
descriptor, then recovered to Android (with one Android reconnect retry):

```text
usb 1-9: new high-speed USB device number 22 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
usb 2-1: USB disconnect, device number 103
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

`bootreason.txt` was `watchdog`; no `1234:0001` identity appeared. Reusing
the Fastboot DMA page is therefore not sufficient.

The next A/B enables `--start-ungated`, removing only the `DSTS.USBLNKST ==
U0` prerequisite from the deferred SETUP-arm retry. This tests whether the
controller accepts the EP0 OUT transfer during the short host reset/U0 timing
window; the halt check and all other handoff safeguards remain active.

The start-ungated A/B completed as `tmp/fullerene-bramble-loop.528555.0`.
The artifact SHA-256 was
`d4bd3c443c956b232876dbfbf67889f3166429899103d52d8a58fe24e881d431`, and
`fastboot boot` was accepted. The host again timed out on the Fullerene
high-speed descriptor and then saw the normal Android SuperSpeed device:

```text
usb 1-9: new high-speed USB device number 23 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: new SuperSpeed USB device number 106 using xhci_hcd
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7, bcdDevice= 4.40
```

The handset returned with `bootreason=watchdog`; no `1234:0001` identity or
host-visible gate result appeared. Removing the link-state prerequisite did
not move the boundary. Following the working ABL/EDK2 implementation is now
the next diagnostic step, before another speculative EP0 A/B: inspect its
DWC3 device-mode initialization, endpoint context/resource setup, TRB/event
ring placement, PHY/clock programming, and Run/Stop ordering against the
Fullerene handoff. This source audit does not authorize flashing or erasing
the handset.

### ABL/XBL and Qualcomm USB function-driver source audit (2026-08-30)

The stock Bramble factory bootloader FBPK was read and unpacked locally without
writing to the handset. Its `abl` entry is an ARM32 ELF image, but it contains
no useful DWC3/USB diagnostic strings. The adjacent `xbl` entry is an AArch64
ELF image and does contain the USB implementation boundary, including
`usb_shared_hs_phy_init`, `usb_shared_ss_phy_init`, `endxfer EP0 OUT`,
`endxfer EP0 IN`, `ep_cmd_write failed`, and `recover ctrl state machine`.
The XBL memory map strings identify the primary USB controller region at
`0x0A600000` (`USB30_PRIM`), consistent with the Bramble platform mapping used
by Fullerene. The USB device-mode implementation is therefore in XBL and its
linked driver path, not in the small ABL payload alone.

Qualcomm's complete ABL/XBL USB source is not publicly present in the checked
out repositories. As the closest source-level comparison, the public
`edk2-msm-binary` repository was inspected for its Qualcomm
`UsbfnDwc3Dxe.efi` driver; the SM7150 and SDM845 binaries retain the original
source-file and function names. The SM7150 disassembly shows this concrete
initialization sequence:

1. `UsbfnDwcInitXferRsc` records the DWC3 core base and allocates/clears the
   software transfer-resource tables.
2. Core registers are initialized, including the global TX FIFO registers
   beginning at `0xc300` and the endpoint-command/event configuration.
3. `DEPSTARTCFG` is issued and its endpoint-command completion is polled.
4. For each configured direction, `SETEPCONFIG` is issued and polled, then
   `SETTRANSFRESOURCE` is issued and polled. In the driver diagnostics this
   second operation is named `SetEPXferRscConfigCmd`; it is not a separate
   undocumented command beyond the standard DWC3 endpoint command.
5. Only after the command pair retires does the driver allocate the endpoint
   transfer/TRB software state and continue toward the control state machine.

This comparison narrows the current Fullerene issue: the required command
pair already exists in Fullerene, but its exact Qualcomm parameter construction,
all-endpoint initialization scope, and ordering against the reused Fastboot
state still need to be made byte-for-byte comparable. The next code change will
be based on that sequence and will be recorded with its build hash and host
journal result before proceeding to another hardware run. No flash, erase, or
partition write was performed.

The initial source-derived hypothesis about a duplicate EP0 OUT
`SETTRANSFRESOURCE` was corrected before any physical test. The Fastboot-reuse
path calls `configure_endpoint_config` (config-only) for EP0 OUT, so its outer
`SETTRANSFRESOURCE` is required; only `configure_endpoint` performs the paired
allocation automatically. The mistaken deletion was reverted and no hardware
run used it. The next physical run will use the restored Qualcomm-like pair,
with the same non-destructive harness and the exact `idVendor=1234` journal
criterion.

### EP0 resource-pair correction run after ABL/XBL source audit (2026-08-30)

Before this run, the source-audit hypothesis about a duplicate EP0 OUT
`SETTRANSFRESOURCE` was corrected. The Fastboot-reuse path uses
`configure_endpoint_config` (config-only), so the outer EP0 OUT resource
command was restored; the mistaken deletion was not used on hardware.

- Harness output: `tmp/fullerene-bramble-loop.564030.0`
- Boot artifact SHA-256: `3fefc1465dd9d6fdb27bf121fb381f6f12ad44d07e3f60543ff6470b58663d5f`
- `fastboot boot` was accepted (`OKAY`) for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `idVendor=18d1, idProduct=4ee0` appeared, then
  disconnected; the USB2 attempt reached `device descriptor read/64, error
  -110`; no `idVendor=1234` line appeared.
- Recovery: the handset returned through Android at 37 s and enumerated as
  `idVendor=18d1, idProduct=4ee7` (`Pixel 4a (5G)`, serial
  `26191JECB00076`). `boot-reason.txt` was `watchdog`.

Conclusion: restoring the Qualcomm-like EP0 OUT command pair did not change
the physical outcome. This is another non-success result; `1234` remains the
only success criterion. No flash, erase, or partition write was performed.

### XBL endpoint-enable ordering differential (2026-08-30)

The stock Bramble XBL was then disassembled far enough to inspect the actual
EP0 initialization, rather than relying only on the public Linux/EDK2
comparison. Its endpoint setup calls issue `DEPSTARTCFG`, then the EP0 OUT
`SETEPCONFIG`/`SETTRANSFRESOURCE` pair, then the EP0 IN pair. The
`DwcConfigureEP` helper writes the corresponding `DALEPENA` bit after each
direction's command pair; the surrounding init code may also write the final
combined state.

At the time of this differential, Fullerene's Fastboot-reuse path was
enabling EP0 OUT immediately after its resource command and EP0 IN after the
second pair. The planned differential moved this to one
`DALEPENA |= 0b11` after both pairs. The test used the same `fastboot boot`-only
harness and the exact host-journal `idVendor=1234` criterion. No flash, erase,
or partition write was authorized or required.

### XBL endpoint-enable ordering run (2026-08-30)

The XBL-derived endpoint-enable ordering differential was built and booted.
Fullerene now leaves `DALEPENA` clear while the EP0 OUT and EP0 IN
`SETEPCONFIG`/`SETTRANSFRESOURCE` pairs retire, then enables both directions
with one `DALEPENA |= 0b11`.

- Harness output: `tmp/fullerene-bramble-loop.573633.0`
- Boot artifact SHA-256: `80757ec7248184dc919cb4d8994065369ab29ceed13ebf726481fa280311324e`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached the same
  USB2 high-speed attach and `device descriptor read/64, error -110`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned at 38 s; `boot-reason.txt` was
  `watchdog`.

Conclusion: the tested combined endpoint-enable ordering did not cross the
descriptor boundary. `1234` remains unobserved, so this is another
non-success result. No flash, erase, or partition write was performed.

### XBL DALEPENA interpretation correction before next run (2026-08-30)

On re-reading the XBL disassembly after the failed run, the prior differential
description was corrected. At `DwcConfigureEP`'s ep-command completion path,
XBL computes the physical endpoint as `(logical_ep << 1) + direction` and sets
that single bit in `DALEPENA` immediately after the corresponding
`SETEPCONFIG`/`SETTRANSFRESOURCE` pair. Therefore the source-derived behavior
is the original per-direction Fullerene sequence, not only one final
`DALEPENA |= 0b11` write. Fullerene was restored to that sequence before the
next physical test; no hardware run uses the corrected code yet.

This correction invalidates the endpoint-enable ordering hypothesis, but the
`.573633.0` result remains a valid failed record of the combined-write test.
The next candidate will be selected from the remaining XBL-vs-Fullerene
initialization differences, with the same non-destructive harness and exact
`idVendor=1234` success criterion.

### XBL post-endpoint device-state differential before next run (2026-08-30)

The next source-derived comparison is the post-endpoint device-state block in
the stock Bramble XBL disassembly. Immediately after the two EP0
`SETEPCONFIG`/`SETTRANSFRESOURCE` pairs, XBL writes `DEVTEN=0x47` (disconnect,
reset, connect-done, link-state, wakeup, hibernation, and suspend events),
then sets `DCTL.APPL1RES` and `DCTL.HIRD_THRES=7` before its later device
Run/Stop transition. Fullerene had enabled the additional command/error event
bits before `DEPSTARTCFG` and changed HIRD to the Lito DT value `0x10` at
Run/Stop; that is not the XBL state observed in the stock image.

The probe path was changed to match those XBL values and timing. This is a
source-derived device-state differential, not a claim of success: the next
physical run must still produce the host journal line
`New USB device found, idVendor=1234` (implemented as `1234:0001`). The run
will use only `fastboot boot`; no flash, erase, or partition write is involved.

### XBL post-endpoint device-state differential run (2026-08-30)

The XBL-derived `DEVTEN` timing and `DCTL` state differential was built and
booted using the documented direct-handoff loop.

- Harness output: `tmp/fullerene-bramble-loop.586935.0`
- Boot artifact SHA-256: `7e69e38bd1b805ebce5ca527ba8c74e3a891ce5cf9f20bf60873ff75fdcf31c0`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached the same
  USB2 high-speed attach and `device descriptor read/64, error -110`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned after 38 s; `boot-reason.txt` was
  `watchdog`.

Conclusion: matching the XBL post-endpoint `DEVTEN=0x47` timing and
`DCTL.APPL1RES`/`HIRD_THRES=7` state did not cross the descriptor boundary.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### XBL event-ring size differential before next run (2026-08-30)

The XBL disassembly also gives a concrete event-ring size: its core-init path
writes `GEVNTSIZ0` with `0xf0` after publishing the event address, while the
current Fullerene probe used a 4 KiB ring (and the earlier Fastboot-DMA A/B
used `0x100`). The backing allocation remains larger for safety, but the
probe's programmed ring length is now `0xf0`, matching XBL. The preceding XBL
device-state match is retained as the common source-derived baseline.

The next physical run will use the same non-destructive `fastboot boot` loop
and requires the exact host-journal `idVendor=1234` criterion. No flash, erase,
or partition write is involved.

### XBL event-ring size differential run (2026-08-30)

The probe was rebuilt with the XBL-observed `GEVNTSIZ0=0xf0` for its EP0
event ring and booted through the standard direct-handoff loop.

- Harness output: `tmp/fullerene-bramble-loop.591086.0`
- Boot artifact SHA-256: `9bb15b881b113f4653a940d9e71ee987acb17a748c8c535d0f40fba4545b7a8f`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene again reached
  USB2 high-speed attach and `device descriptor read/64, error -110`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned after 38 s; `boot-reason.txt` was
  `watchdog`.

Conclusion: the XBL event-ring length did not cross the descriptor boundary.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### XBL event-DMA alignment correction before next run (2026-08-30)

The stock XBL disassembly shows `GEVNTADRLO0` receiving an address ending in
`0x10` (the decoded value is `0x0a6fc010`), which is 16-byte aligned but not
4 KiB aligned. DWC3's event-buffer alignment requirement is 16 bytes. The
Fastboot-DMA reuse path incorrectly rejected every such address with a 4 KiB
mask, so the earlier reuse-DMA run could fall back before testing the captured
firmware buffer. The validation was corrected to require only 16-byte
alignment. No physical run uses this correction yet.

The next run will explicitly use `--reuse-fastboot-dma --no-smmu` so the
captured Fastboot event-DMA address is exercised, with the XBL-derived event
size and device-state settings retained. Success still requires the exact
host journal `idVendor=1234` result; only `fastboot boot` is permitted.

### XBL event-DMA alignment correction run (2026-08-30)

The Fastboot-DMA reuse path was run after relaxing its address validation from
4 KiB to the XBL-observed 16-byte requirement. This time the captured
firmware-owned event page was eligible for the reuse path; the probe retained
the XBL-derived `GEVNTSIZ0=0xf0` and device-state settings.

- Harness output: `tmp/fullerene-bramble-loop.593971.0`
- Boot artifact SHA-256: `34f267d0e15fd81a986aa1e6c5c2c3f1e93e7511ec5e3d07e6698f1fb155ed42`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached
  USB2 high-speed attach and `device descriptor read/64, error -110`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned after 38 s; `boot-reason.txt` was
  `watchdog`.

Conclusion: exercising the eligible firmware-owned event-DMA page did not
cross the descriptor boundary. `1234` remains unobserved. No flash, erase,
or partition write was performed.

### XBL EP0 event-driven STARTTRANSFER differential before next run (2026-08-30)

The Bramble stock XBL disassembly provides a more specific control-flow
difference than the earlier register comparisons. `DwcCoreInit` queues the
8-byte EP0 OUT request in the firmware endpoint object, but its initialization
tail issues only `DEPSTARTCFG`, the two EP0 `SETEPCONFIG`/
`SETTRANSFRESOURCE` pairs, `DEVTEN=0x47`, and the later Run/Stop write; it does
not issue `STARTTRANSFER` there. The event dispatcher handles DWC3
`XferNotReady` events, including `CONTROL_DATA` status 1, and then calls
`DwcStartTransfer` with the queued EP0 request. Fullerene previously recorded
status-1 `XferNotReady` but acted only on status 2 (control status), relying on
an eager/poll-loop EP0 SETUP `STARTTRANSFER` instead.

The next A/B adds an XBL-deferred EP0 mode: it leaves the SETUP TRB published
but does not arm it after Run/Stop, and arms it only from an EP0
`XferNotReady(CONTROL_DATA)` event. The normal mode is unchanged. This is a
source-derived control-flow test, not a success claim; success still requires
the host journal line `New USB device found, idVendor=1234` (implemented as
`1234:0001`). It remains a non-destructive `fastboot boot` run.

### XBL EP0 event-driven STARTTRANSFER differential run (2026-08-30)

The new XBL-deferred mode was built and booted through the standard direct
handoff loop. It left the initial EP0 SETUP transfer unarmed at gadget start
and enabled the status-1 `XferNotReady(CONTROL_DATA)` handling described by
the stock XBL control flow.

- Harness output: `tmp/fullerene-bramble-loop.612369.0`
- Boot artifact SHA-256: `7662567f8061920009c1020415ad169916aa02b69a974806c309689e59e60861`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached USB2
  high-speed attach and `device descriptor read/64, error -110`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned after 38 s; `boot-reason.txt` was
  `watchdog`.

Conclusion: deferring EP0 `STARTTRANSFER` to the XBL-style
`XferNotReady(CONTROL_DATA)` event did not cross the descriptor boundary.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### XBL DEVTEN exact-value correction before next run (2026-08-30)

The preceding XBL event-driven run did not yet test the stock XBL device-event
mask exactly. The Fullerene path still programmed `0x7f` (Disconnect, USB
Reset, Connect Done, Link Status Change, Wakeup, Hibernation Request, and
Suspend). The stock Bramble XBL `DwcCoreInit` disassembly writes `0x47`, which
enables only Disconnect, USB Reset, Connect Done, and Suspend. The next A/B
will select the exact `0x47` mask only with `--xbl-deferred-setup`; all other
paths remain unchanged. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`).

### XBL DEVTEN exact-value correction run (2026-08-30)

The event-driven XBL differential was rebuilt with the stock XBL device-event
mask `DEVTEN=0x47` and booted through the standard direct handoff loop.

- Harness output: `tmp/fullerene-bramble-loop.618187.0`
- Boot artifact SHA-256: `90db397e5cad36ec6880d1093cb79c050151c9ce539d276f489cd5370ec088ef`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached USB2
  high-speed attach at 16:59:59 and `device descriptor read/64, error -110`
  at 17:00:05; no `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned at 17:00:26; `boot-reason.txt` was
  `watchdog`.

Conclusion: the exact XBL `DEVTEN` mask did not cross the descriptor boundary.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### XBL queued EP0 OUT request differential before next run (2026-08-30)

The stock XBL path has one remaining control-flow detail that the prior
event-driven A/B did not mirror. After the two EP0 endpoint resources are
created, XBL queues an 8-byte EP0 OUT request in its software request object
before Run/Stop. Its `XferNotReady(CONTROL_DATA)` path then prepares that
queued request and marks it in flight immediately before `STARTTRANSFER`.
Fullerene had the corresponding setup TRB and event-driven command, but no
EP0 request-ownership record. The next A/B adds a bounded EP0 OUT request
slot: it is queued at the XBL initialization boundary, transitioned to
in-flight only after the hardware command succeeds, completed/released with
the setup event, and requeued for the next control cycle. Reset/disconnect
paths invalidate the slot so it can be posted again. This is limited to
`--xbl-deferred-setup`; normal handoff paths are unchanged. Success still
requires `New USB device found, idVendor=1234` (`1234:0001`).

### XBL queued EP0 OUT request differential run (2026-08-30)

The XBL-style software ownership slot was added to the deferred EP0 setup
path and the artifact was booted through the standard direct handoff loop.

- Harness output: `tmp/fullerene-bramble-loop.629600.0`
- Boot artifact SHA-256: `be624a8bc3fae1c1326a1ae0fcc78335b1e2d6c2d1749fff1b9401ad47ef5f45`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected; Fullerene reached USB2
  high-speed attach, then `device descriptor read/64, error -110` at
  `17:08:57`; no `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned at `17:09:19`; `boot-reason.txt` was
  `watchdog`.

Conclusion: mirroring XBL's queued EP0 OUT request ownership did not cross the
descriptor boundary. `1234` remains unobserved. No flash, erase, or partition
write was performed.

### XBL event-driven TRB control differential before next run (2026-08-30)

The Bramble XBL request-builder disassembly adds one remaining hardware-visible
difference. After the EP0 `XferNotReady(CONTROL_DATA)` transition, XBL builds the
controller TRB with the endpoint state value `1` in the TRBCTL field, plus
HWO/CHN/ISP and then LST/IOC. Fullerene's deferred path was still publishing the
generic `TRB_CONTROL_SETUP` value (`TRBCTL=2`) at that boundary. The next A/B
will use the XBL-derived `TRBCTL=1 | CHN` value only for the deferred event-driven
EP0 arm; the buffer address, event ring, endpoint resources, and `DEVTEN=0x47`
remain unchanged. This is a source-derived control-word differential, not a
success claim. Success still requires `New USB device found, idVendor=1234`
(`1234:0001`), and only `fastboot boot` will be used.

### XBL event-driven TRB control differential run (2026-08-30)

The XBL-derived `TRBCTL=1 | CHN` control word was built and booted through the
standard direct-handoff loop. The event ring, EP0 buffer address, endpoint
resources, and `DEVTEN=0x47` were unchanged from the preceding queued-request
run.

- Harness output: `tmp/fullerene-bramble-loop.645651.0`
- Boot artifact SHA-256: `13a2580a3e2445390008ee6bdf41682a9d5635b1483a121985048e797a7b7072`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected at `17:20:58`; Fullerene
  reached USB2 high-speed attach at `17:21:09`, then
  `device descriptor read/64, error -110` at `17:21:14`; no
  `New USB device found, idVendor=1234` line appeared.
- Recovery: Android `18d1:4ee7` returned at `17:21:35`; `boot-reason.txt` was
  `watchdog`.

Conclusion: matching the XBL request-builder's observed TRB control word did
not cross the descriptor boundary. `1234` remains unobserved. No flash, erase,
or partition write was performed.

### XBL initial EP0 request-order and zero-buffer differential before next run (2026-08-30)

The preceding `TRBCTL=1|CHN` A/B was based on an incorrect control-flow
reading. Re-reading the Bramble XBL disassembly shows that its initial EP0 OUT
request is queued by `DwcQueueRequest` before Run/Stop, and the request builder
immediately builds a `TRBCTL=2` setup TRB and calls `DwcStartTransfer`; the
`CONTROL_DATA XferNotReady` event is not the initial arm trigger. The same
builder publishes buffer address zero for this special setup request. The
next A/B therefore removes the event-driven initial arm, restores the
pre-Run/Stop STARTTRANSFER order, restores `TRBCTL=2`, and uses a zero TRB
buffer address only in `--xbl-deferred-setup`. The software EP0 request slot
and stock `DEVTEN=0x47` remain enabled. This is a source-derived correction,
not a success claim; success still requires
`New USB device found, idVendor=1234` (`1234:0001`). Only `fastboot boot` will
be used.

### XBL initial EP0 request-order and zero-buffer differential run (2026-08-30)

The corrected XBL-order path was built and booted through the standard direct
handoff loop. It queued the software EP0 OUT request, issued the initial
`TRBCTL=2` STARTTRANSFER before Run/Stop, and published a zero TRB buffer
address for the XBL differential.

- Harness output: `tmp/fullerene-bramble-loop.672344.0`
- Boot artifact SHA-256: `c22f5d80db37942e7e4151f404d3c5f80c9ece9c4ee980b03a37273931670af`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected at `17:39:35`; no
  `New USB device found, idVendor=1234` line appeared before Android
  `18d1:4ee7` returned at `17:40:12`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: the source-derived XBL ordering plus zero-buffer request did not
produce a host-visible Fullerene USB device or cross the descriptor boundary.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### XBL initial-order explicit setup-buffer differential before next run (2026-08-30)

The zero-buffer A/B suppressed even the Fullerene USB2 attach, so the literal
XBL request buffer value cannot be copied into this handoff unchanged. The
next A/B keeps the source-derived parts that are independently meaningful —
EP0 request queued before Run/Stop, initial `TRBCTL=2`, `DEVTEN=0x47`, and the
pre-Run/Stop STARTTRANSFER order — while restoring the explicit DMA setup
buffer needed by Fullerene to receive the host SETUP packet. This isolates the
zero-address effect from the ordering effect. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL initial-order explicit setup-buffer differential run (2026-08-30)

The source-derived initial-order path was rebuilt with an explicit DMA setup
buffer instead of the prior zero address. It queued the software EP0 OUT
request, used initial `TRBCTL=2`, enabled stock `DEVTEN=0x47`, and issued
STARTTRANSFER before Run/Stop.

- Harness output: `tmp/fullerene-bramble-loop.675026.0`
- Boot artifact SHA-256: `3fc45d1623ec02bc4543dd09ecad77b4ce8f722119ea2f6c2d8733abd2ccc8ba`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected at `17:41:32`; no
  `New USB device found, idVendor=1234` line appeared. Android `18d1:4ee7`
  returned at `17:42:08`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: restoring the explicit setup buffer did not produce a host-visible
Fullerene USB device in the pre-Run/Stop XBL-order path. `1234` remains
unobserved. No flash, erase, or partition write was performed.

### XBL initial SETUP self-buffer differential before next run (2026-08-30)

The XBL request-builder disassembly was read one level deeper. For the
initial EP0 OUT request (`TRBCTL=2`), the builder takes the special setup
branch and writes the generated transfer-TRB address into the TRB buffer
field; it does not publish the request object's literal buffer argument
(`0`) there. The previous two A/Bs tested those two wrong alternatives for
this Fullerene path: literal zero suppressed attach, and an explicit setup
payload buffer did not attach in the XBL pre-Run/Stop ordering. The next
single differential mirrors this XBL special-case TRB self-buffer while
keeping the already source-derived request order, `TRBCTL=2`, and
`DEVTEN=0x47`. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL initial SETUP self-buffer differential run (2026-08-30)

The XBL-special-case setup TRB was built and booted with its buffer field set
to the generated TRB address, while retaining the source-derived request
order, `TRBCTL=2`, and `DEVTEN=0x47`.

- Harness output: `tmp/fullerene-bramble-loop.689124.0`
- Boot artifact SHA-256: `a18d669fe0877478996c679a136bbe744e1e676d9b4b8eb1121a973636d3568b`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected at `17:51:45`; no
  `New USB device found, idVendor=1234` line appeared before Android
  `18d1:4ee7` returned at `17:52:22`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: the XBL setup self-buffer special case did not produce a
host-visible Fullerene USB device in this handoff path. `1234` remains
unobserved. No flash, erase, or partition write was performed.

### XBL EP0 IN data-TRB control differential before next run (2026-08-30)

The self-buffer result rules out the initial setup-buffer address as a
standalone fix. The same XBL request-builder shows a second relevant split:
for an EP0 IN request it selects the ordinary request path with
`TRBCTL=1` (`NORMAL`), whereas Fullerene currently emits the Linux
`TRB_CONTROL_DATA` value (`TRBCTL=5`) for the descriptor response. The next
A/B keeps the already working Fullerene initial SETUP/link boundary and
changes only the EP0 IN data-phase TRB control type to XBL's observed value.
The initial SETUP buffer remains explicit; this isolates response-phase TRB
semantics from the failed XBL initial-order/self-buffer experiment. Success
still requires `New USB device found, idVendor=1234` (`1234:0001`); only
`fastboot boot` will be used.

### XBL EP0 IN data-TRB control differential run (2026-08-30)

The known `--start-after-connect` initial SETUP/link boundary was kept, and
only the EP0 IN descriptor-response TRB was changed from Linux
`TRBCTL=5` (`CONTROL_DATA`) to the XBL-observed `TRBCTL=1` (`NORMAL`).

- Harness output: `tmp/fullerene-bramble-loop.696011.0`
- Boot artifact SHA-256: `ac5d51ac80f88b56f3c6045fa68d947838c3888c34a0fa74144be394e1ac55e`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`.
- Host journal: Fastboot `18d1:4ee0` disconnected at `17:57:08`; no
  `New USB device found, idVendor=1234` line appeared. Android USB2 then
  timed out with `error -110` at `17:57:24`, and Android `18d1:4ee7`
  returned at `17:57:45`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: changing only the XBL-observed EP0 IN data TRB type did not
produce a host-visible Fullerene USB device. `1234` remains unobserved. No
flash, erase, or partition write was performed.

### XBL event-ring physical address differential before next run (2026-08-30)

The XBL core-init disassembly gives one remaining concrete DMA placement
difference at the first descriptor boundary: it publishes `GEVNTADRLO0` as
`0x0a6fc010`, while the probe's ordinary EP0 event ring remains in the
linker-reserved `0x90000000` USB DMA pool. The next A/B changes only the EP0
event-ring address to that XBL-observed value; the explicit setup buffer, TRB
addresses, response buffer, endpoint commands, and `--start-after-connect`
timing remain unchanged. This isolates event delivery/acknowledgement from
the already-tested TRB control variations. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL event-ring physical address differential run (2026-08-30)

The EP0 event ring was moved only to the XBL-observed `GEVNTADRLO0` value
`0x0a6fc010`; the setup/TRB/response buffers and handoff timing were left
unchanged.

- Harness output: `tmp/fullerene-bramble-loop.707003.0`
- Boot artifact SHA-256: `720f4fb20340a138d4767d9c57c66a1c4fc2d060718f33e29b5db8117c13c99d`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:04:16`; USB2
  reattached at `18:04:27`, but `device descriptor read/64` timed out with
  `-110` at `18:04:32`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:04:53`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: changing only the event-ring DMA address did not produce a
host-visible Fullerene USB device. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### XBL EP0 SETEPCONFIG notification-mask differential before next run (2026-08-30)

The Bramble XBL `DwcConfigureEP` path was re-read at the fixed EP0 setup
sequence. Its configuration word contains `0x300` in the notification field
(transfer-complete plus transfer-in-progress), while Fullerene's Linux-derived
control-endpoint default is `0x500` (transfer-complete plus transfer-not-ready).
The next A/B changes only EP0's `SETEPCONFIG.P1` to the XBL-observed `0x300`;
the EP0 event ring returns to the ordinary `0x90000000` pool, and setup/TRB,
response buffer, endpoint resources, `DEVTEN`, and `--start-after-connect`
timing remain unchanged. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL EP0 SETEPCONFIG notification-mask differential run (2026-08-30)

The EP0 `SETEPCONFIG.P1` notification field was changed only from the
Fullerene/Linux default `0x500` to the XBL-derived `0x300`.

- Harness output: `tmp/fullerene-bramble-loop.717719.0`
- Boot artifact SHA-256: `1a6c0cb2939af1c263092bb4dfc95f06907b4b4b5a901b7d61f5da01d7626fb3`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:12:18`; USB2
  reattached at `18:12:28`, but `device descriptor read/64` timed out with
  `-110` at `18:12:34`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:12:54`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the XBL-derived EP0 notification mask did not produce a
host-visible Fullerene USB device. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### XBL inter-direction EP0 request-order differential before next run (2026-08-30)

The stock XBL `DwcCoreInit` loop configures EP0 OUT, allocates its transfer
resource, calls the initial request-queue helper, and only then configures EP0
IN. The previous Fullerene XBL request-slot A/B queued the software request
after both EP0 directions were configured. The next A/B mirrors this exact
inter-direction boundary and arms the explicit EP0 SETUP TRB before the IN
pair; the event ring, setup payload address, `SETEPCONFIG` notification mask,
and other handoff state remain unchanged. This A/B intentionally uses XBL's
pre-Run/Stop initial arm rather than `--start-after-connect`. Success still
requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL inter-direction EP0 request-order differential run (2026-08-30)

The initial EP0 request was inserted and its explicit SETUP TRB was armed
after the EP0 OUT resource pair, before the EP0 IN configuration pair, as in
the stock XBL initialization loop.

- Harness output: `tmp/fullerene-bramble-loop.725985.0`
- Boot artifact SHA-256: `2a16e2b9de051664f5f4517c56f5ca5fb337b4bfa76862b76919bb71db97f210`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:18:34`; USB2
  reattached at `18:18:45`, but `device descriptor read/64` timed out with
  `-110` at `18:18:50`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:19:11`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: matching XBL's inter-direction request insertion and early EP0
arm did not produce a host-visible Fullerene USB device. `1234` remains
unobserved. No flash, erase, or partition write was performed.

### XBL post-endpoint global-register-order differential before next run (2026-08-30)

The stock XBL `DwcCoreInit` sequence was re-read in the Bramble bootloader
image. After both EP0 `SETEPCONFIG -> SETTRANSFRESOURCE` pairs it writes
`DEVTEN=0x47`, publishes `DALEPENA=3`, then applies the usb31 global state
including `GSBUSCFG1.PIPETRANSLIMIT=0xe00` and the `GUCTL1` bits. Fullerene
currently applies the same Linux-derived global deltas before the endpoint
pairs. The next A/B moves only that call to after `DEVTEN` publication; EP0
request/TRB contents, event DMA, and link timing remain unchanged. Success
still requires `New USB device found, idVendor=1234` (`1234:0001`); only
`fastboot boot` will be used.

### XBL post-endpoint global-register-order differential run (2026-08-30)

The usb31 global-delta call was moved from before the EP0 endpoint pairs to
after `DEVTEN` publication, following the stock XBL order after both
`SETEPCONFIG -> SETTRANSFRESOURCE` pairs. EP0 request/TRB contents, event DMA,
and link timing were unchanged.

- Harness output: `tmp/fullerene-bramble-loop.734404.0`
- Boot artifact SHA-256: `4fc779767bd36d04eb890747a0eb6c0377158a992fe4a9c0650d49fd764752fe`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:24:55`; USB2
  reattached at `18:25:05`, but `device descriptor read/64` timed out with
  `-110` at `18:25:11`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:25:31`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: matching XBL's post-endpoint global-register order did not
produce a host-visible Fullerene USB device. `1234` remains unobserved. No
flash, erase, or partition write was performed.

### XBL fixed EP0 DMA-address differential before next run (2026-08-30)

The XBL request builder was traced past the queue helper. For the initial EP0
OUT request it stores the setup payload pointer `0x80798d70`, and its EP0
transfer-ring entry is the fixed DDR address `0x80798f70`; these are separate
from the event ring at `0x0a6fc010`. The next A/B uses all three XBL-derived
addresses while keeping the current endpoint commands, explicit 8-byte setup
payload, and `--start-after-connect` timing. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### XBL fixed EP0 DMA-address differential run (2026-08-30)

The EP0 setup buffer and first EP0 TRB were changed to the fixed XBL-derived
DDR addresses `0x80798d70` and `0x80798f70`; the event ring remained at the
XBL-derived `0x0a6fc010`, with the post-endpoint global-register ordering also
enabled.

- Harness output: `tmp/fullerene-bramble-loop.743222.0`
- Boot artifact SHA-256: `e3744599b74a0b82603c386269349ab2d75f2272641b7c253b29964fbba09d15`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:31:03`; USB2
  reattached at `18:31:14`, but `device descriptor read/64` timed out with
  `-110` at `18:31:19`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:31:40`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: using the three XBL-derived EP0/event DMA addresses did not
produce a host-visible Fullerene USB device. `1234` remains unobserved. No
flash, erase, or partition write was performed.

### XBL raw DCTL.Run/Stop differential before next run (2026-08-30)

The XBL `DwcRunStop` routine was checked directly: it reads DCTL and changes
only bit31 (`RUN_STOP`). Fullerene's common helper additionally clears
`TRGTULST`/`KEEP_CONNECT` and applies its reconnect policy at that write. The
next A/B keeps the XBL HIRD/APPL1RES state but changes only the final DCTL
Run/Stop write to the XBL bit-preserving form. EP0 DMA addresses, event ring,
endpoint commands, and link timing remain as in the preceding source-derived
run. Success still requires `New USB device found, idVendor=1234`
(`1234:0001`); only `fastboot boot` will be used.

### XBL raw DCTL.Run/Stop differential run (2026-08-30)

The final DCTL Run/Stop write used the XBL bit-preserving form: it changed
only `RUN_STOP` while retaining the existing DCTL state, including XBL's
`HIRD_THRES=7` and `APPL1RES`. EP0 setup/TRB DMA, event DMA, endpoint command
ordering, and link timing were otherwise unchanged.

- Harness output: `tmp/fullerene-bramble-loop.750429.0`
- Boot artifact SHA-256: `054b4314c7c93bba9708ce8117d185caffe11a755d8b925b6b054e9f1fba775e`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:36:05`; USB2
  reattached at `18:36:16`, but `device descriptor read/64` timed out with
  `-110` at `18:36:21`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:36:41`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: preserving the XBL DCTL Run/Stop write semantics did not produce
a host-visible Fullerene USB device. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### XBL per-direction EP0 TRB differential before next run (2026-08-30)

The stock XBL request builder was re-read at `0x80748c90`: its TRB base is
`0x80798f70`, then it adds `endpoint << 6` and `direction << 5`. For EP0 this
places OUT at `0x80798f70` and IN at `0x80798f90`. Fullerene currently uses
`ep0_trb_ptr(0)` for both EP0 directions, so the next A/B gives EP0 IN data
and status their direction-specific slot (`ep0_trb_ptr(1)`) while keeping the
OUT setup in slot 0. The fixed XBL EP0 DMA, event DMA, post-endpoint global
ordering, and raw Run/Stop differentials remain enabled. Success still
requires `New USB device found, idVendor=1234` (`1234:0001`); only
`fastboot boot` will be used.

### XBL per-direction EP0 TRB differential run (2026-08-30)

The EP0 IN data and status paths used TRB slot 1 while the EP0 OUT setup
path remained on slot 0. With the fixed XBL DMA base, this corresponds to
the source-derived OUT/IN addresses `0x80798f70` / `0x80798f90`.

- Harness output: `tmp/fullerene-bramble-loop.769969.0`
- Boot artifact SHA-256: `3bf104cf3d41bb1fcc52fe97946b70156680af2e3cd77e1915847f666eeaa403`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:49:51`; USB2
  reattached at `18:50:15`, but `device descriptor read/64` timed out with
  `-110` at `18:50:20`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:50:42`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 39 seconds.

Conclusion: separating the EP0 IN/status TRB slot according to XBL's
direction-indexed address calculation did not produce a host-visible
Fullerene USB device. `1234` remains unobserved. No flash, erase, or
partition write was performed.

### XBL complete EP0 TRB control-word differential before next run (2026-08-30)

The XBL request builder at `0x80748d70` adds `HWO|CHN|ISP_IMI` to the TRB
control qword, and its finalization at `0x80748e10` adds `LST|IOC`; this is
separate from the earlier, incorrectly timed `TRBCTL=1|CHN` test. The next
A/B enables the `CHN` bit on every Fullerene EP0 setup/data/status TRB while
retaining the corrected pre-Run/Stop initial `TRBCTL=2` path and the
direction-specific IN slot. Event DMA, fixed XBL EP0 addresses, endpoint
ordering, and raw Run/Stop remain enabled. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot`
will be used.

### XBL complete EP0 TRB control-word differential run (2026-08-30)

The XBL-derived `CHN` bit was enabled on Fullerene's EP0 setup/data/status
TRBs, while the corrected pre-Run/Stop setup path and direction-specific IN
slot remained enabled.

- Harness output: `tmp/fullerene-bramble-loop.780393.0`
- Boot artifact SHA-256: `a25da4d464de591df078c7bc8ca9b0857de200eec7f680008c41b0846f62bc68`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:56:31`; USB2
  reattached at `18:56:41`, but `device descriptor read/64` timed out with
  `-110` at `18:56:47`. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:57:08`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: matching XBL's complete EP0 TRB control flags did not produce a
host-visible Fullerene USB device. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### XBL pre-Run/Stop ordering plus complete EP0 TRB control before next run (2026-08-30)

The preceding complete-TRB-control run still included the generic
`--start-after-connect` timing differential, so it did not exercise XBL's
actual initial order end-to-end. The next A/B removes that option: initial
EP0 `STARTTRANSFER` will be issued before the final Run/Stop write, with the
XBL-derived `CHN` control bit and direction-specific IN slot retained. The
XBL event/TRB addresses, post-endpoint global order, and raw Run/Stop write
remain enabled. Success still requires `New USB device found, idVendor=1234`
(`1234:0001`); only `fastboot boot` will be used.

### XBL pre-Run/Stop ordering plus complete EP0 TRB control run (2026-08-30)

The generic `--start-after-connect` timing differential was removed, so the
initial EP0 `STARTTRANSFER` was issued before the final XBL-style Run/Stop
write. The XBL-derived `CHN` control bit and direction-specific IN slot were
also retained.

- Harness output: `tmp/fullerene-bramble-loop.783969.0`
- Boot artifact SHA-256: `dd1fe5beef2d42236a64e6e991e0044fab421db6778ad22bbb2298919a37a876`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `18:58:14`; no Fullerene
  USB2 high-speed attach or `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `18:58:51`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the exact pre-Run/Stop timing plus XBL EP0 control-word
differential suppressed even the USB2 attach on this handoff path. `1234`
remains unobserved. No flash, erase, or partition write was performed.

### EP1 `STARTTRANSFER` no-resource gate before next run (2026-08-30)

ABL/XBL source audit places the current descriptor boundary after EP0 SETUP
handling, when the read/64 response is queued on physical endpoint 1 (EP0 IN).
The existing retained trace can distinguish an EP1 `STARTTRANSFER` returning
the DWC3 `No Resource` status from no EP1 command being issued. The next run
keeps the known-attaching `--start-after-connect` baseline and enables only the
`--signal-cmd-gate ep1-nores` readout; a true gate stops the running controller
so the host journal records it, while a false gate drops the diagnostic pull-up
without treating that as USB success. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### EP1 `STARTTRANSFER` no-resource gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.796295.0`
- Boot artifact SHA-256: `919e56450758b2ce66abb8f5d58f0c1fa93f36a0b60f5cee2300c4c497bd1bef`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:05:51`; Fullerene
  USB2 high-speed attached at `19:06:01`, then
  `device descriptor read/64, error -110` at `19:06:07`. The
  `ep1-nores` gate produced no true-gate disconnect readout. No
  `New USB device found, idVendor=1234` line appeared. Stock Android
  `18d1:4ee7` returned at `19:06:28`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the retained trace did not satisfy `EP1 STARTTRANSFER == No
Resource` in this run. Because the false result also covers “no EP1 command,”
the next A/B checks `ep1-none` before changing the handoff implementation.
`1234` remains unobserved. No flash, erase, or partition write was performed.

### EP1 `STARTTRANSFER` issuance gate before next run (2026-08-30)

The preceding `ep1-nores` gate was false, but that alone cannot distinguish a
clean EP1 command from a missing EP1 command. The next run keeps the same
known-attaching baseline and changes only the gate to `ep1-none`; a true gate
means no physical EP1 `STARTTRANSFER` was retained, while a false gate means
the command was issued (regardless of its status). This remains a diagnostic
readout, not USB success. Success still requires
`New USB device found, idVendor=1234` (`1234:0001`); only `fastboot boot` will
be used.

### EP1 `STARTTRANSFER` issuance gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.799174.0`
- Boot artifact SHA-256: `a746aec5326e583a0b915f27cb1aabee589671016821e80564c465adec155c9c`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot disconnected at `19:07:18`; Fullerene USB2
  high-speed attached at `19:07:28`, then
  `device descriptor read/64, error -110` at `19:07:33`. The `ep1-none`
  gate produced no true-gate disconnect readout. No
  `New USB device found, idVendor=1234` line appeared. Stock Android
  `18d1:4ee7` returned at `19:07:54`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 37 seconds.

Conclusion: an EP1 `STARTTRANSFER` record exists in the retained trace. In
conjunction with the preceding false `ep1-nores` gate, the command was issued
and did not report the exact `No Resource` raw value. Its completion status is
still unknown; `1234` remains unobserved. No flash, erase, or partition write
was performed.

### EP1 `STARTTRANSFER` completion gate before next run (2026-08-30)

The EP1 command is present and is not the exact `No Resource` value, so the
next single-gate A/B selects `ep1-done`. A true result means the EP1 command
completed with no command-status bits selected by the gate; a false result
means the command either returned another error status or the retained raw
word differs from the expected encoding. The host identity criterion remains
strictly `New USB device found, idVendor=1234` (`1234:0001`). Only
`fastboot boot` will be used.

### EP1 `STARTTRANSFER` completion gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.802090.0`
- Boot artifact SHA-256: `945abe33a594746ebe4024921e72f55910f30d6a0d269fd24b9c707d338f215b`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot disconnected at `19:08:53`; Fullerene USB2
  high-speed attached at `19:09:03`, then
  `device descriptor read/64, error -110` at `19:09:08`. The `ep1-done`
  gate produced no true-gate disconnect readout. No
  `New USB device found, idVendor=1234` line appeared. Stock Android
  `18d1:4ee7` returned at `19:09:29`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the retained EP1 `STARTTRANSFER` record is neither the exact
`No Resource` value nor a clean command completion. The current gate does not
expose which other command-status nibble was returned, so the next change is
diagnostic-only: publish that raw status via bounded pull-up cycles. `1234`
remains unobserved. No flash, erase, or partition write was performed.

### EP1 raw `STARTTRANSFER` status readout before next run (2026-08-30)

The preceding three gates establish that an EP1 command exists, is not
`No Resource`, and is not encoded as a clean completion. The next A/B adds a
single diagnostic gate, `ep1status`, which maps the retained status nibble to a
bounded number of QSCRATCH pull-up drop/restore cycles. This changes no USB
command or endpoint state; it only names the failing status from the host
journal. The exact USB success criterion remains
`New USB device found, idVendor=1234` (`1234:0001`), and only `fastboot boot`
will be used.

### EP1 raw `STARTTRANSFER` status readout run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.807454.0`
- Boot artifact SHA-256: `188eb9ea47725a69ed03813606450a82507f9057cdfb3ad3b8cb70f28d9b4ebb`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:12:12`; Fullerene
  USB2 high-speed attached at `19:12:22`, then
  `device descriptor read/64, error -110` at `19:12:27`. The diagnostic
  QSCRATCH pull-up cycles were host-invisible, so no status-count readout was
  produced. No `New USB device found, idVendor=1234` line appeared. Stock
  Android `18d1:4ee7` returned at `19:12:48`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the status-readout code ran through the normal recovery timeline,
but QSCRATCH session overrides do not create host-visible disconnects on this
boundary. The next attempt keeps the same status encoding and changes only
the readout actuator to DWC3 `DCTL.SDIS`, which is the existing host-visible
mechanism. `1234` remains unobserved. No flash, erase, or partition write was
performed.

### EP1 raw status via DCTL.SDIS before next run (2026-08-30)

The previous status gate could not be read because its QSCRATCH cycles were
inert at the incomplete descriptor attach. The next diagnostic-only A/B uses
the same retained EP1 status but emits the nibble as bounded DCTL.SDIS
disconnect/re-attach pairs (maximum six), without reissuing or changing the
EP1 command. The host journal should then expose the status count. USB success
still requires `New USB device found, idVendor=1234` (`1234:0001`); only
`fastboot boot` will be used.

### EP1 raw status via DCTL.SDIS run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.811606.0`
- Boot artifact SHA-256: `dffb7ab60a1ff404a95f9324141793285ca6e085187026365e4ef82a6904e5aa`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot disconnected at `19:14:50`; Fullerene USB2
  high-speed attached at `19:15:01`, then
  `device descriptor read/64, error -110` at `19:15:06`. No additional
  DCTL.SDIS disconnect/re-attach pairs appeared, so the raw-status count was
  not observable. No `New USB device found, idVendor=1234` line appeared.
  Stock Android `18d1:4ee7` returned at `19:15:26`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 36 seconds.

Conclusion: DCTL.SDIS is also inert after this incomplete attach; the
controller is not exposing a host-visible soft disconnect at this boundary.
The next readout uses the existing APSS-WDT timing path, with the status nibble
encoded as a bounded bite delay. `1234` remains unobserved. No flash, erase, or
partition write was performed.

### EP1 raw status via APSS-WDT timing before next run (2026-08-30)

The next diagnostic-only A/B keeps the same `ep1status` status capture but
maps status nibble + 1 to an APSS watchdog delay (capped at six seconds), then
parks without further USB writes. If APSS WDT is writable from the probe, the
Pixel's Android return time identifies the nibble; if not, the unchanged
secure-watchdog return confirms that timing channel is unavailable. USB
success still requires `New USB device found, idVendor=1234` (`1234:0001`), and
only `fastboot boot` will be used.

### EP1 raw status via APSS-WDT timing run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.816112.0`
- Boot artifact SHA-256: `7682d469ca62b07c70eddc3bb5d7d3d89a5f3cd109e96fb354f248da77cb9c9f`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:17:25`; Fullerene
  USB2 high-speed attached at `19:17:35`, then
  `device descriptor read/64, error -110` at `19:17:41`. The APSS-WDT
  timing path did not change the host-visible attach or produce a status
  readout. No `New USB device found, idVendor=1234` line appeared. Stock
  Android `18d1:4ee7` returned at `19:18:01`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the secure-owned watchdog path is not a usable status transport
for this probe, so the raw EP1 status nibble remains unknown. The next
diagnostic-only A/B changes the gate predicate itself: it tests whether the
retained EP1 `STARTTRANSFER` status nibble is zero, while keeping the USB
command path unchanged. `1234` remains unobserved. No flash, erase, or
partition write was performed.

### EP1 status-nibble success gate before next run (2026-08-30)

The next run uses `--signal-cmd-gate ep1-status-success`. It is a read-only
classification of the retained EP1 `STARTTRANSFER` result: TRUE means the
DEPCMD status nibble is zero, including any non-status completion bits; FALSE
means it is nonzero or no EP1 command was recorded. The existing generic gate
will publish TRUE as the usual host-visible disconnect, so the journal can
distinguish a successful command from the current `ep1-done` false-negative.
No USB command, endpoint configuration, or transfer descriptor is changed.
USB success still requires `New USB device found, idVendor=1234`
(`1234:0001`), and only `fastboot boot` will be used.

### EP1 status-nibble success gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.824061.0`
- Boot artifact SHA-256: `913971a7505dfc529cc24e0b964d6dc8f55d4275ca67211b89375e7a1e046ddc`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:22:37`; Fullerene
  USB2 high-speed attached at `19:22:48`, then
  `device descriptor read/64, error -110` at `19:22:53`. No additional
  host-visible disconnect occurred, so `ep1-status-success` was FALSE. No
  `New USB device found, idVendor=1234` line appeared. Stock Android
  `18d1:4ee7` returned at `19:23:14`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 38 seconds.

Conclusion: the retained EP1 `STARTTRANSFER` command was recorded, but its
status nibble is nonzero. The command is therefore failing before a usable
EP1 IN transfer is exposed; the exact nonzero nibble is still unclassified.
The next diagnostic-only A/B tests the DWC3 `BUS_EXPIRY` status value (`2`)
using the same unchanged USB path. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### EP1 status-nibble BUS_EXPIRY gate before next run (2026-08-30)

The next run uses `--signal-cmd-gate ep1-status-bus-expiry`. It publishes a
host-visible TRUE only when the retained EP1 `STARTTRANSFER` DEPCMD status
nibble is exactly `2` (`BUS_EXPIRY`); all other outcomes are FALSE. This is
diagnostic-only and changes no endpoint command, descriptor, or handoff state.
USB success still requires `New USB device found, idVendor=1234`
(`1234:0001`), and only `fastboot boot` will be used.

### EP1 status-nibble BUS_EXPIRY gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.827607.0`
- Boot artifact SHA-256: `8b215a91419213d1d11543ea12ac80581d0eebdb1e2da68eac5aeb104c1bc598`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fullerene again attached as USB2 high-speed and timed out
  during `device descriptor read/64` with `error -110`; no additional
  host-visible gate disconnect occurred, so `ep1-status-bus-expiry` was
  FALSE. No `New USB device found, idVendor=1234` line appeared; stock
  Android fallback was detected and restored.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after approximately 39 seconds.

Conclusion: the retained EP1 status nibble is not `BUS_EXPIRY` (or the
  current host-visible gate remains unavailable). The previous exact
  `0x1000` check also cannot identify a status-1 result when completion bits
  are present. The next diagnostic-only A/B normalizes the status field and
  tests status nibble `1` (`No Resource`). `1234` remains unobserved. No
  flash, erase, or partition write was performed.

### EP1 status-nibble No Resource gate before next run (2026-08-30)

The next run uses `--signal-cmd-gate ep1-status-nores`. It publishes a
host-visible TRUE only when the retained EP1 `STARTTRANSFER` DEPCMD status
nibble is `1` (`No Resource`), ignoring CMDIOC and other non-status bits. This
is diagnostic-only and changes no endpoint command, descriptor, or handoff
state. USB success still requires `New USB device found, idVendor=1234`
(`1234:0001`), and only `fastboot boot` will be used.

### EP1 status-nibble No Resource gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.830620.0`
- Boot artifact SHA-256: `cbb8640c1286d23be888c7d61f9c8d792dcc31af6cf86d156c0b6951fb836cb7`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:25:59`; Fullerene
  USB2 high-speed attached at `19:26:15`, then
  `device descriptor read/64, error -110` at `19:26:20`. No additional
  host-visible gate disconnect occurred, so normalized status-1 (`No
  Resource`) was FALSE. No `New USB device found, idVendor=1234` line
  appeared. Stock Android `18d1:4ee7` returned at `19:26:40`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after 37 seconds.

Conclusion: the recorded EP1 `STARTTRANSFER` status nibble is neither 0
  (success), 1 (`No Resource`), nor 2 (`BUS_EXPIRY`), subject to the
  host-visible gate limitation. The next step reads the ABL/XBL-derived
  command path for the remaining DWC3 status classes and adds a normalized
  status-other gate. `1234` remains unobserved. No flash, erase, or partition
  write was performed.

### EP1 status-nibble other-status gate before next run (2026-08-30)

The next run uses `--signal-cmd-gate ep1-status-other`. It publishes a
host-visible TRUE when the retained EP1 `STARTTRANSFER` status nibble is
nonzero and is neither `No Resource` (`1`) nor `BUS_EXPIRY` (`2`). This is a
bounded diagnostic classification only; it does not retry or alter the USB
command, endpoint configuration, TRB, or handoff state. USB success still
requires `New USB device found, idVendor=1234` (`1234:0001`), and only
`fastboot boot` will be used.

### EP1 status-nibble other-status gate run (2026-08-30)

- Harness output: `tmp/fullerene-bramble-loop.833373.0`
- Boot artifact SHA-256: `3e1a61a3c70f36e3473d8ae49456b8503739b2140b47417f142d4137cdc3adb3`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- Host journal: Fastboot `18d1:4ee0` disconnected at `19:27:27`; Fullerene
  USB2 high-speed attached at `19:27:37`, then
  `device descriptor read/64, error -110` at `19:27:42`. No additional
  host-visible gate disconnect occurred, so `ep1-status-other` did not
  publish TRUE. No `New USB device found, idVendor=1234` line appeared.
  Stock Android `18d1:4ee7` returned at `19:28:04`.
- Recovery: `boot-reason.txt` was `watchdog`; the harness recovered Android
  after approximately 38 seconds.

Conclusion: the four host-visible status classifications are not a reliable
raw-value transport on this handoff boundary; the absence of a gate
disconnect cannot distinguish an unavailable retained trace from a status
class. Stop adding readout actuators and return to the XBL-derived source
path. The next change targets the first EP1 `STARTTRANSFER` call and will be
documented before any physical run. `1234` remains unobserved. No flash,
erase, or partition write was performed.

### Public dwc3 source audit and gate-encoding correction before next run (2026-08-30)

A source-level audit of the public dwc3 references was completed before
another physical run: Linux mainline `drivers/usb/dwc3` and the
Qualcomm downstream `clo/la/kernel/msm-5.4` (`LA.UM.9.14.1.c30`) gadget and
ep0 drivers. Findings that change the local diagnosis:

1. The DEPCMD `CmdStatus` nibble is only ever 0 (success), 1 (No free
   transfer resource), or 2 (Bus Expiry, isochronous only) in both
   references. No public source defines statuses 3-15 for STARTTRANSFER.
2. The completed DEPCMD register returns the transfer-resource index in
   bits 22:16 (`DWC3_DEPCMD_GET_RSC_IDX`). Linux reads it after every
   STARTTRANSFER and passes it back to ENDTRANSFER/UPDATETRANSFER via
   `DEPCMD_PARAM(resource_index)`. EP1's healthy resource index is 1, so a
   completed EP1 STARTTRANSFER legitimately sets bit 16.
3. `DEPSTARTCFG` XferRscIdx=0 resets all endpoint resources (power-on/soft
   reset); 2 resets only non-control endpoints. Each endpoint then receives
   one `SETTRANSFRESOURCE` with `NUM_XFER_RES(1)`.
4. On a USB bus reset Linux never re-allocates resources: it issues
   ENDXFER for active transfers (Databook 3.30a Table 4-2), forces EP0 back
   to the setup phase (stall and restart), and clears the device address.
5. `GCTL.RAMCLKSEL` is reset to 0 by USB reset and must be reprogrammed per
   Connect Done (Linux comment); the local capture/reapply already covers
   this.

The public Qualcomm XBL USB device-mode driver source remains absent:
`qualcomm/edk2` is an upstream Tianocore mirror, `qualcomm/fastboot.efi` is
a Rust UEFI fastboot that only consumes XBL's `EFI_USB_DEVICE_PROTOCOL`,
`edk2-porting/edk2-msm` has no device-mode driver, and the CodeLinaro
`abl/tianocore/edk2` tree carries `QcomModulePkg` only.

The audit exposed a local encoding defect: the retained-trace harvest used
bit 16 as its timeout flag, which collides with EP1's healthy returned
XferRscIdx=1. Every completed EP1 STARTTRANSFER therefore looked like a
timeout wedge, `ep1-done` could never be true for EP1, and the newest-START
harvest decoded a healthy start as `13` (wedge). The timeout flag moved to
bit 31 (`0x8000_0000`), and the gate predicates were corrected:

- `ep1-wedge` (new): the timeout flag is set (CMDACT never retired).
- `ep1-clean` (new): the command retired with status 0 and returned
  XferRscIdx 1 - the healthy data-phase start.
- `ep1-done` now means retired with a zero status nibble regardless of the
  resource index; `ep1-nores` means retired with status nibble 1.
- `timeout`/`done`/`last-timeout`/`last-done`/`cfg-hv-*`/`rsc-hv-*` moved to
  bit 31, and the probe's newest-START decode matches.
- `end_transfer` no longer rewrites EP0 OUT's legitimate resource index 0
  to 1 (Linux passes the captured index unchanged).

Separately, the four preceding status-gate runs all returned through
Android in the ~37 s early-reset bucket instead of the 60/90 s park
buckets, which means the gate evaluation block was never reached: the
trace-quiet watchdog reset the probe first. Gate runs now fall through to
gate evaluation (trace marker `QUIE`) when the trace goes quiet instead of
resetting. The next runs re-establish the readout baseline before any
behavioral change: `always` (mechanism self-test), `ep1-xfer` (did the core
complete the data TRB), `ep1-wedge`, and `ep1-clean`. Only `fastboot boot`
will be used; no flash, erase, or partition write is involved.

### Recovery-timer identification correction and short-observe gate plan (2026-08-30)

The working hypothesis that the ~17 s early-reset biter is the bootstrap
assembly recovery timer is corrected: the probe bootstrap arms `CNTP` with
`TVAL = CNTFRQ * 60` (a 60 s window, `usb_probe.rs` `usb_probe_el1_entry`),
so it cannot be the +17-18 s biter. The journal's earlier elimination work
already ruled out the APSS watchdog (not armed at entry; re-arm and pet
ineffective) and the secure-watchdog SCM disable; the biter therefore remains
unidentified (PON/PMIC watchdog or an XBL-armed instance are still the
candidates). Two consequences follow:

1. The bisect park experiments (60 s park at probe start, 45 s park before
   the entry block) were confounded: an unpetted ~17 s biter overrides any
   park, so the observed ~37-38 s Android returns cannot prove whether
   `run_ep0_signal_probe` was reached. Timing-only bisects are invalid while
   the biter window is shorter than the park.
2. Disarming `CNTP` before the handoff would not move the 38 s bucket and
   would remove the only recovery net for a handoff that hangs past 60 s,
   so that change is dropped. Recovery ownership stays where it is: the
   signal probe disarms `CNTP` at its start, and head-stall/park recovery
   is probe-internal.

The gate runs that matter must instead land evaluation and the host-visible
publish BEFORE the biter, inside its window. The handoff on the fixed SMMU
path attaches at ~+10 s and returns at ~+13-14 s; with
`FULLERENE_USB_PROBE_OBSERVE_SECS` (harness `--usb-observe-secs`) set to 1,
`cmd_gate_condition_met()` evaluates at ~+15 s and the TRUE branch stops the
device immediately (host-visible disconnect line), well before the
~+17-18 s bite. The next two runs re-establish the readout baseline on a
live gadget: `always` with `--usb-observe-secs 1` (TRUE self-test: expect a
Fullerene USB disconnect line plus an Android return at roughly +65 s from
the 30 s post-eval park) and `never` with `--usb-observe-secs 1` (FALSE
baseline: expect the pull-up drop plus the 90 s park return at roughly
+125 s). A ~37-38 s return for both would instead prove the handoff does
not return before the bite, and the next differential moves evaluation into
the handoff tail. Only `fastboot boot` will be used; no flash, erase, or
partition write is involved.

### USB30 core-domain collapse identified as the wedge and the ~17 s biter (2026-08-30)

The attach-delay readout was added to the signal probe: the gate-run entry
block shifts the physical attach by an encoded delay, and the host kernel
log's attach timestamp publishes the value (the attach time is clockwork at
+10-11 s with ±0.5 s noise, so 300 ms steps resolve). Combined with the
timing buckets that work again once the core domain is powered, this
identified the root cause that five sessions of command- and watchdog
elimination had been circling:

1. **The USB30 core domain arrives SW_COLLAPSED.** The snapshot encoded
   GDSCR bit 0 set (SW_COLLAPSE) with a dead GSNPSID: every DWC3 core
   register read is dead. The physical attach is carried entirely by the
   QSCRATCH/PHY session overrides, which is why the host sees a high-speed
   attach while every endpoint command "wedged" (CMDACT never retired on an
   unpowered core) and every post-attach publish was electrically or
   journal-invisible.
2. **The ~17 s biter is the collapsed domain's teardown**, not an APSS,
   secure, CNTP, or GIC watchdog. After the power recovery below, 30 s and
   90 s parks complete (Android at +64 s instead of +38 s), the attach is
   published with the encoded delay, and the earlier biter disappeared. The
   QSCRATCH session drop (the FALSE-gate action) re-triggers it; the
   core-side Run/Stop stop does not.
3. **The recovery sequence restores the core**: RPMh rail votes
   (apply_usb_power, the TCS ACKs succeed), the forced GDSC enable
   (force_enable_usb30_gdsc, bypassing the secure-rail contract gate),
   configure_usb_clocks(Nominal) + enable_usb_clock_branches, and
   reset_usb_blocks(false). After it, GSNPSID answers and the endpoint
   commands retire; the in-handoff 400 ms SETUP arm window was added for
   the host's first SETUP token (~100-200 ms after the pull-up).
4. **The handoff now completes** (the failure-stage park readout showed
   gadget_ready on the gate runs) and the host saw the first non-timeout
   control response: one run logged `device descriptor read/64, error -71`
   (EPROTO) instead of -110, meaning the EP0 answered and the data phase
   glitched. Most runs still log -110, so the SETUP-serving path is
   intermittent.

The readout channels are restored with the core alive: TRUE-gate park
buckets resolve (always -> ~+65 s), the failure-stage park readout
(stage*15 s) works on failed handoffs, and the attach-delay encoding
remains available. The FALSE-gate drop (QSCRATCH clear) must be reworked
before FALSE gates are readable again, because it re-triggers the domain
collapse. Only `fastboot boot` is used; no flash, erase, or partition
write is involved.

### Post-recovery enumeration boundary: first device responses, remaining data-phase and collapse work (2026-08-30)

With the power recovery in the probe entry block, the campaign crossed
several boundaries that had been unreachable for weeks, then stopped one
step short of `1234:0001`:

1. **The handoff now completes on the powered core.** The failure-stage
   park readout showed `gadget_ready` on the gate runs; the reuse path's
   DEPSTARTCFG (previously the suspected stage-4 failure) passes after the
   3 s settle window that follows the power sequence. Without the settle
   the immediate handoff fell to the super-speed fallback.
2. **The host saw the first non-timeout device responses in the campaign.**
   Repeated runs now log `device descriptor read/64, error -71` (EPROTO)
   within the same second as the attach, meaning the SETUP was served and
   the control data phase answered immediately but the host rejected the
   transaction. The -71/-110 split is run-to-run nondeterministic and
   tracks the in-handoff arm window versus the host's first SETUP token.
3. **The composite enumeration-progress readout is wired** (`pub` gate:
   park 10 s + 12 s per `diag_readout_code()` step, readable through the
   restored park buckets) but is currently cut by the remaining collapse
   before the park completes.
4. **The remaining collapse** fires ~5-8 s after the attach,
   nondeterministically, even with the full rail set voted
   (super_speed=true adds pm8150_l9), the MMIO keepalive in
   `park_for_seconds`, and the 3 s settle. It kills parks and readouts in
   most runs. The 90 s parks survived before the attach existed (the
   is-runs) and one run survived it with the attach plus the 4 s attach
   delay, so the trigger is USB-activity-correlated. The journal's
   proposed PON reset-reason SPMI read remains the identification path.
5. **The data-phase hypothesis ladder for the -71**: the EP1 (EP0 IN)
   STARTTRANSFER is rejected "right after the bus reset" (the code's own
   documented flaky window), the host's IN tokens go unanswered (no
   handshake on an unstarted endpoint), the xHCI reports the transaction
   error, and usbcore's retry starts a NEW port reset that re-enters the
   same flaky window until the collapse ends the run. The XBL
   XferNotReady-driven arm (`--xbl-deferred-setup`) was retested on the
   powered core and was pre-empted by the collapse in its one run.

Next session's highest-value steps: (i) read the PON reset-reason
registers over SPMI at probe entry and publish them through the working
attach-delay/park channels to name the collapse; (ii) capture the packet
level from the host side (usbmon needs root, or a USB analyzer) to decode
the -71 transaction error's exact failure (no handshake vs babble); (iii)
make the first EP1 data-phase STARTTRANSFER succeed inside the host's
transaction-error window (fast retry cadence, or the XferNotReady-driven
arm with the collapse neutralized). Only `fastboot boot` is used; no
flash, erase, or partition write is involved.

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)

### EP0 TX FIFO repair A/B after USB30 power recovery (2026-08-31)

The powered-core retest used `--direct-handoff --start-after-connect
--signal-probe --ep0-txfifo-fix`; this is the previously inconclusive EP0/EP1
TX FIFO repair path, retested with the USB30 GDSC/clock/reset recovery in
place.

- Harness output: `tmp/fullerene-bramble-loop.1551282.0`
- Boot artifact SHA-256:
  `a5ac8d976f23316bba94842908d0d912fd8138912d29b2d8f07c525786185854`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:21:42`; Fullerene
  `usb 1-9` high-speed attached at `04:21:55`; then
  `device descriptor read/64, error -110` at `04:22:00`; stock Android
  `usb 2-1` re-enumerated as `18d1:4ee7` at `04:22:21`. No
  `idVendor=1234` line appeared.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: the EP0 TX FIFO repair does not change the post-recovery result;
the run remained in the -110 (no EP0 data response) class. The next repair
target is the first EP1 data-phase Start Transfer within the host's ~0.5 s
transaction-error window, using a ~200 us retry cadence or XferNotReady-driven
pre-arm. Only `fastboot boot` was used.

### Composite diag readout gate after EP0 TX FIFO A/B (2026-08-31)

The `pub` gate was run with `--direct-handoff --start-after-connect
--signal-probe --signal-cmd-gate pub --observe-secs 1` to classify the
remaining failure.

- Harness output: `tmp/fullerene-bramble-loop.1554505.0`
- Boot artifact SHA-256:
  `65c3eab3e07460346ce94dcc2bd4bb0dd1ddea0817db2935cefaa772a60322d6`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- `kernel.log`: Fullerene attached at `04:23:21`; then
  `device descriptor read/64, error -110` at `04:23:26`; stock Android
  returned at `04:23:47`. No `idVendor=1234` line appeared.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: Android returned after 40 seconds, so the remaining
USB-activity-correlated collapse cut the `pub` park before the code bucket
could be published. The readout is invalid, not a valid code 1; this confirms
the collapse still precedes park-based diagnostics. Only `fastboot boot` was
used.

### SETUP-arm link blip diagnostic (2026-08-31)

The link-U0/tight-arm blip was run with `--direct-handoff
--start-after-connect --signal-probe --arm-blip`.

- Harness output: `tmp/fullerene-bramble-loop.1557406.0`
- Boot artifact SHA-256:
  `f9d097e9cfc131455570594998baf4187b6d0dfc9214429b5ccfd490414d2254`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- `kernel.log`: Fullerene attached at `04:24:44`; then
  `device descriptor read/64, error -110` at `04:24:50`; stock Android
  returned at `04:25:10`. No host-visible blip and no `idVendor=1234`
  appeared.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: the arm-blip diagnostic did not publish a disconnect/re-attach
pair. Under its documented semantics, the tight SETUP arm did not succeed
with the core link reporting U0, so the remaining -110 is still the initial
EP0 SETUP delivery path rather than the later EP1 data phase. Only
`fastboot boot` was used.

### SETUP-arm ungated diagnostic (2026-08-31)

The DSTS link-state gate around the tight SETUP arm was bypassed with
`--direct-handoff --start-after-connect --signal-probe --start-ungated
--arm-blip`.

- Harness output: `tmp/fullerene-bramble-loop.1560461.0`
- Boot artifact SHA-256:
  `33c98edfa9ece556da937eb8e59f9bcc13f969565832f24a899c5d17ca7c76d2`
- `fastboot boot` was accepted for Pixel serial `26191JECB00076`; no
  flash/write operation was used.
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:25:49`; Fullerene
  `usb 1-9` high-speed attached at `04:26:02`; then
  `device descriptor read/64, error -110` at `04:26:08`; stock Android
  returned as `18d1:4ee7` at `04:26:28`. No host-visible blip and no
  `idVendor=1234` appeared.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: bypassing the link-state gate did not produce the arm-blip
diagnostic or improve enumeration. The tight SETUP arm still misses the
host's initial SETUP window, so the remaining -110 is not explained by the
U0 gate alone. The next diagnostic should use the PM8150 PON reset-reason
readout unless another bus-level capture becomes available. Only
`fastboot boot` was used.

### PON reset-reason attach delay A/B (2026-08-31)

PM8150 SID 0/peripheral 8 PON reset-reason reads were published by delaying
physical attach by `(code + 1) * 300 ms`. Two runs used the same artifact and
`--direct-handoff --start-after-connect --signal-probe --skip-typec-spmi
--observe-secs 1`; only `fastboot boot` was used. The prior non-instrumented
attach delay was approximately 13 s, so the stable +15 s delay indicates PON
reason code 6.

- Harness output: `tmp/fullerene-bramble-loop.1568105.0`
- Boot artifact SHA-256:
  `23c0f5fb61440af3a078e05344659775a928f1a5edb95ff6a2dba7bb669f5544`
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:30:08`; Fullerene
  `usb 1-9` high-speed attached at `04:30:23` (+15 s); then
  `device descriptor read/64, error -110` at `04:30:28`; stock Android
  returned as `18d1:4ee7` at `04:30:48`.
- Recovery: `boot-reason.txt` was `watchdog`.

- Harness output: `tmp/fullerene-bramble-loop.1570065.0`
- Boot artifact SHA-256:
  `23c0f5fb61440af3a078e05344659775a928f1a5edb95ff6a2dba7bb669f5544`
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:31:11`; Fullerene
  `usb 1-9` high-speed attached at `04:31:26` (+15 s); then
  `device descriptor read/64, error -110` at `04:31:31`; stock Android
  returned as `18d1:4ee7` at `04:31:53`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: the PON readout is repeatable and reports reason code 6. Upstream
POFF decoding maps this toward RESIN_N/reset-line, but PON may only describe
the planned recovery reset rather than the USB-domain collapse. USB enumeration
still stopped at -110, with no `idVendor=1234` line.

### Fast control-transfer retry A/B (2026-08-31)

The failed-arm cooldown was reduced from 200 to 20 and EP0 status/DataIn Start
Transfer retry windows from 2500 ms to 750 ms. The first run accidentally
tested only the SETUP-arm and status portions before the DataIn replacement
was compiled in; the second run tested the complete change. Both used
`--direct-handoff --start-after-connect --signal-probe --skip-typec-spmi
--observe-secs 1`. Only `fastboot boot` was used.

- Harness output: `tmp/fullerene-bramble-loop.1579019.0`
- Boot artifact SHA-256:
  `47dde08b2bf209804e05b2bff38cdaa74704b09b312528e07478f075467a9150`
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:36:24`; Fullerene
  `usb 1-9` high-speed attached at `04:36:38` (+15 s); then
  `device descriptor read/64, error -110` at `04:36:43`; stock Android
  returned as `18d1:4ee7` at `04:37:04`.
- Recovery: `boot-reason.txt` was `watchdog`.

- Harness output: `tmp/fullerene-bramble-loop.1581994.0`
- Boot artifact SHA-256:
  `9e33b2d10ed5d1b5a9147240cf291e98181c40ca3f687dba5aecefef98003178`
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:38:01`; Fullerene
  `usb 1-9` high-speed attached at `04:38:15` (+15 s); then
  `device descriptor read/64, error -110` at `04:38:21`; stock Android
  returned as `18d1:4ee7` at `04:38:42`, briefly disconnected, and
  re-enumerated at `04:38:43`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: reducing the SETUP-arm cooldown and Start Transfer retries to
750 ms did not change the result. Neither run reached the -71 data phase or
`idVendor=1234`; the stable failure remains -110 before the first descriptor
data response. The host-side usbmon path is not accessible without an
interactive sudo password.

### Start-after-reset SETUP-arm A/B (2026-08-31)

The direct handoff was moved from `--start-after-connect` to
`--start-after-reset`, with the standard signal-probe observation. Only
`fastboot boot` was used.

- Harness output: `tmp/fullerene-bramble-loop.1589000.0`
- Boot artifact SHA-256:
  `8017d582c663572b4b365c75d687081decadbd0339cb67208f0a64fb0ffcb79f`
- `kernel.log`: Fastboot `usb 2-1` disconnected at `04:41:45`; Fullerene
  `usb 1-9` high-speed attached at `04:41:59` (+15 s); then
  `device descriptor read/64, error -110` at `04:42:04`; stock Android
  returned as `18d1:4ee7` at `04:42:27`.
- Recovery: `boot-reason.txt` was `watchdog`.

Conclusion: deferring the initial EP0 SETUP arm until the USB reset event did
not improve enumeration. The failure remained the pre-data -110 class.

### Host-side usbmon capture of the -110/-71 failure (2026-08-31)

The user granted usbmon access (`chmod o+x /sys/kernel/debug`,
`chmod a+r /sys/kernel/debug/usb/usbmon/[0-9]u`), so `cat` of
`usbmon/1u` (Fullerene enumerates on `usb 1-9`, bus 1) could be captured
around each `bramble-usb loop` run. The capture is plain text; the host
URB stream for the failing enumeration is:

- `GET_DESCRIPTOR(Device, 64)` to address 0 submitted ~0.2 s after the
  attach line; it completes `-2` (ENOENT = the 5 s software timeout,
  printed as `-110`) with **no transaction error** - the device either
  NAKs the SETUP tokens or ACKs SETUP and NAKs the IN tokens.
- The hub then issues its second port reset and re-submits the read;
  ~0.2-0.35 s later every URB completes `-71` (EPROTO) in the same
  millisecond, and the hub's next port-status poll reads the port
  unconnected (CSC, status 0x0100): **the handset reboots at that
  moment** (`boot-reason.txt` was `watchdog`, Android returns ~20 s
  later at ~42 s from `fastboot boot`).

So the -71 is downstream of the reset, not an independent packet
corruption: the real failure is still "no EP0 data within the host's
first 5 s window", and the window is cut at ~5.5-5.9 s post-attach by
the handset reboot.

### Characterization of the ~5.6 s post-attach handset reset

Five A/Bs pinned the reset down as software-flow-independent:

- `--observe-secs 20` (observation window 20x longer): the reset time
  did not move (still try1-timeout + ~0.6 s).
- A pure-spin park gate (`spin`: wdt pet only, no `usb::poll`, no MMIO,
  30 s, then PSCI reset): the handset still returned at ~42 s, so the
  reset is not caused by our MMIO traffic into a collapsing domain.
- `--swdd-fnid 0x82000107` and `0x02000107` (modern SMCCC encodings of
  the secure-watchdog disable): unchanged ~42 s returns; the SMC path
  does not disable whatever bites.
- The rail refresh keepalive (see below) did not move it either.
- `boot-reason.txt` stayed `watchdog` in every run.

The reset is therefore a firmware/PON-level event roughly 5.5-5.9 s
after the physical attach, independent of Fullerene's control flow. The
leading suspect remains the RPMh/QMP-core-domain collapse documented in
the recovery section above cascading into a PON reset; the rail
re-vote keepalive did not prevent it.

### Readout-channel findings (all one-bit gates this era are void)

- `u0_arm_wdt_bite` never lands: the control gate `u0-status4` (which
  must evaluate FALSE because `U0_ARM_STATUS` is 0xFFFF_FFFF when the
  u0-arm probe env is absent) returned the same ~42 s bucket as the
  TRUE gates. The APSS WDT registers are not writable from EL1 on this
  unit, so **every bite-timed gate readout is void**.
- Every park is cut by the ~5.6 s reset, so TRUE (`stop` + 30 s park)
  and FALSE (90 s park) also collapse into the same return time.
- The generic-gate FALSE path now schedules `u0_arm_wdt_bite(1)` +
  `park_for_seconds(90)`; it is correct in intent but inert until the
  bite channel works.
- The only surviving readouts are: usbmon (host side), kernel.log
  lines, the attach-suppression gates (`--signal-fsr-gate` proved an
  Apps-SMMU fault latches during the pre-attach EP0 DMA probe), and
  the reset time itself.

### Disconnect primitives are all electrically inert

`ep0_signal_drop_pullup` (QSCRATCH SS bit 24 + HS bits 20/28), the
QUSB2 `VBUSVLDEXT0`/`VBUSVLDEXTSEL0` pair, and **DCTL Run/Stop clear**
(`gate_true_stop_device`) all leave the bus unchanged: with a `dstat`
gate run no root-hub interrupt event, no URB abort, and no attach line
appeared. The D+ pull-up on this unit is owned by something outside
these registers (most likely the QUSB2 session sensing the host's own
VBUS). The `pubd` and `dstat` gates remain in the tree as records of
these negative results.

### Code changes (all uncommitted, in the working tree)

- `refresh_usb_power(super_speed)` in bramble.rs: re-sends the rail
  VRM votes unconditionally; `apply_usb_power` early-returns once its
  state flag matches, which made the periodic park keepalive a no-op.
  Wired into the park keepalive and a new 0.5 s keepalive in the
  signal-probe observation loop (GDSC force-enable retained).
- Data phase is now XferNotReady-driven (`DATA_PHASE_PENDING_START`):
  the SETUP dispatch prepares the EP1 IN TRB and the
  XferNotReady(CONTROL_DATA) handler issues
  `retry_start_transfer(1, ..., 200 ms)`, matching the stock XBL flow
  ("the core only consumes a data TRB armed from the NRDY boundary").
  The dispatch-time proactive Start Transfer was removed.
- The status phase no longer issues `start_status(1)` proactively at
  StatusIn; the XferNotReady(CONTROL_STATUS) handler owns it.
- The USB-reset KEEP path now clears the stale `EP0_SETUP_ARMED` and
  re-arms (`try_arm_setup`): a bus reset terminates the armed EP0 OUT
  transfer even though the software state survived, which NAKed every
  post-reset SETUP token.
- `try_arm_setup` no longer gates on the DSTS USBLNKST nibble (it read
  non-U0 states 4/6/7/10/12/13 while the host was already issuing
  tokens); only the DEVCTRLHLT halt check remains.
- The tail reset now publishes the composite diag code as
  `code*1 s` of extra poll-serviced wait (trace marker "MRAD"+code)
  before `reset_after_probe_failure`, and stable-parks the moment
  `probe_ep0_progress()` reports the data phase armed. In the current
  regime the ~5.6 s reset still preempts it.
- `ARM_COOLDOWN` reverted to 200 polls and the data/status Start
  Transfer retry windows to 2500 ms (the values from the era that once
  reached the -71 data phase).

### A/B results this session (all `--direct-handoff
--start-after-connect --signal-probe --skip-typec-spmi --observe-secs 1`
unless noted; only `fastboot boot` was used)

- default (usbmon run 1): -110 at +5 s; -71 burst + handset reboot at
  +5.6 s. Unchanged by: `--swdd-skip`, the retry revert, the rail
  keepalive, the NRDY-driven arm, the KEEP re-arm, the DSTS-gate
  removal, `--start-ungated`, `--swdd-fnid` variants, `--xbl-deferred-setup`.
- `--signal-fsr-gate 1 --signal-dma-probe`: the device ATTACHED, so an
  Apps-SMMU fault latched during the probe's own EP0 DMA window while
  the probe's event DMA still delivered. Cause unknown; the faulting
  address is journalled in the retained trace (TRACE_SMMU_FAULT) but
  the trace is only readable from an enumerated gadget.
- `--signal-cmd-gate always|setup|armed|desc|darm|ep1-clean|ep1-nrdy|
  ep1-xfer-ok|ep1-xfer-err|ep1-none|u0-status4|spin|dstat`: all returned
  the same ~42 s bucket; none is decodable while both the bite channel
  and the parks are cut by the ~5.6 s reset.

### Next steps (priority order)

1. Beat the ~5.6 s reset: read the PM8150 PON/S2 reset reason registers
   at the NEXT probe entry (the existing `read_pm8150_pon_reset_code`
   already reports code 6 = RESIN_N-ish) and correlate; consider voting
   the rails through a different RSC set (sleep TCS) or re-arming the
   GDSC with HW control cleared inside the first 5 s window.
2. Complete the data phase inside the first 5 s: the XferNotReady-driven
   arm is in place; what remains unverified is whether the SETUP event
   is dispatched at all. The valid next readout is the attach-delay
   channel (`core_attach_delay_code`-style): encode
   `TRACE_HARVEST_SETUP > 0` into the attach delay of the *next* boot's
   first attempt, since the PON runs proved ~300 ms attach steps are
   readable in kernel.log.
3. If the SMMU fault is real on the enumeration path, the faulting IOVA
   (TRACE_SMMU_FAULT FAR) identifies which DMA object lacks a mapping;
   reproduce it with a context-FAR value gate through the
   attach-suppression mechanism instead of the dead bite channel.
4. Only `fastboot boot` was used.

## 2026-08-31 (session 3): the mrad tail readout names the failure, cross-boot trace harvest is dead

### New readout channel: the `mrad` gate falls through to the tail

`--signal-cmd-gate mrad` (new) makes `cmd_gate_condition_met()` return
`None`, so the signal probe skips every gate branch and reaches the tail:
the composite `diag_readout_code()` rides a `code*700 ms` poll-serviced
wait (tail changed from `code*1 s`) before the PSCI reset. The eval lands
~1 s after the handoff return, so even code 6's reset (~+5.2 s post-eval)
beats the ~5.5 s post-attach death, and the Android return time names the
code bucket (`reset + ~21 s`; the death-reset bucket is attach+26.6 s).
The previous `code*1 s` tail only separated codes 1-4; 5/6 were cut by
the death, which made every standard run land in the death bucket (~41-42
s) and look identical.

### The composite diag code is 5-6 (best fit 6)

The `mrad` run (`tmp/fullerene-bramble-loop.1828852.0`, standard flags
plus the gate) attached at +15 s, failed read/64 at +5 s, and Android
returned at attach+26.0 s - 0.6 s ahead of the death bucket (attach+26.6
s measured across every standard run today), which fits the PSCI reset at
eval+4.2 s = code 6. The standard runs' stable ~41 s returns rule out
codes 1-4 (+37-40 s PSCI buckets never observed).

The ladder semantics (re-derived; the earlier comments conflated DESC):
code 2 = one SETUP reached DRAM; 3 = two SETUPs; 4 = a full
dispatch+DARM cycle twice; 5 = the newest data-phase arm (DARM) queued
cleanly; 6 = XferNotReady(CONTROL_DATA) on EP1 IN fired twice. So the
handoff flow now provably receives SETUPs, dispatches them, and issues
Start Transfers that retire cleanly - and the endpoint still never goes
ready on the wire. The failure is inside the core's EP1-IN transfer
start, not the arm machinery the last session repaired.

### Cross-boot trace harvest: the retained region does not survive

`prev_boot_progress_code()` (trace.rs) plus the entry-time harvest and
the `--signal-prev-trace-gate 1|2|3` suppression gate (flasks
`--usb-signal-prev-trace-gate`) were built to publish the previous boot's
progress through the attach-delay/attach-presence channels. Findings:

- The first version required the boot marker as entry[0]; it is orphaned
  by `trace_reset_head_for_boot()` (written before the cursor reset), so
  the check always failed. Fixed by dropping the marker requirement.
- Suppression works and is jitter-immune: the mode-1/3 runs reset at
  entry (return +29 s from boot, no attach line) exactly when the
  harvested code failed the predicate.
- The harvest ALWAYS reads 0 cross-boot: two marker-only traces written
  by suppressed boots read back as code 0, and the 4 s/step attach-delay
  ladder (second step of the channel) stayed at the code-0 delay
  (`+14 s` attach, unchanged across four boots with identical artifacts
  and a pinned 0x9000a000 `.usb_trace` address). XBL/ABL or the boot
  image load scribbles the region between the PSCI reset and the next
  kernel entry, despite the NOLOAD design. The in-boot harvest
  (attempt N reads attempt N-1) remains the only working use of the
  retained region.

### Negative A/Bs this session (all standard flags + the option; -110 unchanged)

- `--smmu-disable` (SCR0.SMMUEN=0/CLIENTPD=1/WACFG=00, readback-verified
  by the attach): the SMMU is not the data-phase blocker.
- `--reset-endpoints` (rebuild both EP0 contexts on the host USB reset):
  the reset does not destroy the EP1-IN context in a way this rebuild
  fixes.
- The `pub`/`dstat` style attach-cycle readouts remain dead: DCTL
  Run/Stop stop/run cycles are host-invisible in this regime (confirmed
  again: zero 1-9 disconnect/re-attach lines in the dstat run).

### Reference checkout (user-provided)

`QRD-Development/android_device_qcom_lito` (the QRD device layer) points
at `a600000.dwc3`; the sibling `android_kernel_qcom_sm7250` carries the
stock lito USB DT (`lito-usb.dtsi`): core 133.33 MHz / HS 66.67 MHz
(matched by `usb_clock_plan`), `snps,disable-clk-gating`, TX FIFO total
27696 with `tx-fifo-resize`, QUSB2 param overrides `0x6c<-0x63`,
`0x70<-0x85`, `0x74<-0x17`, and the Apps-SMMU stream 0xE0 with the
0x90000000 DMA pool (matched by `.usb_dma`). The QRD device layer itself
is HAL/init only and not useful for the core failure.

### Next steps (priority order)

1. EP1-IN never goes ready despite clean Start Transfers: dump the core
   debug surface for the transfer state (GDBGEPINFO, DEPCMD2/DEPCMD3
   parity reads for EP1) into the mrad code ladder so the
   not-ready reason (resource vs context vs FIFO) names itself. The
   ladder now has spare resolution: codes 1-4 are provably below the
   current state, so new sub-states can extend the ladder past 6.
2. Re-test the `--android-resource-order` differential (32-endpoint
   SETTRANSFRESOURCE preallocation before SETEPCONFIG, the stock msm
   ordering) against the code-5/6 state; it directly probes the
   resource-allocation theory.
3. The QUSB2 PHY param overrides from the stock DT (0x6c/0x70/0x74) are
   not yet confirmed applied on our path; diff our hsphy init against
   `qcom,param-override-seq` and A/B via the mrad ladder.
4. Only `fastboot boot` was used; the working tree is uncommitted.

## 2026-08-31 (session 4): the "~5.6 s death" is the host's port reset, and the data phase is the only real blocker

### The reset that cuts every park/tail is not a watchdog we own

Every standard run since the GDSC repair returns to Android at ~T+41 s
(attach+26-27 s). Re-deriving the device-side timeline from that return
(Android boot ~= 21 s) shows the reset at attach+5-6 s in **every** run,
and the new experiments pin its identity:

- `--pon-readout force10` (+3 s attach shift): the attach moved to T+16 s
  and the reset moved with it (Android at T+43 s) - the reset is
  **attach-relative (attach+~5.5 s)**, not boot-absolute.
- `--bare-pullup` (no DWC3 core, no EP0, DMA-free): attach T+10 s, the
  host's read/64 timeout at attach+6 s, Android at T+37 s (attach+27 s) -
  **the reset fires with the DWC3 core completely out of the picture**.
- `--quiet-after 4` (all controller MMIO stopped from Run/Stop+4 s): the
  reset still fires at attach+5 s - our CPU is idle and uninvolved.
- `--swdd-skip`, and the `wd2` PMIC-watchdog readout (WD_RST_S2_CTL2
  reads 0x00-0x02, WD_EN=0): the secure-watchdog-disable SMC changes
  nothing and the PMIC PON watchdog is disarmed. The APSS watchdog is
  re-armed to 100 s/110 s by the first `wdt_pet`, so none of the three
  classic watchdogs is the actor.
- The new `sdis` gate (DCTL stop after the post-arm sample): the host's
  `-110` still printed at attach+5 s - **post-attach disconnects are
  host-invisible** (the Qualcomm session override; consistent with the
  old dstat finding), so the session cannot be detached to dodge the
  reset.

What happens at attach+5.2 s on the host is the first control transfer's
5 s timeout, and hub.c's new-scheme loop calls `hub_port_reset()` after
EVERY failed `get_bMaxPacketSize0` attempt. The reset's identity is
therefore: **the host's port reset (a real bus reset) at attach+5.3 s
kills the session/PMIC chain and reboots the handset ~0.2 s later**. The
`bootreason=watchdog` every run is XBL's decode of that chain, and the
handoff note "watchdog = 復帰リセットで毎回正常" was literally correct -
there never was a second, independent biter.

### Consequence: the death is a shadow of the data-phase failure

The host only resets the port because the read/64 times out. If the data
phase delivers inside the first 5 s, the host completes SET_ADDRESS and
the read/all well before attach+5 s and never resets the port - the
death never fires in a successful enumeration. Conversely the stable
park (which the ladder preempts: `probe_ep0_progress()` is true in the
current regime, so codes 5-6 never published a tail reset - every
"code 6 bucket" reading this week was the death bucket) is only cut
because of the same failure. **Fixing the data phase fixes the death,
not the other way round.**

### Host enumeration is 64-byte-first

The host is a 6.8 xHCI kernel using `use_new_scheme()`: the FIRST control
transfer is GET_DESCRIPTOR with wLength=64, retried once (GET_DESCRIPTOR_TRIES=2)
with a port reset in between. There is no 8-byte first read; the ladder
comment's "read/8 then read/64" story was wrong and is now corrected
(`diag_readout_code()` docs). Both harvested SETUPs are read/64 attempts.

### PON state is frozen at the recovery boot

The seq readout (code 6 = KPDPWR_OR_RESIN, the recovery-key entry) has
been constant across dozens of boots that each included warm resets -
including the death and `adb reboot bootloader`. No PON sequence has run
since the original recovery boot, so the PON reason block cannot name
the death. The readout channel itself works (the `wd2` run attached
~1 s earlier than the seq runs, exactly as the shorter delay predicts).

### New tools this session

- `--pon-readout <REG>` (kernel `FULLERENE_USB_PON_READOUT`): publishes
  one value through the attach-delay channel: `seq` (default, previous
  reset bucket), `imem` (IMEM restart-reason cookie 0x146ab65c: 1 step
  for the recovery chain's 0x77665500, 2 for zero, else 2+low 5 bits),
  `forceN` (N fixed 300 ms steps, the attach-shift knob used above), and
  the raw PON bytes `wd2|s1|s2|ctl|warm|soft` as (byte+1)*300 ms capped
  at 32 steps.
- The mrad tail restructured: 0.4 s post-arm settle, then the composite
  diag code published as 450 ms/code (1..9) before the reset; the
  in-loop stable-park short-circuit was folded into a single park
  decision (`ep1_data_phase_complete()` - the newest EP1 transfer-complete
  status == 0) so codes >= 5 can actually publish.
- The diag ladder extended to 9: 7 = an EP1 XferComplete event arrived
  after the arm, 8 = no completion but the data TRB's HWO was cleared
  (the core consumed the TRB, the transfer silently died), 9 = no
  completion and HWO still set (the core never consumed the armed TRB).
- The `sdis` gate (negative result, kept as a marker).

### A/B negatives this session (standard flags unless noted)

- `--xbl-ep0-in-data` (stock XBL's NORMAL TRB for the data phase; never
  run before): -110 unchanged, death bucket.
- `--smmu-disable` re-run WITH the mrad ladder (the earlier -110-only
  runs could not distinguish "translation fine" from "event ring broke
  under bypass"): still the death bucket - inconclusive again, because
  codes 5-9 cannot publish inside the attach+5.5 s death window.
- `--ep0-txfifo-fix` was already an effective A/B post-recovery
  (session 3): unchanged.

### Timing reality for readouts

Probe entry lands at ~attach-0.5 s (the handoff returns right after the
connect/U0 wait). The eval for gate runs is `entry+observe`; the
first-signal eval for non-gate runs is ~attach+0.2-0.5 s. The death at
attach+5.3-6 s caps every reset-timed publish: with the 450 ms ladder
only codes 1-5 land clearly ahead of it, 6 races, 7-9 are swallowed.
The Android boot adds +-0.5-1.5 s jitter, so sub-second ladder steps are
unreadable. **Any readout that needs more than ~4.5 s of live session
must ride the attach-delay channel of a later boot or wait for the data
phase to work.**

### Next steps (priority order)

1. Deliver the read/64 data phase inside the first 5 s. The arm retires
   cleanly and XferNotReady(CONTROL_DATA) fires on both attempts, so the
   failure is between the doorbell and the wire. Cheapest discriminators
   left: the 7/8/9 rungs via a stop-free publish (they are implemented
   but unreadable until a run reaches them through a faster eval - e.g.
   `--observe-secs 1` with the settle, where code 9 resets at
   attach+~4.9 s, 0.4 s ahead of the port reset), the GDBGLTSSM state at
   the sample (`gdbstop`-style), and the SMMU FAR journal for the EP0
   buffer fetch (the session-2 fsr-gate fault is still unexplained).
2. The -110/-71 alternation from earlier sessions means the arm
   sometimes WINS the race and the host then rejects the wire response:
   reproducing a -71 run with the 7/8/9 ladder would prove data left the
   core and move the search to the PHY/electrical layer (QUSB2 param
   overrides 0x6c/0x70/0x74 are still unverified).
3. Once enumeration succeeds, verify the death never fires (no port
   reset) and the stable park holds the session for the autonomous
   loop.
4. Only `fastboot boot` was used.
