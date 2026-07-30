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
This keeps an uncompleted PCIe load out of the boot path. AHCI and NVMe are not
attached at boot until their kernel adapters can publish usable block devices;
their former adapters reset hardware but returned zero-sized placeholder
devices.

PCI resource assignment preserves every non-zero firmware BAR without issuing
the destructive all-ones size probe. Fullerene probes and assigns only a BAR
whose address is genuinely zero. This matters for firmware-initialized USB and
card-reader endpoints whose state must survive until their explicit rescan.

Reference: Linux [`drivers/usb/host/pci-quirks.c`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/pci-quirks.c)
and [`drivers/usb/host/xhci-ext-caps.h`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/xhci-ext-caps.h).

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
