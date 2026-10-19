# Status — 2026-10-19 22:2x (bench night)

## Where the hold actually ends

The green hold is not being cut by its own loop: the counter stops at
tick 31 (≈15.5 s) and the panel then turns **red**. A panic screen is
confirmed on the device. So the hold runs, paints, and *dies* — the
45 s budget never runs out.

`v332n-panicred` (md5 `207a581afa8a8e13b9a68227d8654bd4`, commit
`648aa186`) makes that legible: whole panel red, line number in white
digits, column in yellow digits, file name small at the top, held for
uptime 300 s (≈2.0–2.5 min of real time — the SoC clock runs fast).

## The reader's own bug (found tonight, fixed in `acef2a72`)

`report_previous_panic()` scanned the durable block for
`FULLERENE AARCH64 PANIC`, a marker **nothing ever wrote**, so it
answered "none found" on every boot since it was written. And when it
did find a record it painted rows 58–59, below the canvas — invisible
by construction. The panic handler now calls `blackbox::panic_record`,
which writes marker, file, line and column; the reader paints the
previous panic as a green field with the same white/yellow digits, and
holds for the peek build. One photograph, taken minutes after the fact,
answers what the last boot died of.

## Next flash

`artifacts/fullerene-ane-v332o-prevpanic.img` (md5 `b62b72179038f0270885c4c07a985579`,
build `FULLERENE_ANE_PANIC_PEEK=1`, commit `acef2a72`).

Flashed 22:18:47. First boot has no record yet (nothing was written
before this image), so it is expected to be quiet; the second boot —
the one `watch_flash.sh` is waiting to reflash — is the one that shows
`PREVIOUS PANIC` with the white line and the yellow column.

## Ruled out tonight (so they stay ruled out)

- `delay_ms` spin cap — fixed in `3ffa7587`, hold now reaches ~15 s.
- Event ledger overrun — rows 38..57 with `LINE_PITCH = 24` land at
  y 1598..2054, inside 2340; `render_text` clips both axes.
- Explicit panics on the enumeration path — the only `panic!` in
  `ane_usb_dev.rs` is the deliberate `oem crash` handler at 1746.
- `delay_ticks` — no panic path; it returns, it does not fail.

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