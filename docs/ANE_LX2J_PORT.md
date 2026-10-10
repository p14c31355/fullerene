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

## DWC2 reset/service comparison (2026-10-09, source audit)

Same-host regression boundary (2026-10-09): the previously packaged
`fullerene-ane-commit8e-no-desktop-v1.img` was reflashed at 06:23 and,
after initial host `-110/-71`, enumerated at 06:24:24 as `ANE fastboot`
with 50 actual descriptor bytes. A serialized retry after stopping the host
adb server returned `CNXN reply: True`, raw 2052 bytes, ring 2002 bytes in
`p20-root/rings_night/recheck_8e_no_desktop_retry_20261009.bin`.
The immediately following current-source build with only `desktop::bring_up()`
skipped (`FULLERENE_ANE_SKIP_DESKTOP=1`, configuration gate still active)
was flashed at 06:25:43; host `-71` repeated and only loader `Fastboot2.0`
enumerated at 06:27:16. No current-source CNXN/ring was recovered. Thus the
bench can still enumerate the historical no-desktop image, but removing the
Desktop call alone from current source is insufficient. The later source/boot
path differences must be isolated independently of the original Desktop
introduction. Initial `EBUSY` on the historical image was a host client
claim, not a device enumeration failure.

Follow-up 06:29: current no-desktop/config-gated source with only the four
new per-EP0 `blackbox::text_puts` completion/stall markers removed passed the
full kernel test suite and ANE build, then flashed successfully. Host reported
`Device not responding to setup address` and `-71` at 06:29:54/06:30:04,
then loader `Fastboot2.0` at 06:30:08; no kernel product/CNXN. Therefore
the added synchronous EP0 completion markers alone are insufficient to
explain this regression. Distinguish this SET_ADDRESS failure from the
prior first-descriptor `-71` failure; both precede configuration.

Follow-up 06:33: on that same EP0-quiet/desktop-disabled build, moving
only the timer IRQ USB pump back to its historical after-S3 position
also failed: device 62 descriptor `-71`; devices 63/64 enumerated with
`config index 0 descriptor too short (expected 32, got 9)` and product
`Љ` (different string-slot offsets), then loader at 06:33:54. This
shows live but mis-sequenced EP0 responses, not absence of electrical
reconnect. The historical pump placement alone is not sufficient; the
diagnostic placement change was reverted in source after this test.

06:36-06:41 same-host sequence isolation: historical `8e9f11cf` no-desktop
baseline returned a 50-byte descriptor, `ANE fastboot`, CNXN True and a
2049-byte ring after one initial usbfs EBUSY retry. Current-source
no-desktop/EP0-quiet image with both the post-eMMC config wait removed
and historical post-desktop pull-up edge restored likewise returned
50 bytes, `ANE fastboot`, CNXN True and 2049 ring bytes after an EBUSY
retry. This is a single success, not stability evidence. A derivative
which kept the removed wait but disabled that extra pull-up produced
50-byte descriptors and `ANE fastboot`, yet host registered the second
attach at full speed (512-byte EP1 maxpacket invalid), first CNXN hit
EBUSY, retry hit EOVERFLOW and no ring. Thus the initial device/config
stage can succeed even without the extra pull-up, but bulk success is
not established. All three builds passed full kernel tests/ANE build;
keep the initial EBUSY and subsequent result distinct. The next run
must repeat the no-reconnect condition or isolate the polling wait alone.

06:43 reproducibility check: re-flashing the *same* current-source
no-desktop historical-sequence image that yielded CNXN/ring at 06:38
failed on a fresh boot. Host showed device descriptor `-71`, config
read `-71`, short device read `-75` and then loader `Fastboot2.0` at
06:43:40; no clean descriptor/CNXN. Thus the 06:38 success was a good
draw, not a reliable fix. A poll+post-reconnect comparison image has
been built and tested, but is NOT yet hardware-tested: a failed
baseline cannot serve as an oracle for its next run. The recovered
06:38 ring shows `WD: USB WAIT`, `WD: RECONNECT`, host-reset/setup
sequence including SET_ADDRESS, configuration descriptor and
SET_CONFIGURATION(1), then `ADB: CNXN`. This successful draw required
the existing timer self-heal before clean enumeration.

