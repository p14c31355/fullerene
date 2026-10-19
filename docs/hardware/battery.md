# ANE-LX2J battery: what the evidence says

Sources, in order of authority:

1. The stock ANE-LX2J device tree (`/sys/firmware/fdt`, decompiled with dtc).
2. The vendor kernel published for the hi6250 family
   (`twicejr/HUAWEI-P9-Lite_OpenSource`, `drivers/power/hisi/...`).
   Local copies live in `docs/refs/hi6250/`.

## PMU registers (Hi6555V100)

`platform/common/pmic.rs` addresses the PMIC cell at `0xFFF34000`, one byte per
32-bit slot: register `r` is the byte at `r << 2`. The vendor calls the same
space through `hisi_pmic_reg_read()` / `hisi_pmic_array_read()`.

| register | name | use |
|---|---|---|
| `0x1F2` | `PMIC_CURRENT_0_ADDR` | battery current, 16-bit LE |
| `0x1F4` | `PMIC_V_OUT_0_ADDR` | battery voltage, 16-bit LE |
| `0x1F6` | `PMIC_CLJ_CTRL_REG_ADDR` | coulomb counter control (`BIT(7)` = calibrate) |
| `0x1F8` | `PMIC_CL_OUT0_ADDR` | charge accumulation |
| `0x212` | `PMIC_OCV_VOLTAGE0_ADDR` | OCV snapshot |

Bit 15 of the voltage and current words is `INVALID_TO_UPDATE_FCC`: the value
has not been refreshed in the current period and must be read again.

Conversions, verbatim from `hisi_hi6555v100_coul.c`:

```
voltage_uv = (u16 & 0x7FFF) * 24 * 1000 * 100 / 10661      // 1 LSB = 225.1196 uV
current_ua = (u16 & 0x7FFF) * 1000 * 1000 * 5 / 10661 / (R_COUL_MOHM / 10)
```

`R_COUL_MOHM` has no override in the ANE device tree, so the driver default
(10 mohm) applies and the divisor is 1.

Registers above `0x1FF` need the vendor's "unlock" dance
(`HI6555V100_REGS_READ`): when `g_ec_6555v100` is false, read
`((addr + size - 1) & 0x1FF)` and then `(addr & 0x1FF)` before the real array
read. `0x1F2` and `0x1F4` are below the boundary, so the plain read works.

## The OCV ladders

The charge curve is not in the driver: `hisi_battery_data.c` reads it from the
battery node's device-tree properties.

```
pc_temp_ocv_rows    = 29
pc_temp_ocv_cols    = 6
pc_temp_ocv_percent = 100 95 90 ... 2 1      (28 entries)
pc_temp_ocv_ocv     = <174 values, row-major by [row][temp]>
```

Temperature columns are `temp_points[] = {-20, -10, 0, 25, 40, 60}` C.

Two packs are provisioned, both 2900 mAh:

| DT node | id_identify range | ladder |
|---|---|---|
| `SCUDATL_2900_39k_4400` | 1235 .. 1759 mV | `ane_battery::SCUDATL` |
| `DESAYLGA_2900_200k_4400` | 2786 .. 3312 mV | `ane_battery::DESAYLGA` |

The identity comes from the BATT_ID ADC (`adc_batt_id = <6>` in the battery
node). `battery_board_type = <2>`.

The ladder has 29 voltage rungs but only 28 percent labels: the last rung
(3200 mV at every temperature) is the cut-off floor. `ocv::percent` returns
1 % there and clamps below it.

## What is implemented

* `platform/common/power.rs::ocv` — table interpolation (temperature, then
  voltage), integer maths, host-tested.
* `platform/ane/ane_battery.rs` — PMU reads, unit conversion, both ladders,
  `state() -> BatteryState`.
* The home screen paints a battery capsule from `state()`.

## What is missing, and its evidence trail

* **Battery identity** — needs the PMU ADC channel 6. The vendor path is
  `hisi_adc` (`drivers/hisi/adc/`); the read is one channel conversion.
  Until then `battery_identity_mv()` returns `None` and the SCUDATL ladder is
  used; the two ladders differ by under 1 % except near empty.
* **Temperature** — the same ADC block (channel `adc_batt_temp`); until it is
  wired the ladder is read at 25 C.
* **Charging state** — `current_ua() > 0` is a *proxy*. The charger IC's
  status register (the `hi6422`/`bq25892` charger node) is the authority and
  is not touched yet.
* **Coulomb integration** — `CL_OUT0` accumulates charge; the vendor uses it
  to smooth OCV steps. Not read yet.