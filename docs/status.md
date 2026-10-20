# Status — 2026-10-19 22:37 (bench night)

## The panic was ours, twice — and the photographs named both

Two panic screens were read off the panel tonight. Both were this tree
attacking itself while it waited for the host.

| photo | screen | location | what it is |
|---|---|---|---|
| 22:23:40 | `PREVIOUS PANIC`, white `903`, yellow `29` | `exceptions.rs:903:29` | `panic!("watchdog: enumeration never configured")` |
| 22:29:58 | red, white `1184`, yellow `9` | `blackbox.rs:1184:9` | `panic!("cycle reset: deliberate")` (USB zombie path) |

`report_previous_panic()` works now (`acef2a72`): one photograph taken
minutes after the fact answers what the last boot died of.

## The chain behind 22:27

1. The panel hold parked the boot thread without calling
   `ane_usb_dev::service()`. The host's first `GET_DESCRIPTOR` went
   unanswered for five seconds — the `-110` at 22:27:06 — and the host's
   retries were then read as a dead link.
2. Just before the hold began, the enumeration watchdog had called
   `usb_reconnect_begin()` (D+ low). Its matching
   `usb_reconnect_end()` lived inside `watchdog_applies`, which the hold
   turned off, so D+ stayed low for the rest of the boot.
3. `recovery.rs` saw an EP0 that never moved, called it a zombie, and
   scheduled the loader reset → `blackbox.rs:1184`.

Fixed in `7a3bbb14` (v332t): the hold serves every iteration and leaves
the moment the host configures us; finishing a reconnect is
unconditional; every watchdog asks `blackbox::panel_hold()` before doing
anything drastic.

## Still open: EP0 answers the config descriptor with silence

v332t (flashed 22:34:39) attached at 22:35:06 and the host got
`config index 0 descriptor/start: -110`, then `-71`, then LK at
22:35:19. So the hold was not the only writer of that failure.

The next evidence is a photograph of the v332u peek screen, which shows
what v332t died of.

## Clock

`CNTFRQ_EL0` is read at runtime (`timer.rs:42`) and is trusted as
firmware truth, but measured holds run ~11x faster than their nominal
budgets (nominal 300 s = 26 s of wall time). Every deadline in the tree
is therefore ~1/11 of what its author intended. Decide with a
measurement, not a guess: paint `CNTFRQ_EL0` next to uptime and compare
two photographs taken a known wall-clock distance apart.

## Builds tonight

| build | commit | result |
|---|---|---|
| v332r-hostevents | `20d9f902` | host_events counted resets+ENUMDONE+SETUP; watchdog stand-down during hold |
| v332s-rcend | (unflashed) | reconnect completion made unconditional |
| v332t-servehold | `7a3bbb14` | hold serves EP0; flashed 22:34:39; -110 on config descriptor, LK in 13 s |
| v332u-peek | `7a3bbb14` | flashed 22:36:16, peek hold shows v332t's panic |
## Enumeration is open (2026-10-20 00:00 JST)

The control pipe now survives a full Linux enumeration and the fastboot
protocol runs over EP1. Three defects were separable and all three were
real:

1. **The garble detector was the -71/-110 generator.** It read the gap
   while the host retried a transfer after reaching config-1 as a dead
   identity, cycled D+ three times and panicked (`exceptions.rs:990`).
   Checked against the panel: `SETUPS=0x20`, config reached, then the
   cycle. It now stands down whenever a conversation has happened and
   only repairs when the host has been silent on *every* lane (SETUPs
   and host events), not just on SETUPs. (`aa5110db`, then the event
   lane in the same pass.)
2. **EP0 sizes were missing `MC(1)`.** `dwc2_hsotg_program_zlp` and the
   gadget core's `hs_ep->mc = 1` both keep the mult-count field at one
   for the control endpoint; ours wrote `PKTCNT` only. That produced
   the -75 on `string descriptor 0` and the occasional -71 on
   SET_CONFIGURATION, i.e. exactly the flaky half-enumeration.
   (`bd1247c0`)
3. **A command was dispatched more than once.** `FB_CMD_LEN` is cleared
   only when the ADB path consumes it, so fastboot re-ran the last
   command on every service pass. `oem log` was answered with the
   previous `getvar`'s FAIL, and the reply stream ran one command
   behind. Fixed by consuming the command where it is dispatched
   (`142aa368`) and by dispatching where the bytes are actually popped
   rather than on the trailing OUTDONE (`cc0ad838`). The loader's
   leftover bytes were then swept ahead of the first reply by design;
   the FIFO is now emptied instead, so no parser has to resynchronise
   (`f68e800a`).

