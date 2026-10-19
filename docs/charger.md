# The charger IC on ANE-LX2J

Resolved 2026-10-20 from the vendor device tree, because three candidates
shared one address and the answer decides which driver to write.

## The primary sources

`arch/arm64/boot/dts/auto-generate/config/hi6250/hi6250_udp_default_config.dts`
(the board config, `i2c4` enabled, 120 MHz):

```
i2c4: i2c@FDF0D000 {
        status = "ok";
        scharger_v200: scharger_v200@6B {
                gpio_int = <&gpio22 4 0>;
                vclamp = <80>;
                fcp_support = <0>;
                status = "ok";
        };
        max98925_smartpa_l: smartpa@31 { status = "ok"; };   // smart PA
        fairchild_fsa9685: fsa9685@25 { status = "ok"; };    // USB switch
};
```

`hisi_charger.dtsi` and `huawei_charger.dtsi` both declare three nodes at
`0x6B` - `huawei,hi6522_charger`, `huawei,hi6523_charger` and
`huawei,bq2560x_charger` - all `status = "disabled"` by default. The board
enables the one **labelled** `scharger_v200`, and that label belongs to the
hi6522 node.

So, for ANE-LX2J:

| what | value |
|---|---|
| charger IC | **hi6522** (`huawei,hi6522_charger`) |
| bus | i2c4, `0xFDF0D000` |
| address | `0x6B` |
| interrupt | `gpio22 4` |
| fast charge | not supported on this board (`fcp_support = <0>`) |
| USB switch / type detector | fsa9685 at `0x25` on the same bus |
| audio smart PA | max98925 at `0x31` on the same bus |

## What this does not give us

The register map. The vendor tree in `~/dev/p20-root/hi6250-kernel` carries
the DTS but not `drivers/power/.../hi6522_charger.c`, and inventing offsets
from the address alone is exactly the guess this project does not make.

## The experiment that settles it

Read-only, harmless, and it needs the phone once:

1. With the cable out, dump `0x00..0xFF` of `0x6B` on i2c4.
2. With the cable in, dump the same range.
3. The registers that change are the status block; correlate the change
   pattern with the known states (VBUS present, charging, full) that the
   battery gauge already reports through the PMU cells
   (`0x1F4` voltage, `0x1F2` current, `0x1F8` charge).
4. Read `0x25` (fsa9685) in both states for the charger-type bits.

A dump that is stable across both states means the charger sits on a bus we
have not brought up yet, and the i2c4 pinmux (gpio37/38) is the thing to
check first - it is the same trap as the touch panel's I2C0.