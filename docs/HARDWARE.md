# Real Hardware Compatibility

## InsydeH2O Firmware (June 2026)

Running on real hardware with InsydeH2O UEFI firmware required three fixes:

1. **Do not call `SetMode()`**: InsydeH2O's GOP implementation changes `frame_buffer_base` and/or invalidates `pixels_per_scan_line` after `SetMode()`, causing "backlight only" (no display output). The bootloader now uses the current mode as-is.

2. **BGR/RGB byte-order in `rgb888_to_pixel_format()`**: The color conversion function had its byte-order arguments reversed for BGR vs RGB pixel formats. For BGR hardware (common on Intel GOP), `rgb_pixel(r,g,b)` produces the correct LE memory layout `[b,g,r,0]` which BGR interprets as B=b, G=g, R=r. The fix corrects the mapping: BGR/PixelBitMask formats use `rgb_pixel(r,g,b)` while RGB formats use `rgb_pixel(b,g,r)`.

3. **Skip `safe_map_page` WC remap on real hardware**: The kernel's `safe_map_page` (via `map_page_4k_l1`) attempts to split the boot-phase 2MB/1GB huge-page WB mapping into 4KB WC pages for the framebuffer. On InsydeH2O this operation breaks the mapping entirely, making the framebuffer inaccessible. The fix relies on the existing boot-phase huge-page identity mapping (WB via PAT/MTRR), which is already functional and confirmed working via direct `write_volatile` tests.

## Intel Wildcat Point-LP USB (8086:9cb1 / 8086:9ca6)

The target machine exposes an xHCI controller at `00:14.0` and an EHCI
companion at `00:1d.0`. Before the first xHCI BAR0 load, Fullerene now:

1. moves the endpoint to D0;
2. enables the Intel USB3 terminations (`USB3_PSSEN`, config `0xd8`), then
   routes the firmware-declared USB2 ports (`XUSB2PR`, config `0xd0`) to xHCI;
3. disables standard ASPM and enables PCI memory decoding/bus mastering;
4. maps BAR0 uncached and reads the capability header as a 32-bit register;
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

The Realtek RTS5249 reader (`10ec:5249`) is matched by vendor/device identity,
because PCI class `0xff` is a real vendor-specific class rather than a driver
wildcard. Boot registers the reader without accessing its device registers.
The explicit `sd_rescan` command is the first BAR0 MMIO boundary; a successfully
initialized SDXC then appears dynamically as `/dev/sd0` without being mounted.
This keeps an uncompleted PCIe load out of the boot path. AHCI and NVMe are not
attached at boot until their kernel adapters can publish usable block devices;
their former adapters reset hardware but returned zero-sized placeholder
devices.

Reference: Linux [`drivers/usb/host/pci-quirks.c`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/pci-quirks.c)
and [`drivers/usb/host/xhci-ext-caps.h`](https://github.com/torvalds/linux/blob/master/drivers/usb/host/xhci-ext-caps.h).

## Future Platforms

In the future, we plan to add compatibility notes for:

- **ThinkPad** series
- **Framework** laptops
- **Intel** reference platforms
- **AMD** platforms
- **QEMU** (already supported; detailed notes to be added)
