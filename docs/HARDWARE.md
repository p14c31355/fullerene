# Real Hardware Compatibility

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

4. An IRQ-only DWC3 probe successfully enumerated as `18d1:4ee0` and stayed
   present until the intentional roughly 120-second recovery watchdog. This
   established that the DWC3 controller IRQ path and the basic gadget
   handoff could work on the device. Adding the power-event IRQ did not cause
   an early failure either.

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

8. The resulting no-PDC runtime image enumerated successfully at SuperSpeed
   as `18d1:4ee0`, remained present for about 95 seconds, and then reset at
   the expected watchdog boundary. No `-110` appeared in that run. This is a
   substantial improvement over the 35--38 second PDC-enabled disconnect and
   strongly implicates the runtime PDC IRQ ownership mismatch as a major
   cause of the earlier failure.

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

   The host observed SuperSpeed enumeration at approximately 10:09:13 and
   completed device identification at 10:09:15. The device remained present
   until approximately 10:20:16, or about eleven minutes. This exceeds the
   previous watchdog-boundary failure by a wide margin and confirms that the
   EP0 progress-to-stable transition works on the real Bramble. The eventual
   disconnect was not issued by the probe's no-progress recovery branch, so
   the remaining issue is now the post-enumeration idle/link or device-side
   reset behavior rather than the initial EP0 descriptor exchange.

10. The retry-enabled probe was then booted again with `fastboot boot` (artifact
    `/tmp/fullerene-bramble-stable-probe-retry.img`, SHA-256
    `8b49f46dfd40686d696008832fd1fd368ea1ed51248bcfa700c3ed4ad5d3584f`).
    This run enumerated as SuperSpeed at approximately 10:36:28--10:36:30,
    and `lsusb -v` completed the full Device, Configuration, Interface,
    endpoint, and BOS descriptor reads. The host then kept the device present
    for at least 50 seconds without a new `-110` error. The same session also
    answered the actual Fastboot protocol: `fastboot devices -l` identified
    serial `26191JECB00076`, `fastboot getvar product` returned `bramble`, and
    `fastboot getvar all` completed successfully. This is the first complete
    real-device boundary from physical attach through descriptor enumeration
    and Fastboot control transfers.

The relevant official references are:

- [Android Lito USB device tree](https://android.googlesource.com/kernel/msm-extra/devicetree/+/refs/tags/android-11.0.0_r0.56/qcom/lito-usb.dtsi), including the DWC3 IRQ order, DMA pool, clocks, GSI offsets, and bus resources.
- [Qualcomm PDC irqchip](https://android.googlesource.com/kernel/common/+/ff0000fe82f45/drivers/irqchip/qcom-pdc.c), including `IRQ_ENABLE_BANK`, PDC trigger conversion, parent SPI configuration, and pending-state clearing.
- [Linux Qualcomm DWC3 glue](https://github.com/torvalds/linux/blob/master/drivers/usb/dwc3/dwc3-qcom.c), including the wakeup-only PHY IRQ setup and `IRQF_NO_AUTOEN` behavior.
- [Android Qualcomm PMIC Type-C driver](https://android.googlesource.com/kernel/common/+/c13159a588818/drivers/usb/typec/qcom-pmic-typec.c), which performs PMIC initialization at probe and handles the child interrupt through a threaded path.
- [postmarketOS installation targets](https://postmarketos.org/install/); Bramble was not listed as an official device-specific target in this comparison.

### Current status and next boundary

The current result is not yet an indefinitely stable USB gadget across every
independent handoff attempt, but one retry-enabled run survived at least
eleven minutes and a later run survived at least 50 seconds while also
answering Fastboot commands. It is no longer failing at the original short
descriptor-transfer interval. The next hardware evidence should come from the
retained RAM trace around these boundaries:

```text
SETUP received
descriptor queued
IN complete
STATUS OUT
EP0 rearm
```

The next implementation boundary is to preserve the official distinction
between runtime device operation and suspend/wakeup ownership while identifying
the cause of the post-idle disconnect. Priority is retained-trace correlation
with DWC3 link-status/suspend/hibernation events, followed by QMP/Type-C
runtime state, clock and reset sequencing, GSI/event handling, DMA/SMMU fault
visibility, and the Linux-equivalent EP0 command/error/re-arm paths. All
further Bramble tests continue to use the audited stock template and
`fastboot boot`; partition flashing is not part of this workflow.

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