06:47-06:56 bounded repeat of `current-no-desktop-poll-reconn-v1`:
four identity-gated kernel flashes succeeded. The first three returned
only loader `Fastboot2.0` within the 85-second product observation
window, with host first-descriptor `-71` failures. The fourth reported
no product within that window, but host journal independently showed a
late high-speed device 100, `Product: ANE fastboot`; live sysfs later
showed a 50-byte descriptor and 480 Mb/s. A serialized late CNXN gave
`True`, raw 2060, ring 2002 bytes. The fifth requested iteration did
**not flash** because the loader identity gate did not match while that
fourth kernel remained live. Thus the automated per-cycle `success=false`
for cycle 4 was a deadline classification error, not evidence of a dead
USB path. No five consecutive successes: first three lacked clean
enumeration. `fwupd` had auto-activated and emitted a failed USBFS
CONTROL on device 100; noninteractive stop required sudo credentials
and was not performed. CNXN nevertheless worked; do not blame fwupd
alone for earlier descriptor failures.

06:56 diagnostic retry-budget A/B: with the same current
desktop-disabled, config-wait+post-reconnect source and same 256-beat
interval, raising only the bounded retry maximum from 3 to 6 passed
kernel tests and ANE build. Real hardware still cycled through initial
device descriptor `-71`, then SET_ADDRESS `Device not responding to
setup address`/`-71`, and returned to loader at 06:57:06. Neither a
single success of the 3-retry artifact nor the 6-retry failure proves
the probability distribution, but the added attempts are not a
deterministic repair. Its host result has no descriptor/CNXN/ring.

EP0 source-level hypothesis for the next discriminating test (not yet
proven on hardware): `pop_rx` answers on SETUPRX, while `service_ep0`
clears `SETUP_HANDLED` at IN-ZLP/OUT completion and then can handle a
latched SETUP when `SETUP_BYTES_READY` remains true. If the SETUP latch
arrives/reappears after the immediate SETUPRX-side clear, the same
`SETUP_BUF` may be answered twice, leaving an extra payload in TxFIFO.
The vendor gadget implementation in `hi6250-src/drivers/usb/dwc2/gadget.c`
uses an explicit EP0 SETUP/DATA/STATUS state and enqueues the next setup
after status completion (`s3c_hsotg_handle_outdone`:1519-1537;
`s3c_hsotg_handle_rx`:1616-1672). A test should model delayed and
coalesced RX/DOEPINT ordering, assert one response per distinct SETUPRX,
then replace the single boolean with per-setup ownership/generation only
if the model exposes this duplicate. Do not infer this from comments alone.

07:06 hardware test of the first discrimination step: current
desktop-disabled config-wait+post-reconnect source with ONLY the
SETUPRX-side `SETUP_BYTES_READY = false` clear added after the immediate
`handle_setup()` passed kernel tests and ANE build
(`fullerene-ane-current-no-desktop-setuplatch-v1.img`). The single
boot still showed first-device `-71` storms and loader return at
07:06:26. One negative run does not refute the duplicate-answer model;
the belt branch remains as the only other same-setup answer site and the
burst/misframed 9-13-byte config failures persist across variants.

07:08 latch-order A/B: same source with the SETUP latch clear+CNAK
delayed until AFTER `handle_setup()` staged the EP0 IN response also
passed tests/build but still first-descriptor `-71` x3 and loader
return at 07:08:26. So the early-late ordering of the DOEPINT0.SETUP
clear relative to the response FIFO push is not the discriminator
either. Both same-setup duplicate guards tested negative on single
boots; the remaining difference between working 8e and current EP0
paths must be found by direct diff of the whole SETUPRX/pop_rx block
rather than single-site patches.

Full-diff enumeration `8e9f11cf` vs current (functional, non-format):
ane_usb_dev.rs = SET_CONFIGURATION/GET_CONFIGURATION `CONFIG_WAS_1`
semantics moved (now latched only at GET_CONFIGURATION) plus the two
same-setup guards; ane_fb.rs = formatting; desktop.rs = memory-probe
gates; mmu.rs = formatting; exceptions.rs = USB pump position,
RECONNECT_WAIT_BEATS 1024->256, RECONNECT_MAX 6->3, `WD: USB WAIT`
marker, launchd mark gate, IRQ restamp nesting (formatting); main.rs =
config-wait polling loop, SKIP_DESKTOP and historical-sequence gates;
launchd.rs = formatting; devices.rs = touchscreen/I2C2 inventory
(no runtime writes); flasks = aarch64-user-launchd feature default;
new files clock/designware_i2c/i2c/power/regulator (no boot-path
calls). The only functional EP0 semantic differences are the
`CONFIG_WAS_1` latch point and the same-setup guards.

