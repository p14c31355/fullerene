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

## Verified touchscreen evidence

The LineageOS Hi6250 vendor kernel contains an ANE touchscreen description at
`arch/arm64/boot/dts/auto-generate/config/hi6250/huawei_touchscreen.dtsi`.
This is source evidence, not a register guess.  Its relevant values are:

| property | value |
|---|---|
| parent compatible | `huawei,touchscreen` |
| transport | I2C, bus `1` |
| controller | Synaptics, address `0x70` |
| touch range | `x_max=2200`, `y_max=3800` |
| reset GPIO | `gpio10 4` |
| interrupt GPIO | `gpio1 0`, falling edge (`irq_config=3`) |
| analog supply | `ldo17` |

The same vendor tree contains `synaptics-dsx.dtsi`, but that node is marked
`status = "disabled"` and uses a different address (`0x20`).  It must not be
selected for ANE without confirming the active boot DT.  Fullerene's ANE DT
inventory now reports the active `huawei,touchscreen` node as an input
capability.  This does not claim that the I2C transport or Synaptics report
parser is implemented yet; those remain the next driver work.

Source: `https://github.com/LineageOS/android_kernel_huawei_hi6250/tree/lineage-17.1/arch/arm64/boot/dts/auto-generate/config/hi6250`

The same source exposes an important unresolved dependency: its public
`hisi_6250_i2c.dtsi` defines DesignWare controllers 0, 2, 3, and 4, but not
controller 1.  The touchscreen node nevertheless requests `bus_id = 1`.
Therefore the touchscreen bus may be supplied by an omitted product overlay,
secure firmware, or another Hi6250 I²C description.  Fullerene must not drive
`0xFFD72000` (or any other guessed base) until the active ANE DT or measured
boot evidence identifies controller 1.  The next bring-up diagnostic is an
active-DT dump/parse that resolves this bus-owner mismatch.

## ANE active-DTB correction

The extracted ANE DTB at `/home/placeless/dev/p20-root/firmware/ane_dtb/fdt.dtb`
has now been checked directly through its property dump.  It contains an
enabled DesignWare controller at `/amba/i2c@FFD73000`:

| property | value |
|---|---|
| compatible | `hisilicon,designware-i2c` |
| MMIO | `0xFFD73000`, size `0x1000` |
| IRQ | 120, level 4 |
| bus-id | 2 |
| status | `ok` |
| clock-rate | `0x0124f800` (19.2 MHz) |

The same active DTB marks all three legacy touchscreen candidates under this
controller disabled: Cypress `cyttsp4` at `0x1A`, Sharp `shtsc` at `0x18`, and
Synaptics DSX at `0x20`.  It also contains a separate Huawei `ts_kit` vendor
description for the actual panel path:

| property | active evidence |
|---|---|
| node | `/ts_kit/touchscreen@38` |
| compatible | `fts` |
| bus | `i2c`, `bus_id = 0` at the vendor-framework level |
| slave address | `0x38` |
| chip ID | `0x54` |
| coordinates | `x_max = 1080`, `y_max = 2280`, multitouch same range |
| reset / IRQ | `ts_kit` phandles `0x28e` / `0x28f`, `irq_config = 3` |
| supplies | phandles `0x5f` / `0x60`, regulator-controlled |

This is stronger active-DTB evidence than the unrelated vendor-variant
Synaptics `bus 1 / 0x70` description, but it is not yet a usable Fullerene
driver contract: the local kernel source tree does not contain the matching
`fts`/`ts_kit` implementation or its report protocol.  The next safe step is
to recover that vendor driver/firmware interface or identify the controller
from a stock diagnostic; no I²C transaction or reset write is inferred from
the metadata alone.

