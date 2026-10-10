# ANE-LX2J bootloader unlock — 2026-10-02

[Japanese version](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.ja.md)

The AArch64 port currently targets the Pixel 4a 5G (Bramble) through `fastboot
boot`. The next target is the Huawei P20 Lite (ANE-LX2J, Kirin 659 / hi6250).
Unlike Bramble, this device never exposes a temporary-boot path, so Fullerene has
to go through the `kernel` partition — and that requires an unlocked bootloader.
This document records how the unlock was achieved, in software only, and what
the boot contract looks like now that it is open.

## Target

| Item | Value |
| --- | --- |
| Device | Huawei P20 Lite / ANE-LX2J / Kirin 659 (hi6250) |
| Serial | `SCV7N18927000473` |
| Build when unlocked | `ANE-LX2J 8.0.0.151(C635)`, Android 8.0.0 |
| Kernel | `4.4.23+ #1 SMP PREEMPT Fri Mar 1 00:11:18 CST 2019` |
| Method | Software only — no disassembly, no test points, no paid unlock service |
| Cost | Four firmware packages (~240 JPY in marketplace credits) |

## Why the unlock was the blocker

The bootloader enforces three consecutive gates on `fastboot oem unlock`:

1. `FBLOCK` in the `nvme` partition must be cleared.
2. `OEM unlocking` must be enabled in Android's developer options.
3. The supplied code must hash to the value stored in `USRKEY` (also in `nvme`).

Every one of those lives in a partition, which means every one of them needs root —
and root on this device does not imply the ability to touch block devices.

### The SELinux wall

CVE-2019-2215 yields `uid=0` with all capabilities, but the process stays in
`u:r:shell:s0`. Every block device open fails: `/dev/block/bootdevice/by-name/*`,
`/dev/nve0`, and the `nvme` partition itself. `setenforce`, `/sys/fs/selinux/enforce`,
`/proc/kallsyms` (all zeros even as root), `dmesg`, and `/proc/kcore` are all closed
off as well.

The exploit's built-in escape is to overwrite the AV-cache nodes belonging to our
SID so that permission checks for that SID return "allowed". That needs one
address:

```c
kaslr_offset   = kernel_read_ulong(task_struct + SCHED_CLASS_OFFSET) - FAIR_SCHED_CLASS;
avc_cache_addr = kaslr_offset + AVC_CACHE;
```

Read carefully, this means **only the difference `AVC_CACHE - FAIR_SCHED_CLASS`
matters**. The KASLR slide is derived from whichever `FAIR_SCHED_CLASS` is compiled
in, and cancelled out by the same constant in `AVC_CACHE`. No absolute address and
no 2 MiB alignment check is required — which turns an impossible search into a
measurable, sweepable single number.

### Measuring that number

Four firmware packages were obtained from a marketplace, unpacked, and their
`KERNEL` blocks reconstructed into symbol-bearing ELFs with `vmlinux-to-elf`:

| Package | Kernel build date | `avc_cache - fair_sched_class` |
| --- | --- | --- |
| 8.0.0.110(C635) | 2018-04-18 | `0x132e838` |
| 8.0.0.202(C719) | 2018-05-16 | `0x134a838` |
| 8.0.0.151(C01) all cn | 2018-06-11 | `0x134a838` |
| 8.0.0.127(C719) / kddi_jp | 2019-04-25 | **`0x134c838`** |
| exploit's built-in | unknown | `0x135c838` (never worked) |

The device runs a kernel built 2019-03-01, between the last two rows. The layout
barely moves over that stretch, so `0x134c838` was tried first on hardware — and it
worked: SELinux was no longer able to stop the process, and `dd` on the `nvme`
partition succeeded.

Note that the value is not a function of the build date alone; it is a property of
the linked image's `.text`/`.bss` layout. Two kernels a month apart shared the same
difference in this set.

### Race rules (measured)

| Change to the exploit | Result |
| --- | --- |
| none | wins |
| constant immediates only | wins (used for the sweep) |
| swapped call target, argv handling, added printf | loses every time |

The only edits that survive are ones that cannot move the linker's layout. The race
also depends on system load: it wins reliably on an idle device and loses in the
first minutes after boot or while adb is being hammered.

## The unlock itself

