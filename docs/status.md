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