Stock ANE boot logs provide a more specific implementation clue.  The stock
`ts_kit` registers `gt1x_ts_module_init`, reports driver version
`v1.6<2017/05/20>`, uses reset GPIO `145` and IRQ GPIO `224`, and performs a
`gt1x_chip_detect` after power-on/reset.  Repeated stock logs also show the
watchdog initialization writing three bytes to controller register `0x8041`
(`gt1x_i2c_write ... addr:8041 bytes:3`), followed by retries and eventual
power-off when communication fails.  This confirms that the runtime stock
path selects the Goodix GT1x family despite the generic `fts` DT node name.
It does not by itself prove the register values, touch report layout, or safe
Fullerene reset/power sequence; those remain unimplemented until the actual
GT1x source or a controlled stock trace is recovered.

The public Goodix GT1x reference and current Linux driver define the reusable
8-byte contact layout as `id`, little-endian `x`, little-endian `y`,
little-endian contact width, and one reserved byte, preceded by a status byte
whose low nibble is the contact count and whose `0x80` bit marks valid data.
Fullerene now has an allocation-free parser for this format in
`resonance::touch`; it is unit-tested but is not connected to ANE I²C or IRQ
handling.  This separates safe packet decoding from the still-unverified
transport, reset, power, and controller-identification work.

The common touch contract now also exposes allocation-free GT1x framing
helpers: register addresses are emitted big-endian and the post-report clear
is exactly `[0x81, 0x4e, 0x00]`.  This remains a pure byte-contract layer; it
does not perform an I²C transaction or claim the ANE GPIOs.

The recovered downstream `gt1x_generic.c` also removes the last uncertainty
from the reset-status write: it sends `[0x00, 0x56, 0xaa]` to `0x8041`, then
`[0xaa]` to `0x8040`.  Its non-incell reset sequence drives reset low for
2 ms, high for 2 ms, waits 8 ms, drives the IRQ line low for 50 ms, and then
returns the IRQ GPIO to input mode.  The configured interrupt table selects
falling edge (`GTP_INT_TRIGGER = 1`).  These are now recorded as source-backed
contracts; the ANE backend still does not execute them.

The active DTB resolves the touch supplies to PMIC LDO17 (`phandle 0x5f`,
control register `0x25`) at `3.0 V` and LDO4 (`phandle 0x60`, control
register `0x19`) at `1.8 V`.  The stock driver declares both supplies as
regulator-backed.  These values are inventory contracts only: Fullerene has
not enabled either rail or written the PMIC.

Both rails are now represented by the reusable common regulator descriptor.
The vendor PMIC DTS gives LDO17 `hisi-vset = <0x4e 0x07>` and LDO4
`hisi-vset = <0x42 0x07>`, with a 20 ms off-on delay and 120 us enable time.
The descriptor records those values, the 1-bit enable mask, and the target
target voltage. It is metadata only and does not imply PMIC ownership.
The common descriptor also preserves the vendor `eco_mode_mask = 0x10`.
The vendor enable path performs a read-modify-write of the control register
and applies the off-on delay before enabling; Fullerene has not connected this
operation to hardware yet.

The common DesignWare observation layer now exposes read-only snapshots of
`IC_CON`, `IC_TAR`, `IC_ENABLE`, `IC_STATUS`, FIFO levels, and component
identity registers. The ANE touch backend provides an unsafe wrapper for
I2C0, but it is not called during boot: the active DTB marks I2C0 disabled,
so clock/MMIO ownership must be established before any snapshot read.

An additional local downstream source was found at
`/home/placeless/dev/sprd-sharkl3-src/drivers/input/touchscreen/gt1x_ts`.
Its `gt1x.c` and `gt1x_generic.c` independently confirm the protocol: read
`0x814E`, require bit `0x80`, use the low nibble as the finger count, read
8-byte contacts, append key and checksum bytes, verify the additive checksum,
then write a zero byte to `0x814E`.  Its transport uses a 16-bit big-endian
register prefix and an I²C repeated-start read.  This is source evidence for
the generic parser/transport contract, not proof that the ANE I²C0 controller
can safely be enabled.

The vendor I²C DTS also provides the controller-level contract: I²C0 uses
reset window `0xFFD7E000`, enable/disable/status offsets `0x20/0x24/0x28`,
reset bit `3`, and `delay-off = 300`; its clock names are `clk_i2c` and
`apb_pclk`.  These values are now retained as ANE inventory constants only.
The generic DTS marks I²C0 disabled (and also marks I²C2 disabled), while the
active ANE DTB overlay independently enables I²C2.  Therefore this DTS is
not evidence that Fullerene may enable I²C0 during boot.

