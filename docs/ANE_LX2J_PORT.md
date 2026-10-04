# Fullerene on the ANE-LX2J (Huawei P20 Lite, Kirin 659)

[Japanese version](ANE_LX2J_PORT.ja.md)

Status of the port that follows the bootloader unlock
([ANE_LX2J_BOOTLOADER_UNLOCK_20261002.md](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.md)).
Everything below was measured on the device or read from its own device tree on
2026-10-02; nothing here is a datasheet guess.

## The boot contract (measured)

The `kernel` partition holds an **Android boot image (header v0)**, not a bare
arm64 `Image`, and the device tree arrives from its own `dts` partition in `x0`:

| field | value | source |
|---|---|---|
| magic | `ANDROID!` | stock kernel partition dump |
| `kernel_size` | gzip-compressed payload (stock: 11,681,787) | dump |
| `kernel_addr` | `0x00480000` | dump - where the LK places the decompressed Image and enters it |
| `tags_addr` | `0x07e00000` | dump |
| `page_size` | `2048` | dump |
| `ramdisk_size` | `0` | the ramdisk is its own partition |
| cmdline | `loglevel=4 coherent_pool=512K ... androidboot.selinux=enforcing buildvariant=user` | dump |

The arm64 `Image` inside is entered at its first byte, whose `code0` branches
over the 64-byte Image header to +0x40. So the Rust payload links at
`0x00480000 + 0x40 = 0x00480040`.

Proven, not assumed: the stock kernel was unpacked, repacked by
`mkane_bootimg.py` (v0 header, 2048-byte pages, gzip payload), flashed with
`fastboot flash kernel`, and **it booted**. Every header field survived the
round-trip except `kernel_size`, as expected.

## What the port adds in-tree

