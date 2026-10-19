# ANE-LX2J hardware matrix

What the phone is made of, taken from the vendor device tree
(`~/dev/p20-root/hi6250-kernel/arch/arm64/boot/dts/auto-generate/config/hi6250/`),
and where FullereneOS stands with each part. Written 2026-10-20.

| part | device | bus / address | FullereneOS |
|---|---|---|---|
| SoC | HiSilicon Hi6250 (Kirin 659 class) | - | boots |
| panel | AUO OTM1901A, MIPI DSI, 1080x2340 | DSI | painting (portrait, rotated canvas) |
| touch | FocalTech fts (chip id 0x54) | i2c0 `0xFFD71000`, 0x38, irq ao_gpio6 | driver written, **never seen a touch** |
| eMMC | Arasan SDHCI | `0xFF390000` | read / write / erase, clock ladder in progress |
| PMU + gauge | Hi6555 | PMU cells | battery % on the panel |
| charger | hi6522 (`scharger_v200`) | i2c4 `0xFDF0D000`, 0x6B | bus up, register map unknown |
| USB switch | fsa9685 | i2c4, 0x25 | bus up, unprobed |
| audio codec | hi6402 (`hi6250-hi6402`) | ASP / SLIMbus | untouched |
| smart PA | max98925 | i2c4, 0x31 | untouched (needs the codec first) |
| Wi-Fi / BT | hi1102 | SDIO + UART | untouched |
| GNSS | `hisi_gps` | modem side | untouched |
| fingerprint | fpc | SPI | not needed for the goal |
| keys | `hisilicon,gpio-key` | GPIO | power/volume not yet read |
| USB | dwc2 gadget | `0xFF100000`-ish | enumerated responder |

Notes that matter:

- The touchscreen is **not** the Cypress part the default config also
  mentions (`cypress_touchscreen@1C`); the panel in this unit answered at
  FocalTech's address with chip id 0x54. The DTS carries several board
  variants.
- The charger, the USB switch and the smart PA share i2c4, so one bus
  bring-up (now in `ane_i2c.rs`) unlocks three parts.
- `docs/charger.md` has the read-only experiment that produces the hi6522
  register map; nothing else about charging can be written honestly
  before that.

## What this means for the next sessions

Ordered by what one device session can settle:

1. The clock ladder after the PHY fix (`getvar emmc`).
2. The write path end to end (`getvar flash` counts).
3. The i2c4 dump with the cable in and out -> charger map.
4. A touch on the panel -> the FocalTech driver's first evidence.
5. The arch-timer calibration (`getvar time` vs the host's send seconds).