# Splitting the ANE USB driver

Updated 2026-10-20 04:00 JST. The original version of this file described a
move that has since shipped, so this is the state after it.

## What shipped

| move | commit | result |
|---|---|---|
| fastboot session state (buffers, byte demux) | `8e6d3f1f` → `9b235efe` | `src/fastboot_session.rs`, 147 lines, platform-free |
| the command interpreter (`fb_poll`) | `2c51a6ee` | `arch/aarch64/fastboot_dispatch.rs`, ~600 lines |
| the panel diagnostic | earlier | `platform/ane/ane_emmc_diag.rs`, 817 lines |
| the I2C buses | `46754a26` + `10b5ff1f` | `platform/common/designware_i2c.rs` holds the words |

`platform/ane/ane_usb_dev.rs` is 2350 lines. What is left in it:

| lines (approx) | responsibility | belongs |
|---|---|---|
| 1 - 250 | dwc2 register map, endpoint geometry, PHY | here |
| 250 - 1100 | RX/TX FIFO, EP0 control state machine, EP1 bulk | here |
| 1100 - 1500 | `service()`, enumeration counters, garble/zombie hooks | here |
| 1500 - 1900 | `oem log` streaming, `diag()`, panel strip hooks | `ane_debug_lanes` |
| 1900 - 2350 | SP805 watchdog, PMIC reset reason, reboot scheduling | `ane_reset_lane` |

## The next move, and why it has not happened

The two remaining cuts are not protocol, they are the transport's opinion
about debugging: `oem log` streams one line per `service()` pass (the pace
is a transport concern and must stay with the pump), and the reset lane
touches SP805 and the PMIC reason register in the same pass that decides
whether enumeration is alive.

A move of either is a pure relocation of ~300 lines with no behaviour change,
which means the only thing that can verify it is a full enumeration and flash
on hardware. Doing it blind risks the one window the port iterates through,
so it waits for a device session where `cycle_flash.py` can prove the
transport still works afterwards - the same rule that governed the two moves
that did ship.

What has been done instead, while the phone was on its fallback image: every
change was kept *additive* (new commands, new diagnostics, new pure modules),
and the three drivers below the USB stack were read against their reference
implementations rather than refactored.