| piece | where |
|---|---|
| platform name | `flasks build --arch aarch64 --platform ane` -> `FULLERENE_AARCH64_PLATFORM=ane` |
| link base | `fullerene-kernel/build.rs` `aarch64_linker_script` -> `0x00480040` |
| entry constant | `entry::LINK_ENTRY` -> `0x0048_0040` (relocation delta is computed against it) |
| early console | `platform::ane::UART_BASE` = `0xfdf02000` (`/amba/uart@fdf02000`, `arm,pl011`, `status = "ok"`) |
| DTB fallback | none - the LK supplies the tree in `x0`, and a fallback would point into peripheral space |
| black box window | `blackbox.rs`: `pstore-mem` @ `0x34800000`, size `0x100000` |
| first marker | `blackbox::mark_entry()` -> code `E`, written before the first device register |
| reserved-memory node | looked up as `pstore-mem` (Bramble's trees use `ramoops_region`) |

## The observation channel: reserved RAM, because nothing else is readable

This board has no serial the host can reach and, for an unattended run, no
screen either. What it does have is a full `ramoops` node in its device tree:

```text
/reserved-memory/pstore-mem   reg = <0x0 0x34800000 0x0 0x100000>
/ramoops                      compatible = "ramoops"
    record-size  = 0x20000      console-size = 0x80000
    ftrace-size  = 0            pmsg-size    = 0x20000
    dump-oops    = 1            ecc-size     = 0
```

Linux's `fs/pstore/ram.c` lays those zones out in order
(`dump_mem_sz = size - console_size - ftrace_size - pmsg_size`), so:

```text
0x00000  dmesg records, 3 x 0x20000
0x60000  console zone   0x80000
0xE0000  pmsg zone      0x20000   <- the durable text log
0x100000 end
```

The durable log is the Bramble module's: one pstore record per byte, appended
behind the `persistent_ram_buffer` header (`sig = "DBGC"`, `start = 0`,
`size = n`), so the stock kernel's ramoops keeps the zone instead of
re-initialising it. A later boot - Android with root, or Fullerene itself -
reads it back.

The compiled-in values are the platform's, not Bramble's: `RAMOOPS_REGION_BASE`,
`CONSOLE_ZONE_OFFSET`, `PMSG_ZONE_OFFSET`, and the two zone sizes are cfg-gated
on `fullerene_aarch64_ane`.

## The boot ladder

| level | question | how it is answered |
|---|---|---|
| L0 | did the loader enter our image at all? | an `E` record in the pmsg zone, or nothing |
| L1 | did early bring-up survive the first register write? | records after `E` (`b` = the DT claim, `r` = region seen) |
| L2 | did the DT parse and the runtime start? | later stage codes |
| L3 | did anything device-facing come up? | USB enumeration changes; console mirror content |

`mark_entry()` is deliberately placed *before* the first device register access -
the console's clock gate, then the UART write - so L0 is answerable even when
everything after it dies.

## Host-side preflight (no device needed)

`ane_qemu_preflight.sh` runs the real Image on QEMU's generic ARM virt machine.
QEMU does not model this board, and that is the point: neither the console's
clock gate (CRG+0x20 at `0xfff35020`) nor the PL011 the kernel inits at
`0xfdf02000` exists there, so the first device register write is *rejected* and
QEMU logs the address. Getting that far requires the entry stub,
the static-PIE relocations, BSS zeroing, the Rust entry and `mark_entry` to have
all completed - the mark's own accesses land in QEMU's unassigned low memory,
which is silently swallowed rather than rejected, so the first *rejected* access
is the first device write.

Measured on the built image, two epochs (the gate was added 2026-10-03):

```text
first rejected device access: 0xFFF35020      (CRG+0x20, console clock gate; current)
first rejected device access: 0xFDF02030      (PL011 CR, UART_BASE + 0x30; before the gate)
Data Abort  FAR 0xfdf02030  ELR 0x400d1724    (older build, image loaded at 0x40080000)
```

`0x400d1724` is the `str wzr, [x8]` immediately after the inlined `mark_entry`
in `aarch64_rust_entry`, and the disassembly of that mark shows the device's own
constants (`mov w9, #0x34800000`, `#0xe0000` zone offset, `0x43474244` "DBGC"
signature, `0x45` = 'E'). So this is a mechanical check that the compiled image
executes the port's early path, and it runs on any host with qemu-system-aarch64.

## The first test: a thirty-instruction probe

`ane_mark_probe.sh` assembles a payload that does exactly one thing: write a
single `persistent_ram_buffer` record (`"\n001:E"`, size 6) into the pmsg zone
and halt. No MMU, no stack, no relocations, no BSS, no C. It separates "the
loader accepted our image and entered its first instruction" from everything
Fullerene does afterwards, so the first device run cannot be misread.

Its correctness is checked on the host rather than assumed. The same source is
assembled twice - once for the device's pmsg zone (`MARK_BASE=0x348e0000`) and
once pointed into emulated RAM (`MARK_BASE=0x400e0000`) - run under
`qemu-system-aarch64`, and the bytes are read back out of the guest with QMP:

```text
00000000400e0000: 0x44 0x42 0x47 0x43 0x00 0x00 0x00 0x00   "DBGC", start 0
00000000400e0008: 0x06 0x00 0x00 0x00 0x0a 0x30 0x30 0x31   size 6, "\n001"
expected == got  -> PASS
```

(Two QEMU interface details, measured: HMP rejects absolute paths for
`pmemsave`, and `size` takes no suffixes - so the check reads memory with
`xp /64bx` through `human-monitor-command` over QMP instead.)

Artefacts: `artifacts/fullerene-ane-mark.img` (gzip) and `-raw.img`. The
expected record after a power cycle is `\n001:E`, size 6, at `0x348e0000`.

## Commands

```bash
# one command from source to a verified, flashable image:
#   flasks build (platform ane) -> package -> mechanical validation
bash ~/dev/p20-root/ane_build.sh

# the same steps individually
cargo run -q -p flasks -- build --arch aarch64 --platform ane   # writes the .Image
bash ~/dev/p20-root/ane_pack.sh <the .Image>                    # v0 + gzip, plus raw and lz4 controls
python3 ~/dev/p20-root/validate_ane_image.py <the ELF> artifacts/fullerene-ane.img <the .Image>

# host-side preflight: run the image on QEMU's ARM virt machine and check where
# execution first touches a device address (see below)
bash ~/dev/p20-root/ane_qemu_preflight.sh <the .Image>

# the thirty-instruction L0 probe (self-checked under QEMU; try this first on the device)
bash ~/dev/p20-root/ane_mark_probe.sh

# one device attempt; default is fastboot boot (leaves the stock kernel flashed)
bash ~/dev/p20-root/ane_experiment.sh artifacts/fullerene-ane.img first-gzip --wait 150

# read back what that boot left behind (root shell via the 4.4.23 exploit)
bash ~/dev/p20-root/ane_read_pstore.sh
```

## Device session prerequisites (hands needed once)

The device was left wedged mid-Android-boot by the last root-shell session
(`12d1:107e`, MTP + Mass Storage only, no adb). To get back to a testable
state: hold power ~10 s, boot Android, then enable USB debugging again
(mode "ファイル転送" plus the on-device allow dialog). After that the scripts
above run unattended with no fastboot button combos needed.

## Next

1. First device run: `fastboot boot artifacts/fullerene-ane-mark.img` - the
   thirty-instruction probe - power-cycle, and read the pstore window.
   `\n001:E` appearing proves the loading contract end to end; only then the
   full `fullerene-ane.img`. Capture the stock boot pattern first
   (`capture_boot_pattern.sh`) as the differential.
2. If the payload is rejected: try `fullerene-ane-raw.img` (no gzip) - the
   loader's decompression contract is the one thing this flow has not measured.
3. If `E` appears but nothing after: narrow L1 by moving the UART write later
   (the mark is already ahead of it) or by choosing another PL011.
4. Then the runtime: the ANE's USB is `hisi-usb2phy` @0xfe000000 +
   `usb_otg_ahbif@ff200000` (not DWC3 - the Bramble USB driver does not
   transfer), the panel is `boe_otm1911a_5p84_1080p_video` with backlight
   `panel_blpwm` @0xffd75000.
5. Once the payload format is measured, move packaging into the repo as a v0
   sibling of `patch_bramble_boot_image` in flasks, so
   `flasks build --platform ane --boot-template <stock kernel dump> --boot-output <img>`
   emits the flashable file directly. Deferred until then on purpose: freezing a
   guessed compression contract in-tree is how a wrong default becomes permanent.
