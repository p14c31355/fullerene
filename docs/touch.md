# The touchscreen, and the four faults that kept it silent

The panel's controller has never produced a coordinate on this port. By the
end of 2026-10-20 the bus is believed to work and the report is parsed by the
reference driver's rules, but nothing has been observed yet - so read this as
"four faults found and fixed", not "touch works".

## What the hardware is

| fact | value | source |
|---|---|---|
| bus | I2C0 at `0xFFD71000` | device DTB |
| address | `0x38` | device DTB |
| id read | register `0x00`, expected `0x54` | device DTB inventory |
| reset | gpio18 (`0xE8A1D000`) pin 1 | device DTB |
| interrupt | ao_gpio6 (`0xFFF1D000`) pin 0, SPI 162 | device DTB |
| supplies | VDD 3.0 V and IO 1.8 V, brought up before the bus | vendor `hisi_i2c` nodes |

The vendor `sensorhub` node also lists a Goodix part at `0x14` and, on some
boards, a Cypress `cyttsp5` at `0x1C` on I2C2; this unit's DTB names the
FocalTech part, so that is what the driver talks to.

## Fault 1: the transaction never ended

DesignWare `IC_DATA_CMD` is a command word, not a data byte
(`i2c-designware-master.c`, v6.6):

* bit 8 - read
* bit 9 - STOP
* bit 10 - RESTART

The old code asked for a read (`0x100`) with neither RESTART *before* the
direction change nor STOP on the last byte. The master therefore never
released the bus, and the next transaction found it busy. Every touch access
was the first transaction - and it never completed.

## Fault 2: the controller could be a slave

`IC_CON` must be `MASTER | SLAVE_DISABLE | RESTART_EN | SPEED_FAST`, which is
what `i2c_dw_configure_master` writes. The port had `0x25` instead of `0x65`:
no `SLAVE_DISABLE` (bit 6). The I2C4 copy also had `SPEED_FAST` as `1 << 1`,
which is standard mode, against fast-mode timing registers.

Both files now take the bits from `platform/common/designware_i2c.rs`, so the
control word is defined once.

## Fault 3: 200 ms was two

`spin(ms)` counted `ms * 2000` spin iterations. At this core's speed that is
tens of microseconds per "millisecond", so the 200 ms the part needs after
reset was more like 2 ms. It now waits on the counter, with a x10 allowance
because `uptime_us` runs about ten times fast, plus a spin floor.

## Fault 4: the report was read with the wrong fields

Against `edt-ft5x06.c` (v6.6), which is the reference for this report layout:

* `TD_STATUS` (`0x02`) carries the point count in bits 3:0 - the port read
  bits 6:4, which are zero, so a working bus would still have reported no
  fingers.
* the event is `byte[0] >> 6`, where 0 = down, 1 = up, 2 = contact,
  3 = reserved. The port treated `<= 1` as pressed, i.e. "up" counted as a
  press.

Each point record is six bytes: `XH XL YH YL weight area`, with the
coordinate in the low 12 bits. The records start at `0x03`, so five of them
end at `0x20` - the frame is **33** bytes, not the 31 an off-by-two first
read. `poll()` now fetches `0x00..0x20` in one transaction instead of six,
and the parsing lives in `fullerene-kernel/src/touch_report.rs`, above the
platform, with tests: the count nibble, the three events, reserved slots and
a short frame are all checked on the host.

## The bus timing is deliberately conservative

The device DTB says the I2C0 input clock is 19.2 MHz; the generic UDP DTS in
the vendor tree says 128 MHz. Those disagree, and nothing on the device has
been measured. The counters are therefore programmed for the *faster* of the
two at 100 kHz (HCNT 512, LCNT 768): if the input really is 128 MHz the bus
runs at 100 kHz, and if it is 19.2 MHz it runs at 15 kHz. Both are in spec;
the other choice (assuming 19.2 MHz) would run 6.7x too fast in the other
world and NACK.

15 kHz costs about 19 ms per 31-byte frame, which is ~50 Hz. That is the
right trade for a first working touchscreen; speed comes after the part
answers.

## Open questions for the next session

1. `oem i2c 0 38 0 8` - does `0x38` ACK, and what is at register 0? A
   FocalTech FT5x06-family part answers with its chip id there; an
   FT6x06-class part answers `0x00` and keeps the id at `0xA3`.
2. `oem i2cbus 0` - what else is on the bus (the sensors in
   `docs/hardware.md` all claim I2C0).
3. `getvar touch` - the largest X and Y the panel has ever reported. If Y
   stops at 2280 while the panel is 2340 tall, the coordinate needs scaling
   rather than the identity map.