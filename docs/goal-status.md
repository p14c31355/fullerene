# The goal, subsystem by subsystem

This is the honest scoreboard for "ANE-LX2J behaves like a FullereneOS
smartphone". It exists so that nothing sits disabled because nobody looked at
it: every row either has a native implementation, a mechanism that has been
identified with its evidence, or a blocker with proof of what is missing.

Three states, and they mean exactly what they say:

- **works** - implemented and observed on the device.
- **written** - implemented in the kernel, never observed on the device (the
  device was on stock Android while this was written; see
  `docs/NIGHT_20261020.md`).
- **blocked** - the missing piece is not in this tree and not in mainline.
  `docs/hardware.md` records the evidence.

## Display and UI

| item | state | notes |
|---|---|---|
| boot from the existing chain | works | LK -> kernel, `fastboot flash kernel` |
| framebuffer | works | 1080x2340, landscape canvas drawn, 90 degree CW at scanout |
| portrait abstraction in the shell | written | `mobile_home` lays out in portrait natively; `HomeLayout` is the single geometry source |
| window manager / desktop | works | lattice keeps the desktop path |
| launcher, dock, status bar | works | 4-column grid + dock, notch-aware bar |
| on-screen keyboard | written | `lattice::ime` - layout, hit test, shift/symbols, text model, 12 tests |
| gestures | written | `nitrogen::gesture` - tap, double tap, long press, drag, swipe, 13 tests |
| multitouch | written | `nitrogen::contacts` - id-based contact tracking across slots |
| backlight dimming | blocked | panel brightness rides a PMIC boost (`schg_boost2`); the boost's register map is vendor-only |
| rotation | written | the panel is mounted for portrait; nothing to rotate |

## Input

| item | state | notes |
|---|---|---|
| touchscreen | written | three faults fixed (bus STOP/RESTART, IC_CON, report fields) plus a real settle wait; `TOUCH:` on the panel is the proof to look for |
| volume keys | written | `oem keys` reads the gpio24/25/26 data bytes; which pin is which needs one press |
| power key | blocked | it is a PMIC interrupt, and the PMIC map is only partly known |

## Storage

| item | state | notes |
|---|---|---|
| eMMC read/write/erase | works at 400 kHz | Arasan SDHCI at 0xFF390000 |
| faster card clock | written | ladder re-climbed inside the card's default-speed envelope; `getvar emmc` |
| high-speed timing (52 MHz) | written | `oem hs on` - CMD6 on EXT_CSD[185], read back, then the ladder |
| 8-bit bus | written | `oem bus 8` - EXT_CSD[183] plus HOST_CONTROL1, verified by a read, reverts on failure |
| SD card | blocked | dwmmc1 is a second controller with its own CRG; it stalled the fabric when touched before its clock was on |

## Power

| item | state | notes |
|---|---|---|
| battery percentage | works | Hi6555 coulomb counter through the PMIC cell |
| charging state | blocked | hi6522 on i2c4 at 0x6B; no register map in tree or mainline, `oem i2c 4 6B 0 16` dumps it |
| suspend/resume | not started | needs the PMIC wake sources first |
| thermal | not started | sensors sit behind the PMIC ADC |

## Radios

| item | state | notes |
|---|---|---|
| Wi-Fi (hi1102) | blocked | SDIO plus a firmware blob; the blob is on the device's vendor partition, not in this tree |
| Bluetooth | blocked | same part, same firmware |
| modem / SIM / calls / SMS / data | blocked | modem firmware and its shared-memory protocol are vendor-only |
| GNSS | blocked | `hisi_gps` node exists; the part is behind the modem subsystem |

## Sensors

| item | state | notes |
|---|---|---|
| accelerometer | written | LIS3DH at 0x18 on the touch bus, identified by WHO_AM_I before any write, `oem accel` |
| gyroscope / magnetometer / ALS / proximity | investigated | candidate parts and chip-id registers are in `docs/hardware.md`; `oem sensors` says which one this unit has |
| device orientation | not started | waits on the accelerometer answering |

## USB

| item | state | notes |
|---|---|---|
| device mode / fastboot | works | enumeration, getvar, flash, erase, reboot to LK |
| ADB | works | the shell reaches the device over it when the kernel is up |
| host mode / Type-C | investigation only | `hisi_usb@ff100000` is the only usb node: the dwc2 core can do host (HPRT), but the DTS names no OTG id pin, no CC controller and no VBUS switch. The role is probably the charger IC's, so `docs/charger.md`'s dump comes first |

## What this table refuses to claim

Rows marked *written* are code plus unit tests, not hardware observation. The
one thing that turns them into *works* is a boot with the new image and the
handful of commands in `docs/NIGHT_20261020.md`. Nothing here is marked
*works* because it compiled.