1. `hisi-nve w FBLOCK 0` — clears all seven copies in `nvme`, verified by re-reading.
2. Android's `OEM unlocking` toggle, which was greyed out. The community fix held:
   connecting the device to the internet for a while made it selectable. With no
   `persistent_data_block` service and no oemlock HAL on this ROM, that toggle is
   the only writer of the corresponding NV state.
3. `USRKEY` — **`hisi-nve`'s write silently does nothing here.** It prints
   `Hashing USRKEY...` and reads back the old value. The working method is to dump
   the `nvme` partition, place `SHA256(<code>)` into the value fields directly, and
   `dd` the image back:

   ```
   entry layout : name(8 bytes) + 12  ->  value (104 bytes)
   USRKEY copies: 7, at 0x29d84 + k*0x20000
   ```

   Verified by re-reading the partition: seven copies, all matching.

4. `fastboot oem unlock 0123456789ABCDEF` →
   `(bootloader) The device will reboot and do factory reset...` and the device
   unlocked.

### Verification

| Property | Before | After |
| --- | --- | --- |
| `ro.boot.flash.locked` | `1` | `0` |
| `ro.boot.verifiedbootstate` | `GREEN` | `ORANGE` |

Both values are handed to the kernel by the bootloader at boot; `0`/`ORANGE` is the
unlocked state.

Note that `FBLOCK=0` alone does not move the bootloader's lock state. It removes a
*slow-path security check* — the one that produced `check password failed` before
the bootloader would even discuss unlocking — and opens the way to the two gates
above. Clearing it is necessary but not sufficient.

## Boot contract for the AArch64 port

The `kernel` partition on this device does **not** hold a bare arm64 `Image`. It
holds an Android boot image:

| Field | Value |
| --- | --- |
| magic | `ANDROID!` |
| `kernel_size` | `0x00b23ffb` (11,681,787) — gzip-compressed payload |
| `kernel_addr` | `0x00480000` — the LK loads the kernel here |
| `tags_addr` | `0x07e00000` |
| `page_size` | `2048` |
| `ramdisk_size` | `0` — Huawei splits the ramdisk into its own partition |
| cmdline | `loglevel=4 coherent_pool=512K page_tracker=on slub_min_objects=12 unmovable_isolate1=2:192M,3:224M,4:256M printktimer=0xfff0a000,0x534,0x538 androidboot.selinux=enforcing buildvariant=user` |

The device tree is not appended to the kernel either: the LK supplies it from the
`dts` partition (`mmcblk0p34`, 29,360,128 bytes).

Fullerene's aarch64 image builder already emits a Linux arm64 `Image` header
(`ARM\x64` magic, `text_offset 0x80000`). To boot on this device it additionally has
to satisfy this contract:

- be linked for the load address the LK uses (`0x00480000` observed in the header;
  the actual placement should be confirmed on hardware before trusting it),
- be wrapped in an `ANDROID!` boot-image header with the same page size and a
  cmdline that does not conflict with the LK's,
- expect the device tree in `x0` rather than as an appended blob.

The entry conditions themselves match the Linux protocol, which the Bramble port
already follows.

## Artefacts

Backups, the winning exploit build, and a verification record:

```
~/dev/p20-root/backups/ANE-LX2J_20261002/
    nvme_BEFORE_FBLOCK_write.bin   factory state, before any change
    nvme_after_FBLOCK0.bin         FBLOCK cleared
    nvme_usrkey_installed.bin      the image actually written (FBLOCK=0 + our key)
    nvme_CURRENT_unlocked.bin      dumped from the device after the unlock
    frp_original.bin               untouched
    misc.bin  oeminfo.bin
    cve-2019-2215_working_0x134c838
    kernel_8.0.0.110.elf
    unlock_verification.txt
    MD5SUMS.txt                    `md5sum -c` from inside the directory
```

Restore:

```bash
dd if=nvme_usrkey_installed.bin of=/dev/block/bootdevice/by-name/nvme bs=4096
sync
```

Primary sources used: the kernel tree identified by the device's own
`/proc/version`, the CVE-2019-2215 PoC, `hisi-nve`, and the four firmware packages
named in the table above.

## Next

1. Confirm the LK's kernel load address and entry register state on hardware.
2. Repack Fullerene's aarch64 image into an `ANDROID!` boot image for this device.
3. Bring-up without serial: framebuffer first, then the USB controller, so the
   panic path used on Bramble can be reused.