The corresponding downstream pin descriptions are also explicit: GPIO183
and GPIO184 use mux offsets `0x14` and `0x18`, both with function value `1`.
Their electrical configuration offsets are `0x1c` and `0x20`; the function
configuration value is `0`, with pull/drive/slew fields separately described
by the pinctrl provider.  These values are recorded as read-only metadata;
Fullerene does not yet write the pinctrl registers.

The active DTB binds I²C0 `pinctrl-0` to phandles `0x49/0x4a/0x4b/0x4c`
(GPIO183/184 mux and configuration) and `pinctrl-1` to the corresponding idle
phandles `0x4d/0x4e/0x4f/0x50`.  A host regression test checks these phandle
bindings and the six source-backed pin values against the extracted active-DTB
property file.  This protects the I²C0 contract from accidental I²C2 or idle
state substitutions, but does not enable the disabled controller.

The active DTB phandles resolve to GPIO providers
`/amba/gpio@e8a1d000` (reset) and `/amba/gpio@fff1d000` (IRQ).  Together
with the repeated stock-kmsg reports, this records reset GPIO `145` and IRQ
GPIO `224` as the device contract.  The DT GPIO flags do not, by themselves,
prove reset active level or IRQ edge/polarity, so neither is connected to a
runtime backend yet.

The source-tree search was repeated across the complete local `p20-root`:
the only I²C0 runtime references found are the DT clock/reset properties and
generic Linux `i2c_transfer()` call sites.  No HiSilicon DesignWare adapter
implementation, `clk_gate_i2c0` provider write path, or stock touch-side
clock/reset ownership function is present in the recovered source tree.
The stock logs prove that the GT1x power/reset phase runs, but do not expose
which firmware/kernel owner performed the I²C0 setup.  Therefore the missing
runtime ownership sequence remains an evidence gap, not permission to infer
one from the static DT.

The bus mapping has now been cross-checked against the active DTB.  The
`ts_kit bus_id = 0` value corresponds to `/amba/i2c@FFD71000`, whose DTB
properties are `bus-id = 0` and `status = "disabled"`.  The controller already
registered by Fullerene is `/amba/i2c@FFD73000`, `bus-id = 2`; it must not be
treated as the touchscreen bus merely because it is the only active DesignWare
I²C controller currently exposed by the ANE port.  Stock sensor logs also use
framework bus number 0, consistent with this distinction.  Until ownership of
`FFD71000` and its clock/reset/pinctrl contract is recovered, the touch
backend remains disabled and no I²C2 touch probe is permitted.

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

## USB EP0 source comparison and current recovery boundary

The ANE USB responder is based on the Synopsys DWC OTG/DWC2 register
interface.  The local HiSilicon source tree provides the relevant original
implementation at
`/home/placeless/dev/hi6250-src/drivers/usb/susb/dwc_otg_pcd_intr.c` and
`dwc_otg_cil.c`; Linux v4.4 provides the corresponding upstream gadget code in
`drivers/usb/dwc2/gadget.c`.

The following values are source-confirmed, not inferred from comments:

| operation | HiSilicon source | Fullerene ANE |
|---|---|---|
| arm EP0 SETUP receive | `SUPCNT=3`, `PKTCNT=1`, `XFERSIZE=24` | same in `arm_setup_receive()` |
| EP0 IN programming order | transfer size, `EPENA/CNAK`, then FIFO service | same order in `ep0_send()` |
| EP0 OUT status on DWC 3.00a+ | `XFERSIZE=maxpacket`, `PKTCNT=1`, `SUPCNT=3` | same 64-byte bounded status receive |
| post-status control flow | complete status, then prepare the next SETUP receive | explicit `service_ep0()` completion plus bounded re-arm |
| FIFO flush | wait for AHB idle, flush TX/RX with bounded polling | only in post-bus-reset `handle_reset()` |

