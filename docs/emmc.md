# The eMMC path, from 400 kHz to what the card will allow

Everything here comes from a primary source: the vendor tree's
`drivers/mmc/host/sdhci.c` and `sdhci-of-arasan.c`, the DTS nodes under
`arch/arm64/boot/dts/auto-generate/config/hi6250/`, and JEDEC JESD84-B50.
Where the port still infers something, it says so.

## The controller

| fact | value | source |
|---|---|---|
| controller | Arasan SDHCI at `0xFF390000` | `hisi_6250_mmc.dtsi` (`arasan,sdhci`) |
| input clock | `clk_xin` from CRG `0xB4` | `hisi_6250_clock.dtsi` |
| boot's divider | `CLOCK_CONTROL[15:8] = 0xFA` (250) | read back from the controller |
| effective card clock | `clk_xin / (2 * field)` | vendor `sdhci_set_clock`, spec-3.00 divisors are stored halved |

The 250 divider is the 400 kHz init clock, which only makes sense on a
200 MHz `clk_xin`: `200 MHz / 500 = 400 kHz`. So the loader leaves the CRG
at the rate the vendor probe programs, and the whole speed problem is the
SDHCI divider - not the CRG.

## Why the first ladder failed

`clock_probe` v1 tried fields 1, 2, 3, 4, 6 - and field 1 is 100 MHz. A card
whose `HS_TIMING` is still 0 (default speed) is specified to 26 MHz
(JESD84-B50 6.6.2); asking for 100 MHz is not a faster bus, it is a dead one.
The command dies mid-transfer and the controller then holds CMD/DAT
(`recover_lines` exists because of that). Every later rung ran on a poisoned
path, which is exactly the "all rungs failed" result `getvar emmc` reported
(`0x15EFA00`).

The ladder now climbs inside the envelope, slowest first:

| field | card clock | note |
|---|---|---|
| 8 | 12.5 MHz | first rung |
| 6 | 16.7 MHz | |
| 5 | 20 MHz | |
| 4 | 25 MHz | top of default speed |
| 3 | 33 MHz | only after `oem hs on` |
| 2 | 50 MHz | only after `oem hs on` |

Each rung: set the clock, wait for the new clock to be real, warm up with
CMD16 (block length 512 - it needs no RCA, unlike CMD13), then two CMD17
reads of LBA0 that must both carry `0xAA55` at the end of the sector and
agree on the first word. A failure stops the climb and restores the previous
clock; nothing is ever written.

`getvar emmc` packs the outcome: bits 0..8 chosen field, 8..16 the boot's
field, 16..40 one nibble per rung (0 untried, 1 read, 2 clock refused,
3 warm-up failed, 4 read failed, 5 wrong signature, 6 reads disagreed),
40..48 the first field that failed.

## The PHY

Faster dividers need the eMMC PHY powered: `PHY_CTRL1` PDB, wait for
CALDONE, wait for EXR_NINST to clear, then push-pull 40 ohm
(`sdhci_of_arasan_PHY_init` + `sdhci_disable_open_drain`). `phy_init` is a
port of that sequence; without it the vendor's own faster rungs would not
work either.

## Bytes per second: timing and width

Two switches, both explicit, both verified by reading EXT_CSD back:

* `oem hs on|off` - EXT_CSD byte 185, written with CMD6
  (`(3 << 26) | (index << 16) | (value << 8)`, per `include/linux/mmc/mmc.h`),
  read back with CMD8. On success the ladder re-climbs, because fields 3 and
  2 were previously out of bounds.
* `oem bus 1|4|8` - EXT_CSD byte 183 plus SDHCI `HOST_CONTROL1` (0x28) bits
  2:1. Card first, host second (the CMD6 response travels on CMD, so it is
  valid either way; the data that follows is not). A failed read-back reverts
  both.

Neither runs at boot. A wrong bus width would break every read, and the only
way back would be LK - so they are one command each, with the self-test in
the command.

## The write path

* Chunks of 1024 sectors, count-bounded rather than clock-bounded
  (`uptime_us` runs about ten times fast, so a 900 s deadline fires at ~85 s).
* Between chunks, wait for the card to stop being busy on DAT0; without that
  the next CMD25 is refused and the tail of the image is lost (observed as
  `ok=0x800 bad=0x1B4` at exactly 1 MiB).
* A burst that fails is reset back to idle (`SOFT_RESET` for CMD and DAT),
  or the lines stay held and the next read reports `unknown partition`.
* Failures are counted, and eight in a row end the attempt.
* The counts land in `LAST_FLASH`, readable as `getvar flash`
  (`ok << 32 | bad`).

## What is still inferred, not measured

* `clk_xin` is 200 MHz by arithmetic (0xFA is the 400 kHz init clock), not by
  reading the CRG. `clock_probe` logs `crg40` and `crgb4` for that reason.
* Whether this card tolerates HS (it should - the vendor kernel runs it at
  52 MHz). `oem hs on` answers it in one command, and `getvar flash` shows
  the difference in effective rate.