EP0 differential state model `fullerene-kernel/tests/ep0_setup_model.rs`
(5 tests, passing with the full suite): reproduces RX-first, latch-first,
coalesced, and post-reset stale-latch orderings. It PROVES the 8e shape
can double-answer one SETUP: after the RX-side answer, an 8e boot leaves
`SETUP_BYTES_READY` set (only `arm_setup_receive()` clears it and that is
blocked by `TRANSACTION_BUSY`), and a stale `DOEPINT0.SETUP` latch that
surfaces after the status IN reopens the window fires the belt a second
time. The current tree's SETUPRX-side clear plus the two hardware tests
showed the guard alone does not cure the boot, so the belt branch remains
the only same-setup second answer site; the next hardware candidate must
remove the belt branch (answer ONLY from the RX pop), the exact shape
that failed hard in v198 - meaning the discriminator is elsewhere and the
`CONFIG_WAS_1` latch-point change (GET_CONFIGURATION instead of
SET_CONFIGURATION) is the remaining functional EP0 delta to test.

07:14 hardware test of that delta: current source with CONFIG_WAS_1
restored to the 8e SET_CONFIGURATION latch point (plus everything else
as the 06:38-success shape) passed kernel tests and ANE build
(`fullerene-ane-current-no-desktop-cfgwas1-v1.img`). Single boot: first
device descriptor `-71` x3, loader return at 07:14:22; no
descriptor/CNXN/ring. With the duplicate guards already in the tree and
this latch point restored, the current tree is now functionally
EP0-equivalent to 8e except for the boot-thread polling loop and IRQ
timing constants. The remaining candidates are timing/quantity changes:
RECONNECT_WAIT_BEATS 256 vs 1024, RECONNECT_MAX 3 vs 6, and the
boot-thread `usb_config_was_1` wait loop itself.

07:17 pump-position + historical constants A/B: current source with
`FULLERENE_ANE_PUMP_LATE=1` (USB pump moved back into the beats block
after diagnostics, RECONNECT_WAIT_BEATS=1024, RECONNECT_MAX=6) plus the
historical no-desktop boot sequence passed kernel tests and ANE build
(`fullerene-ane-current-no-desktop-pumplate-hist-v1.img`). Real
hardware: the boot still drew the lottery (device 118: `Љ`/13-byte
config; devices 119-120 `-71/-75`) before a clean device 121 with a
50-byte descriptor at 480 Mb/s; the first CNXN hit EBUSY, the retry
returned `cnxn reply: True` raw 2052 and ring 2002 bytes. One success
with one malformed intermediate draw - same distribution as previous
variants, so pump position and reconnect constants alone do not remove
the misframed-EP0 window either. The remaining structural deltas are
the boot-thread config-wait polling loop and the timer arm interval.

07:20 timer-interval A/B: current source with
`FULLERENE_ANE_ARM_MS_1=1` (timer armed at the 8e 1 ms instead of the
v246b 100 ms) on top of PUMP_LATE + historical sequence passed kernel
tests and ANE build. The first automated cycle attempt hit the device
already in kernel mode (identity gate correctly refused to flash), the
host journal shows a clean device 123 attach with `ANE fastboot` at
07:20:09, live sysfs descriptor is 50 bytes, and a serialized CNXN
returned `True` raw 2096 with a 2049-byte ring
(`rings_night/current_armms1_c1.bin`). First draw of this variant is
clean, but this is one boot; the five-cycle distribution measurement
remains the acceptance test.

07:23-07:28 five-cycle run on the SAME
`fullerene-ane-current-no-desktop-armms1-pumplate-hist-v1.img`:
cycle 1 clean (above). Cycle 2: one `-71` burst during the first
enumeration (device 127), then clean device 5, 50-byte sysfs descriptor,
CNXN True raw 2096, ring 2049 (`armms1_c2_retry.bin` after an EBUSY
first attempt). Cycle 3: clean attach device 8 at 07:25:28; the first
two CNXN attempts returned EBUSY then raw 4, the third timed out with
EP1 EIO; the kernel's own self-heal then re-enumerated (device 10,
07:28:36, with serial) and the next conversation gave CNXN True raw
2096, ring 2049 (`armms1_c3_selfhealed.bin`). So far 3 boots, 3 with a
working CNXN/ring conversation (two required host-side retry after
EBUSY, one required the device self-heal); no `Љ` garble and no
loader-only fail in this variant so far, vs every earlier variant which
failed outright on most boots.

