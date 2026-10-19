# Status — 2026-10-19 (night)

## Device

Off-bus. The flashed build is `v331z-panicpeek3`, whose hold is unbounded in
that binary: it halts before USB and never returns to LK, so nothing can be
flashed until someone power-cycles it. (The bounded hold landed after that
image was built; every later build returns to LK on its own.)

## Next flash

`artifacts/fullerene-ane-v331z7-homehit.img` — ledger + touch + launcher hit
test, built 18:25, `cargo test -p solvent` 37 passed, kernel suites green.

## Reading the next panel photograph

| SETUPS | ADDR | reading |
|---|---|---|
| 0 | – | the SETUP never reaches `handle_setup` — RX pop / reset ordering, or EP0 armed late |
| >0 | 0 | first SETUP answered wrongly (`-71`), i.e. we replied to a phantom/stale SETUP or stalled a standard request |
| >0 | >0, CFG=0 | dies between SET_ADDRESS and GET_DESCRIPTOR(config) |

`WAIT` counts passes of the serve loop, so it also tells whether the loop ran
at all.

## Why `-71` is probably a symptom, not the cause

17:51:18 attach → 17:51:19 `-71` → 17:51:22 SoC reset → 17:51:23 LK. A panic
one second after attach explains both the error and the reset; the host only
sees the endpoint die. That is why the peek build matters more than another
EP0 theory: get the `file:line` first.

## Battery / charging — evidence already extracted

From `/tmp/ane_fdt.dts` (decompiled stock DTB):

- `battery { compatible = "huawei,hisi_bci_battery"; battery_design_fcc = 3000; }`
  — BCI is a *driver*, not a chip: it reads the PMU coulomb counter plus the
  charger chip and presents a percentage.
- `hi6555v100_coul@1 { compatible = "hisilicon,hi6555v100_coul"; interrupts = <9>; }`
  — the fuel gauge lives inside the Hi6555 PMU (same die we already reach for
  `PMIC HRST` at `0xFFF3462C` and `BOOTUP_END` at `0x18C`), interrupt 9.
- Charger candidates in the DT: `hi6522_charger@6B`, `hi6523_charger@6B`,
  `bq2560x_charger@6B` (same I2C address, mutually exclusive alternatives),
  plus `scharger_power` regulators.
- LEDs: `pmic_led@a0` red/green/blue with `dr_ctrl` 0x14D..0x156 — usable for
  charging indication later.
- RTC: `pmic_rtc@a0`, base `0x1D0`, IMR `0x130`, ICR `0x1B2`.

**Do not write guessed coulomb-counter register numbers.** The next step is
either (a) read them from stock Android over adb
(`/sys/class/power_supply/battery/*`, `/proc/...`), or (b) find the Hi6555
coulomb driver in Huawei's published hi6250 kernel source. Until one of those
exists, battery work is blocked on evidence, not on code.

## Open items in priority order

1. `-71` root cause via the ledger (needs the photograph).
2. Touch on real hardware — never yet observed; `first_point()` has never
   produced a sample.
3. Battery reporting + charging (blocked on gauge registers).
4. Storage: eMMC reads work through the bootloader path; mounting in
   FullereneOS is the next real subsystem.
5. Buttons (power/volume) as a proper input source, not just a PMIC IRQ.