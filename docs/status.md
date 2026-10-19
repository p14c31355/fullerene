# Status — 2026-10-20 (small hours)

## Device

Off-bus, last seen 19:4x. The flashed build is `v331z-panicpeek3`, whose
hold is unbounded in that binary: it halts before USB and never returns to
LK, so nothing can be flashed until someone power-cycles it. (The bounded
45 s hold landed after that image was built; every later build returns to
LK on its own.)

`~/dev/p20-root/watch_flash.sh <image> <hours>` is watching for the window.
It gates on serial `SCV7N18927000473` **and** product `Fastboot2.0`, flashes
once, reboots, and dumps the next `journalctl -k usb 1-1` lines to
`logs/watch_flash.*.log` — so the enumeration evidence is collected even if
nobody is watching the terminal.

## Next flash

`artifacts/fullerene-ane-v332b-battery.img` (md5 `cc2fee5dfa5dcc6260c2a3e1c3730a52`)

Contents, in boot order: panic text hold (45 s, then LK on its own), enum
ledger on the panel, touch → shell input, launcher hit test, battery capsule
in the status bar. `cargo test -p solvent` 37 passed, kernel suites 29/7/12/
27/5.

## Reading the next panel photograph

| SETUPS | ADDR | reading |
|---|---|---|
| 0 | – | the SETUP never reaches `handle_setup` — RX pop / reset ordering, or EP0 armed late |
| >0 | 0 | first SETUP answered wrongly (`-71`), i.e. a phantom SETUP or a stalled standard request |
| >0 | >0, CFG=0 | dies between SET_ADDRESS and GET_DESCRIPTOR(config) |

`WAIT` counts passes of the serve loop, so it also tells whether the loop ran
at all. The battery capsule in the status bar is the second reading: if it
fills, the PMU path works and the voltage sample is real.

## Why `-71` is probably a symptom, not the cause

17:51:18 attach → 17:51:19 `-71` → 17:51:22 SoC reset → 17:51:23 LK. A panic
one second after attach explains both the error and the reset; the host only
sees the endpoint die. That is why the peek build matters more than another
EP0 theory: get the `file:line` first.

## Battery — done, unverified on hardware

`platform/common/power.rs::ocv` (generic, host-tested) + `platform/ane/
ane_battery.rs` (backend). Registers, units and both charge ladders are from
the vendor hi6250 coulomb driver and the stock ANE device tree; see
`docs/hardware/battery.md` for the table and the remaining gaps
(pack identity via ADC ch 6, temperature, charger status, coulomb
integration).

## Open items in priority order

1. `-71` root cause via the peek screen (needs one photograph).
2. Touch on real hardware — never yet observed; `first_point()` has never
   produced a sample.
3. Battery voltage/charge on real hardware (build is ready).
4. Charging: identify the charger IC among `hi6522`/`hi6523`/`bq2560x` at
   0x6B and read its status register; then `charging` stops being a current
   proxy.
5. Storage: eMMC reads work through the bootloader path; mounting in
   FullereneOS is the next real subsystem.
6. Buttons (power/volume) as a proper input source, not just a PMIC IRQ.