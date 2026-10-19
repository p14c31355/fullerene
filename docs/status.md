# Status — 2026-10-19 22:37 (bench night)

## The panic was ours, twice — and the photographs named both

Two panic screens were read off the panel tonight. Both were this tree
attacking itself while it waited for the host.

| photo | screen | location | what it is |
|---|---|---|---|
| 22:23:40 | `PREVIOUS PANIC`, white `903`, yellow `29` | `exceptions.rs:903:29` | `panic!("watchdog: enumeration never configured")` |
| 22:29:58 | red, white `1184`, yellow `9` | `blackbox.rs:1184:9` | `panic!("cycle reset: deliberate")` (USB zombie path) |

`report_previous_panic()` works now (`acef2a72`): one photograph taken
minutes after the fact answers what the last boot died of.

## The chain behind 22:27

1. The panel hold parked the boot thread without calling
   `ane_usb_dev::service()`. The host's first `GET_DESCRIPTOR` went
   unanswered for five seconds — the `-110` at 22:27:06 — and the host's
   retries were then read as a dead link.
2. Just before the hold began, the enumeration watchdog had called
   `usb_reconnect_begin()` (D+ low). Its matching
   `usb_reconnect_end()` lived inside `watchdog_applies`, which the hold
   turned off, so D+ stayed low for the rest of the boot.
3. `recovery.rs` saw an EP0 that never moved, called it a zombie, and
   scheduled the loader reset → `blackbox.rs:1184`.

Fixed in `7a3bbb14` (v332t): the hold serves every iteration and leaves
the moment the host configures us; finishing a reconnect is
unconditional; every watchdog asks `blackbox::panel_hold()` before doing
anything drastic.

## Still open: EP0 answers the config descriptor with silence

v332t (flashed 22:34:39) attached at 22:35:06 and the host got
`config index 0 descriptor/start: -110`, then `-71`, then LK at
22:35:19. So the hold was not the only writer of that failure.

The next evidence is a photograph of the v332u peek screen, which shows
what v332t died of.

## Clock

`CNTFRQ_EL0` is read at runtime (`timer.rs:42`) and is trusted as
firmware truth, but measured holds run ~11x faster than their nominal
budgets (nominal 300 s = 26 s of wall time). Every deadline in the tree
is therefore ~1/11 of what its author intended. Decide with a
measurement, not a guess: paint `CNTFRQ_EL0` next to uptime and compare
two photographs taken a known wall-clock distance apart.

## Builds tonight

| build | commit | result |
|---|---|---|
| v332r-hostevents | `20d9f902` | host_events counted resets+ENUMDONE+SETUP; watchdog stand-down during hold |
| v332s-rcend | (unflashed) | reconnect completion made unconditional |
| v332t-servehold | `7a3bbb14` | hold serves EP0; flashed 22:34:39; -110 on config descriptor, LK in 13 s |
| v332u-peek | `7a3bbb14` | flashed 22:36:16, peek hold shows v332t's panic |