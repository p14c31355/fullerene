# docs/

Where to look, in the order the answers are usually wanted.

| file | what it answers |
|---|---|
| `NIGHT_20261020.md` | what changed on the night of 2026-10-20, the morning runbook, and how to get back to stock |
| `goal-status.md` | the goal as a scoreboard: every subsystem, its evidence and where it stands |
| `status.md` | dated history of the port, newest last |
| `hardware.md` | one row per part, where it is on the board, and what the port does with it |
| `emmc.md` | the card clock, the ladder, the PHY, high speed, bus width and the write path |
| `touch.md` | the four faults that kept the touchscreen silent, and the timing trade |
| `charger.md` | why the charging IC is a hi6522 and why its register map has to be dumped |
| `usb-split.md` | what has been moved out of the USB driver, and what waits for a device |

## The rules these files keep

* A primary source beats a comment: vendor `sdhci.c`, `sdhci-of-arasan.c`,
  `edt-ft5x06.c`, the DTS under `hi6250-kernel`, JEDEC JESD84-B50.
* No register value is written from memory. If the map is unknown, the doc
  says so and says what read would settle it.
* "Works" means observed on the device. "Written" means code plus unit tests.
  Nothing in between gets claimed.