# Splitting the ANE USB driver

Written 2026-10-20 while the phone was on the boot loader's fallback image
and nothing could be flashed. It records the split this file *should* have
and why it has not happened yet, so the next session does it in one pass
with a device in reach instead of guessing.

## What is there now

`platform/ane/ane_usb_dev.rs` (2833 lines) holds four responsibilities:

| lines (approx) | responsibility | belongs |
|---|---|---|
| 1 - 260 | dwc2 register map, endpoint config, PHY | here |
| 260 - 1050 | RX/TX FIFOs, EP0 control state machine, EP1 bulk | here |
| 1050 - 1500 | `service()`, enumeration counters, garble/zombie hooks | here |
| 1520 - 2270 | fastboot protocol: `fb_poll`, `oem *`, reboot reasons | `fastboot_dispatch` |
| 2270 - 2833 | SP805, PMIC reason, `oem log` streaming, panel hooks | mixed |

`fastboot_session.rs` (147 lines) already took the two buffers and the
byte-level demultiplexing; that was the cheap half of this split and it
shipped.

## The remaining move, and what it needs

`fb_poll` is ~750 lines and it calls back into the driver for exactly five
things:

- `ep1_send(&[u8])`, `reply_ok(tag, value)`, `fb_send_info(text)` - the
  wire, already `pub(crate)`-shaped
- `wdog_drain_and_reset()`, `PMIC_REASON_ADDR` - the reset lane
- `LOG_ACTIVE` / `LOG_CURSOR` - the paced `oem log` stream

So the move is: make those five `pub(crate)`, cut 1520-2270 into
`fastboot_dispatch.rs` as `pub fn poll(log)`, and have the EP1 OUT path in
the driver call `crate::fastboot_dispatch::poll(log)` instead of `fb_poll`.
No behaviour changes, no new state - `fb_poll` already takes everything
else through `fastboot_session` and `ane_emmc`.

Two things to check when it lands:

1. `oem log` streams one line per `service()` pass, so `LOG_ACTIVE` must
   stay owned by the driver (the pace is a transport concern) and be
   exported, not moved.
2. `panic!("oem crash: deliberate")` must keep its `#[cfg(platform)]`
   guard; it exists for the boot-failure test.

## Why it waited

A pure move is only verifiable by walking the whole enumeration and flash
path on hardware. The device spent the night on the loader's fallback
image, so the move would have been compile-checked and nothing more - and
the morning's flash window is worth more than a cosmetic diff. The design
above is small enough to do in one pass with `cycle_flash.py` as the
verification.