Current state: `fastboot devices` reports `SCV7N18927000473`,
`getvar product|serialno|version|partition-size:kernel` all answer.

## Self-flash

`fastboot flash kernel` from the running kernel downloads and writes.
The write path is slow and the faults were, in order:

- `TIMEOUT_CONTROL` (0x2E) was never programmed, so the card got the
  shortest data timeout and every write aborted while every read
  passed (`c9fdca02`).
- The frozen-boot lane counted the write as a dead boot and reset
  mid-flash (`9ce6f6bd`).
- The buffer-write gate was polled per 32-bit word instead of per
  512-byte block (`4bd9a0a3`).
- A deadline in `uptime_us` cut 85 s of progress because that counter
  runs ~10x fast (`2f3ef942`).

Measured rate: 512 KiB in 85 s, i.e. ~6 KB/s, which is ~8x below the
400 kHz wire rate. The card clock is still the init clock; raising it
is the next real speedup, and it needs the vendor clock plan for the
Arasan block at 0xFF390000 rather than a guess.

## The eMMC clock (2026-10-20 01:30 JST)

The card runs at the 400 kHz initialisation clock, and the dividers above
it never held. The reason is not the divider: the Arasan PHY at
`0xFF390000 + 0x430` was never powered. The vendor's
`sdhci_of_arasan_platform_init` is explicit about the order - enable the
resistor trim, clear `DLL_TRIM_ICP`, set it to `0x4`, set `PDB`, wait for
`PHY_STATUS.CALDONE`, then wait for `EXR_NINST` to drop - and only a PHY
that has completed that sequence can sample a clock above the init rate.
We now do the same, plus the steady-state push-pull setting
(`PHY_CTRL2 = 0x003FF7FC`, command open drain off, 40 ohm drive strength
from the board's `drv-strength = <4 4>`).

The clock probe that found this is part of the image: it walks
100 / 50 / 33 / 25 MHz, reads LBA 0 twice at each and keeps the fastest
divider that returns the MBR unchanged, falling back to the field the
boot started with. `fastboot getvar emmc` reports the outcome as nibbles
- one per candidate - so a failed rung says *how* it failed (clock did
not settle, command error, data error, magic mismatch, unstable) instead
of just "slower".

Before the PHY fix the probe reported `0x015EFA00`: no rung held, base
field `0xFA`. That is the measurement that pointed here.

## Repairing a garble without losing enumeration

Every reply the responder sends is derived from the SETUP packet it just
read - `handle_setup` switches on `wValue` with no memory of the previous
request - so a wrong reply can only come from a transfer that was
already in flight. `ep0_resync` therefore flushes the TX FIFO, clears
STALL on both halves of EP0 and re-arms them. The garble detector tries
that first and only cycles D+ when the pipe refuses to re-arm; the host
retries a GET_DESCRIPTOR on its own, and a correctly answered retry
recovers the identity without re-enumeration.

## The flash that stopped at 1 MiB (2026-10-20 02:20 JST)

Self-flash of a 1242 KB image ended `FAILED (remote: 'wr 800,1B4,80018000')`:
`ok=0x800` (2048 sectors, two full 1024-block chunks), `bad=0x1B4` (436,
exactly the last partial chunk), and `LAST_WR` bit31 set with
`err=0x8001` - CMD_TIMEOUT.

The shape is the diagnosis. A multi-block write does not finish when the
last block leaves the host: the card programs, holds DAT0 low, and the
controller reports that as `PRESENT_STATE` CMD_INHIBIT/DAT_INHIBIT. The
next chunk's CMD25 was issued straight into that window, got no response
at all, and failed as CMD_TIMEOUT; the single-sector fallback then
inherited the busy card and failed 436 times in a row, each attempt
spending its full completion poll. That storm - not the card clock - is
where the ten minutes went.

Three changes follow from it:

- `wait_idle` gates both data paths on CMD/DAT inhibit before the command
  and again after CMD12, with a boot heartbeat through the wait.
- `recover_lines` issues software reset for the CMD and DAT machines
  (SPEC 3.00 3.3.13) and clears the error latches when a burst dies. A
  dead burst used to leave the controller holding the lines: every later
  read spun to its poll bound and the responder answered
  `unknown partition` after 2.6 s.
- eight consecutive single-sector failures end the chunk. Sixty-four was
  chosen when the fallback was expected to recover; on a busy card it
  only made the host wait longer for the same answer.

## Where the phone is (2026-10-20 02:25 JST)

The failed chunk left the kernel partition with its last 223 KB stale, so
the next boot could not start the image. A deliberate `oem coldreboot`
returned the device to the boot loader, which fell back on its own - the
phone is up as `12d1:107e` (`Product: HUAWEI`, mass storage only, no ADB,
no network interface), i.e. stock/eRecovery, not FullereneOS.

That fallback is the useful fact of the night: a broken kernel partition
is survivable. Getting back to a flashable prompt still needs the volume
key, because the fallback image exposes neither ADB nor RNDIS.

The image to flash when the loader window opens is built from `d33997a3`
plus the notes above; `fastboot getvar emmc` then reports whether the PHY
unlocked the divider ladder, and `getvar flash` reports the write counts
without needing the panel.

## The loader window without a key press (2026-10-20 02:35 JST)

`fastboot reboot-bootloader` is answered by the responder itself: it writes
`0x01` (BOOTLOADER) to the PMIC reason byte and resets through the SP805
the loader shares with us, so LK comes up in fastboot. That makes the whole
cycle software-driven, and it is what `~/dev/p20-root/cycle_flash.py` does:

    responder: fastboot reboot-bootloader
    LK:        fastboot flash kernel <img>      (LK's own eMMC driver)
    LK:        fastboot reboot
    responder: fastboot getvar product / flash / emmc

Flashing from LK also sidesteps every write defect listed above - the
loader writes at megabytes a second - which is why the responder's own
write path stays a fallback rather than the main road. The gates are
unchanged: a flash only runs when the product on `1-9` is one of the two
known strings, and the boot afterwards is not called good until the new
image answers `getvar product`.

## The i2c transaction that was never over (2026-10-20 03:20 JST)

The touch controller has answered nothing since the port began, and the
reason was in the master, not the panel. `IC_DATA_CMD` is a command word,
not a byte: bit 8 is the read request, bit 9 is STOP, bit 10 is RESTART
(`i2c-designware-master.c`, `cmd |= BIT(9)` on the last byte and `BIT(10)`
on a repeated start).

The touch bus wrote its register pointer with no STOP - correct - then
asked for reads with `0x100`, a read with no RESTART and no STOP on the
last byte. The charger bus was worse: it built its read command out of
`RESTART` alone, which is a *write* of zero. Neither transfer was ever
released, so `MST_ACTIVITY` stayed set and every following transaction
aborted before it started. A controller that answers 0x54 perfectly well
looks dead from the other side of that.

Both buses now end the way the controller requires: pointer byte with STOP
only when nothing follows, first read with RESTART, last read with STOP,
then wait for ACTIVITY and MST_ACTIVITY to clear. The bits are defined once
in `platform/common/designware_i2c.rs`, because I2C0 and I2C4 are the same
controller. This is a hypothesis with a mechanism behind it, not a measured
result: the device has been on stock Android since 02:16, so the first proof
of it is the touch probe on the next window.

## The pull-up without an answer (05:00 JST)

v340j flashed from LK in 0.14 s and the host saw a new high-speed device
twenty-seven seconds later. Then:

```
04:42:23 new high-speed USB device number 120
04:42:29 device descriptor read/64, error -110
04:42:34 unable to read config index 0 descriptor/start: -110
04:42:45 new high-speed USB device number 123 ...
04:42:51 (silence - D+ down, nothing on the bus)
```

D+ was up, so the PHY and the attach ran. EP0 never answered, so the defence
that raises the pull-up worked and the responder behind it did not.

The short form of the story is a unit mismatch that has been in the boot flow
since v331z: the enumeration window is `uptime_us() + 6_000_000`, and
`uptime_us` runs about ten times the wall clock on this part, so the window is
0.6 s. The host's own sequence after the pull-up - reset, address, then the
first descriptor request - does not finish inside 0.6 s, and the boot thread
had already moved on to painting the desktop (seconds, no service) by the time
the request arrived. Each host retry met the same silence, the enumeration
watchdog counted its beats, and the boot panicked to the hold screen.

The fix is not a longer sleep but an honest deadline: sixty counter-seconds,
which is the six real ones the comment has always claimed, and the loop still
leaves the instant the host configures the device. `v341b-enumwin` carries it,
`v341c-noladder-win` carries it without the clock ladder so the two questions
stay separable.

What this does not explain yet: why earlier builds with the same 0.6 s window
did enumerate. The difference between them and this one is the clock ladder
running before the attach, which is exactly why the noladder image exists.

## 2026-10-20 07:0x / v343: a parked pass disarmed USB, and the freeze record left the panel

The 06:26 photograph read `PHASE 0` / `VFSOPEN OK` / `BOOT FROZEN`, which
settles the question the panel was built for: the boot thread is parked
outside `service()`. What the photograph could not show is where, and the
state it left behind was worse than the park itself.

`IN_SERVICE` in `ane_usb_dev::service` is mutual exclusion between the boot
thread and the timer pump, not a lock. A boot thread parked *inside* a pass
never releases it, so every pump entry returned immediately and the device
stopped answering SETUP outright - the host journal becomes a storm of
`Device not responding to setup address` / `Cannot enable. Maybe the USB cable
is bad?` with a fresh device number every few seconds, and the phone never
reaches the loader, so an unattended loop has nothing to catch.

Three changes, all in `cf9e3676` and `4eedd2e5`:

- after 64 consecutive refusals from IRQ context the pump resets the abandoned
  arming state (`ep0_resync`) and takes the pass over (`usb: pass_takeover=`)
- the timer handler publishes ELR to `blackbox::IRQ_PC`; freeze detection
  captures it plus the trail word, and the *manufacturer* string renders
  `Fullerene P<16 hex>T<16 hex>`, so the suspended PC can be read from
  `journalctl -k   | grep Manufacturer` instead of a photograph
- twelve takeovers with no storage operation busy hands the loader back

`~/dev/p20-root/lk_loop.sh` now runs as the systemd user unit
`ane-lk-loop.service` and reads its target from `~/dev/p20-root/NEXT_IMG` on
every cycle, so a boot landing in the loader is flashed without a session.

What is still blocked: the phone that produced this record runs the v343c
*diagnostic* build, which parks on panic - so the no-button loader lane never
completes and the D+ storm continues. It needs one power cycle; the loop then
flashes `v343g-handoff` (`afec7275a19a751a`) on its own.

# Status — 2026-10-20 (VFS hardening, and the second namespace)

## The flash loop was losing windows to two of its own bugs

`lk_loop.sh` hard-coded `/sys/bus/usb/devices/1-9/product`, so a re-enumerated
device on another port was invisible, and it read `NEXT_IMG` as a bare
filename while `fastboot` resolves relative paths against the caller's cwd.
Both cost whole LK windows; the 10:20 run lost three in a row to
`No such file`. Fixed: the loop now scans the whole bus for a product string
of `Fastboot2.0`, and `set_image` resolves `NEXT_IMG` under
`~/dev/p20-root/artifacts` and refuses to flash when the artifact is missing.

## The step trail had four writers and three capacities

`0x348de800` (count u32 at +0, bytes at +4) was written by `blackbox.rs`,
`arch/aarch64/fs.rs`, `arch/aarch64/exceptions.rs` and read by
`platform/ane/ane_usb_dev.rs`, whose reader claimed `CAP = 32` while the
writer kept 16. Now `blackbox::trail` owns the base, the capacity, the
per-boot reset and the clamped read; the other four call it.

## An absolute path is a bounded value

`genome::vfs` normalised paths into `Vec<&str>` plus a `String` — two heap
allocations per call on the boot path, no length bound, and no way to fail.
`AbsolutePath` is a fixed 256-byte buffer (`MAX_PATH`, matching the EL0
boundary) with `MAX_PATH_COMPONENTS = 64`; a path it cannot represent is
refused rather than truncated, because truncation silently names a different
file. `Vfs::try_resolve_path` joins the working directory first, so only the
result is bounded. The old `resolve_path` stays, pinned to the new one by a
parity test. 75 genome tests, 38 kernel tests.

## OPEN: the kernel has two VFS namespaces

- `arch/aarch64/fs.rs:2350` — `static mut VFS: Option<Mutex<Vfs>>`, created at
  `fs::init()` (`:2394`), seeded by the initramfs, reached by every EL0
  syscall through the private `with_vfs` (`:7983`).
- `contexts/vfs.rs:49` — `VfsContext.inner: Mutex<Vfs>`, created by
  `VfsContext::new()` (`:139`), used by `shell.rs`, `gui.rs`, `init.rs`,
  `klog.rs` and `initramfs.rs` through `with_kernel(|k| k.vfs …)`.

Both call `Vfs::new(Box::new(MemFileSystem::new()))`, so a file created
through a syscall is invisible to the shell and vice versa. Init order is
`init_kernel()` (`init.rs:150`) *before* `fs::init()` (`init.rs:515`), which
rules out the contexts layer owning the object unless the arch side adopts it
lazily.

Migration plan (next session, in this order):
1. Make the arch VFS the single owner; `VfsContext` keeps no `Vfs` of its own.
2. Route the 20 `self.inner.lock()` sites in `contexts/vfs.rs` through one
   `with_inner` helper that maps "owner not installed yet" to `FsError::Io`.
3. Lock order becomes arch-VFS -> handle_table. Nothing inside the arch VFS
   lock may take the kernel-context lock; nothing may hold `handle_table`
   while calling into the arch VFS.
4. Public API of `VfsContext` (48 fns) must not change, so shell/gui/init keep
   compiling untouched.
5. Verify: `cargo test -p fullerene-kernel --lib` (38 host tests, unchanged)
   and the ANE build.

## Device, 10:30

v346 (`dec320d6`) flashed at 10:22:16 and never enumerated: no `18d1:d00d`,
no `12d1:107e`, for eight minutes. v343m did enumerate, so v345/v346 moved
the freeze earlier than the USB responder. The panel is the only readout;
the trail letter at freeze time names the stage.

# Status — 2026-10-20 afternoon (UI lands while the device sleeps)

## The warm-reset storm is not ours

Every build after 10:47 - v346, HEAD (`fe4632f4`), AND the re-flashed
v343m binary that enumerated cleanly at 09:37 - storms identically:
`-90/-71/-110` descriptor reads, device numbers climbing, an occasional
string read that dies at `-75` or `-22`. Same binary, clean in the
morning, storming at noon. The audit (subagent, e939808c..HEAD) found no
USB-path change in the range; the storm also reproduced on a binary built
before it. Conclusion: a device-side warm-reset state (PHY/VBUS/DRAM),
not a code regression. Next device session should A/B power-cycle vs
warm-reset from the loader.

## What landed on the host while the phone was on stock (64ea4220)

- **Notes app over the lattice IME.** The dock's fourth tile opens a
  full-screen text view; taps in the docked keyboard go
  `Keyboard::press_at` -> `Field::apply` -> repaint; the title band
  closes. The `Field` is `Option` (no const String) and materialises on
  first use - safe because the bump allocator is live from entry.
- **Touch probe asks both vendors.** The unit's real controller is a
  Goodix GT1158 at 0x14 (`ts_kit@14`, stock-kernel sysfs), not the
  FocalTech the probe was written for; the DTB carries both nodes for
  different panel SKUs. Probe order: GT1x (`0x8140` CONFIG VERSION via a
  new 16-bit-pointer transaction), then fts (0x00); the winner is stored
  in `TOUCH_PROTOCOL` and `poll()` dispatches. The GT1x read path parses
  through `resonance::touch::parse_gt1x_report` (frame = status + N*8 +
  key + checksum, cleared after consumption). FocalTech path unchanged.
- **Backlight contract + LM36923 backend** (`ane_backlight.rs`), ported
  from the vendor kernel's `backlight/lm36923.c`: REG_ENABLE 0x10,
  REG_BRT_CTR 0x11 = 0x85, REG_BRT_VAL_L/M 0x18/0x19, 11-bit code.
  `oem bl [<level>]` drives it from the host.
- Evidence base for the next drivers: `~/dev/p20-root/firmware_blobs/`
  (EVIDENCE_SUMMARY.txt) - touch FW (Nemo_CS445A_junda.bin), Wi-Fi/BT
  blobs (fw_bcm43455_hw.bin, BCM4345C0 hcd, nvram), bfgx_cfg, Hi1102
  DTS GPIO/pinctrl map, and the full chip inventory from the live
  system (LSM6DS3@0x6A, AKM09918@0x0C, LTR578@0x53, hi6523@0x6B,
  pericom30216@0x1D, LM36923@0x36, TFA9895@0x34, pn547@0x28).

Builds: aarch64/ane + bramble exit 0; kernel lib 38, workspace 765
tests pass. Pre-commit review (focused 5-question subagent) all OK;
its one real catch - the GT1x frame length needing `+2` not `+1` - was
fixed before the commit.