Cycles 4-5 complete the five-boot run: cycle 4 flashed cleanly and its
kernel attach (device 14, after one `Љ` 13-byte-config draw on device
13 that the kernel's own recovery redrew) returned CNXN True raw 2096,
ring 2049 (`armms1_c4.bin`, attempt 1). Cycle 5: clean attach device 17,
50-byte descriptor, 480 Mb/s; first CNXN attempt hit host-side EBUSY,
the retry returned CNXN True raw 2052 with ring 2002 bytes
(`armms1_c5_retry.bin`). Tally for
`fullerene-ane-current-no-desktop-armms1-pumplate-hist-v1.img`:
5/5 boots reached a working CNXN + ring conversation (2 needed a
host-side retry after transient EBUSY, 2 relied on the kernel's own
recovery of a first malformed draw, 1 clean on the first attempt). The
malformed-draw window still exists (device 13) but no longer ends a
boot: every boot reaches descriptors, CNXN and a ring file. Per the
goal's 5-consecutive-cycle contract this run satisfies the reproducibility
evidence; the root-cause statement is the gate combination (1 ms timer
arm + late pump placement + historical boot sequence without the
config-wait poll, with EP0 semantics matched to 8e).

Primary source: `../hi6250-src/drivers/usb/dwc2/gadget.c`, compared with
`fullerene-kernel/src/platform/ane/ane_usb_dev.rs`. This is a source comparison,
not a claim that any of these differences causes the current host `-71` failure.

| Boundary | Vendor source | Current ANE implementation | Difference / constraint |
|---|---|---|---|
| IRQ dispatch | `s3c_hsotg_irq` lines 2440-2478: masked status, ENUMDONE, OUT/IN endpoint interrupts first | `service` lines 2497-2516: raw subset, USBRST first, ENUMDONE second, then RX/EP | Ordering differs; do not infer which ordering is safe without the host/device trace. |
| Bus reset admission | lines 2480-2502: acknowledge USBRST, disconnect, then reinitialise only if `GOTGCTL_BSESVLD` and `time_after(jiffies, last_rst + msecs_to_jiffies(200))`; kill EP0 OUT requests before reinit | lines 2499-2502 call `handle_reset` on every observed USBRST; lines 713-770 reset software latches and re-arm EP0/EP1 unconditionally | Session-valid and 200 ms predicates are absent. Porting them blindly could skip the first valid reset; measure the predicate inputs and actual reset sequence first. |
| Reset core configuration | `s3c_hsotg_core_init_disconnected(..., true)` lines 2284-2397 skips core soft-reset but writes GUSBCFG, FIFO layout, DCFG, interrupt masks, GAHBCFG, EP0 transfer size/controls and enqueues SETUP | `handle_reset` lines 741-770 flushes FIFO, clears address and endpoint IRQs, arms SETUP and EP1; no full core/FIFO/mask rewrite there | Full reinit is not equivalent to FIFO flushing. The vendor init also owns software request lists and FIFO allocation; transplanting register writes would overwrite inherited LK state. |
| FIFO reset | Vendor `s3c_hsotg_init_fifo` called at line 2297 (inspect its implementation before emulation) | lines 748-763 bounded AHB-idle wait followed by all-Tx/Rx flush, but continues even if a wait times out | Flush completion is not checked before the next write; outcome is unmeasured. The older pre-host-reset flush wedged the handoff, so no speculative pre-attach flush. |
| RX processing | Vendor lines 2529-2537 handle RXFLVL after endpoint and USBRST branches, retry while FIFO nonempty (lines 2566-2572) | lines 2512-2516 pop RX before EP0/EP1 handlers, `pop_rx` lines 774-776 drains until RXFLVL clears | Different ordering and retry structure. Target handles SETUPRX immediately (lines 783-815), so changing ordering requires a separate wire-level A/B. |

The tight hardware verdict remains: read the exact `/sys/bus/usb/devices/1-1/descriptors`
bytes (50-byte config is a necessary milestone), product string, then one serialised
`adb_cycle.py log` conversation with a nonempty piggyback ring. No such successful
current-image run is established by this source audit.

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

## The MMIO-stall root cause (2026-10-09 evening, v256b-v263 metered)

The stage meter (IQ: `W=` worst-stage field) and the service-phase meter (UD8
`PC=` field) named the cost centre that the v242/v244 "IRQ ~75 ms" model
underestimated: **a single DWC2 MMIO access can stall the bus for 0.5-3.6 s**
while the core serves host traffic (v260/v263: phase A entry `GINTSTS` read
worst 461 ms, phase C ENUMDONE write+read worst 3.65 s / 754 ms). One ring
text_byte costs 61 us - the console/ring writes are innocent. The 1.4 s/IRQ
average is these stalls, not handler logic.

Measured consequences:

- A handler that outlives its arm interval re-fires the timer on ERET. Moving
  the arm to the tail (v261) starved the responder instead: the pump stopped
  running between beats and every CNXN died EIO; 10 ms tail-arm (v262) still
  ended in unrecoverable non-enumeration. Both reverted (v263 = the v260
  head-arm shape, enumeration + CNXN green on attempt 1).
- Gating the EP0 SETUP re-arm on a consumed-SETUP delta (v259) broke
  enumeration 2/2 with garbled 27-byte descriptors: the every-pass re-arm is
  part of the proven v197 EP0 shape. Reverted.

The structural exit is the message-passing shape: let the DWC2 SPI interrupt
(galready unmasked: RXFLVL/IEPINT/OEPINT in GINTMSK) do the one ack read in
IRQ context, push the event into an SPSC queue, and drain - every other MMIO
access - from the boot thread. That moves the stalls out of the interrupt
path entirely and removes the starvation mechanism instead of tuning around
it. Unverified on this handoff: whether the loader's GIC routing delivers the
DWC2 SPI to the kernel at all (the v1-era record says the controller IRQ was
never wired).

## 2026-10-11 04:00-05:35 - garbled-boot self-heal and the stock fallback

Two facts came out of the night of 10/11 and both got code.

### The garbled boot is a distinct failure, not "config never completed"

v297/v300 reach SET_CONFIGURATION and the host still logs a mangled identity:

    Product: Љ
    Manufacturer: ANE fastboot      <- shifted one field
    SerialNumber: Fullerene

The one-behind EP0 state corrupts exactly the string-descriptor stage, so the
host latches a device identity that is not ours. The enumeration watchdog only
watches for "config-1 never arrived", so it never fires again and the boot sits
wedged until the tick cap (~28 min), with no host traffic to recover it.

v300 moved the D+ re-enumerate edge before `desktop::bring_up()` (the v280
proof order). Result on hardware: still garbled, and it wedged the same way.
The order was not the cause.

### v301: a self-heal for the garbled identity

- `platform/ane/ane_usb_dev.rs` counts served string descriptors
  (`STRINGS_SERVED`, incremented in the GET_DESCRIPTOR string path). A clean
  enumeration always reads langid + product; a garbled one serves none.
- `arch/aarch64/exceptions.rs` adds a garble detector beside the enumeration
  watchdog: if the boot claims configured and `strings_served() == 0` after the
  reconnect wait, cycle D+ and re-enumerate. Bounded by the same RETRIES budget;
  on exhaustion it panics so LK comes back and the auto-flasher can retry.

This closes the gap the previous watchdog left: it now covers "reached config-1
but the host got a wrong identity", not just "config-1 never arrived".

### Recovery path (verified)

Genuine stock kernel: `~/dev/p20-root/firmware/kernel_stock.bin`
(25,165,824 B, `ANDROID!` header, 8.0.0.151). Flash with
`fastboot flash kernel` + `reboot`. Confirmed 2026-10-11 05:35:20: the handset
re-enumerates as `12d1:107e` Product `ANE-LX2J` / Mfr `HUAWEI` - a normal phone
again, USB free to unplug.

### Open at the boundary

- v301 is built (`artifacts/fullerene-ane-v301-garble-selfheal.img`) but NOT
  yet flashed - the device was returned to stock for a break.
- The production path (desktop ON) still does not hold a clean enumeration;
  v300 showed reordering alone is insufficient. Next: verify whether the
  self-heal recovers the desktop path, or whether the panel flush during
  `bring_up()` is what starves EP0.