The host evidence remains the controlling hardware result.  The recurring bad
attempts produce `-71`, `-110`, or `-62`; later attempts can return
`Fastboot2.0`.  The known clean host descriptor is 50 bytes (18-byte device
descriptor plus 32-byte configuration); the corrupted form is 27 bytes with a
duplicated 9-byte configuration header.  Therefore the current blocker is not
the `24`-byte SETUP arm value.  The remaining investigation is the EP0 state
transition after an aborted transfer: whether `EPENA` is still set when the
next `DIEPTSIZ0` is written, and whether the completed status/STALL path has
re-armed SETUP exactly once.  `WD: EP0 EPENA`, `UD: RST`, and `UD: FFLUSH`
must be correlated with each host descriptor result before another EP0 change.

The pre-attach cleanup helper is intentionally not connected to the boot path:
earlier hardware experiments showed that touching `GRSTCTL` before the first
host bus reset can wedge the controller.  Recovery flushes remain restricted
to `handle_reset()`.

### Reproduced first clean USB enumeration

The first documented clean Fullerene USB session was reproduced from the
preserved artifact `p20-root/artifacts/fullerene-ane-v211-stock.img` on
2026-10-08.  The host journal first showed the known `-110/-71` retry storm,
then a fresh attach with `Product: ANE fastboot`.  The host descriptor file
contained exactly 50 bytes (18-byte device descriptor plus 32-byte
configuration descriptor), not the 27-byte duplicated-header form.  A single
CNXN-first session then returned `cnxn reply: True frames: 1 raw: 2060` and
stored a 2002-byte piggyback ring tail.  This is the USB acceptance baseline:
loader return alone is not considered success.  The v211 build recipe also
included `FULLERENE_ANE_MARKS_UART_ONLY=1` and
`FULLERENE_ANE_NO_PUTC_WD_TEND=1`; later USB changes must be compared against
this baseline rather than against a loader-only `Fastboot2.0` return.

## Verified battery and charger inventory

The active ANE DTB exposes a usable power-supply path even though Fullerene
does not yet have a generic battery service.  The verified nodes are:

| node | active evidence |
|---|---|
| `/battery` | `compatible = "huawei,hisi_bci_battery"`, non-removable, design FCC `0xBB8` = 3000 mAh |
| `/pmic@FFF34000/hi6555v100_coul@1` | `compatible = "hisilicon,hi6555v100_coul"`, status `ok`, IRQ 9 |
| `/amba/i2c@FDF0D000/hi6523_charger@6B` | `compatible = "huawei,hi6523_charger"`, address `0x6B`, status `ok` |
| `/amba/i2c@FDF0D000/scharger_v200@6B` | status `disabled`; not selected |
| `/amba/i2c@FDF0D000/bq2560x_charger@6B` | status `disabled`; not selected |

The reusable `platform::common::power::BatteryState` is now the transport-
neutral state model.  ANE only publishes the verified PMIC/charger identity;
it does not activate disabled charger alternatives or write PMIC registers.
The remaining implementation dependency is the vendor coulomb-counter
register map and the I2C transport needed to read `hi6523_charger`.

## Generic touch and orientation contracts

The reusable input layers now provide two transport-neutral contracts:

* `lattice::common::ScreenOrientation` maps physical framebuffer coordinates
  into logical window/input coordinates and reports the logical dimensions.
* `resonance::InputEvent::Touch(TouchContact)` carries a contact ID, position,
  pressure, and `Down`/`Move`/`Up`/`Cancel` phase without collapsing a future
  multi-contact report into a mouse event.

`solvent` applies the selected orientation at its common pointer boundary;
the default remains no rotation for existing desktop platforms.  ANE's
measured scanout dimensions are `1080 x 2340`; the active `ts_kit` metadata
provides a likely touch coordinate range of `1080 x 2280`, but no ANE touch
backend is enabled yet.  The controller report protocol, reset/IRQ ownership,
and firmware dependency remain unverified.  These contracts therefore do not
claim touch operation; they only provide the safe integration point for a
future source-grounded backend.

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
