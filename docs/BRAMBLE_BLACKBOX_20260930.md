# Bramble black box: what was built, what was measured, what was refuted

2026-09-30. Written during an autonomous run (17:05-19:00 JST) at the user's request,
after the plan for a persistent early-boot recorder was handed over. Everything below is
either a measurement with the run that produced it or is marked as a proposal.

## The question the plan asked first

> If a region with `compatible = "ramoops"` exists, document: physical base, length,
> record-size, console-size, pmsg-size...

Answered from the device rather than from a DTS revision. `tools/fdt_nodes.py` (new,
read-only) parses the 36 stock DTBOs pulled off the phone; all 36 agree:

| node | base | size |
|---|---|---|
| `ramoops_region@B7E41000` | 0xb7e41000 | 0x400000 |
| `alt_ramoops_region@B8241000` | 0xb8241000 | 0x400000 |
| `ramoops_meta_region@0xB8641000` | 0xb8641000 | 0x1000 |
| `ramdumplog_region@0xb8642000` | 0xb8642000 | 0x10000 |

`/fragment@75/__overlay__/ramoops` declares `compatible = "ramoops"`,
`memory-region -> ramoops_region`, `record-size = 0x20000`, `console-size = 0x100000`,
`pmsg-size = 0x80000`, and **no `ecc-size`**, so the zones carry no per-record ECC. The
header comment in the vendor DTS naming a different address is stale; the DTBOs are what
the bootloader uses.

Linux's layout for those sizes, from `fs/pstore/ram.c:1059-1086`:

```
dump_mem_sz = 0x400000 - 0x100000 - 0 - 0x80000 = 0x280000
cnt         = 0x280000 / 0x20000 = 20 dmesg records
console zone at 0x280000 (0xb80c1000)      pmsg zone at 0x380000 (0xb81c1000)
```

The end of the pmsg zone lands on 0xb8241000, which is exactly `alt_ramoops_region` in the
same DT - the arithmetic checking itself against a second, independent reservation.

## What was implemented and builds

`cargo run -q -p flasks -- build --arch aarch64 --platform bramble --usb-gadget-handoff-probe
--usb-gadget-handoff-direct --boot-template tmp/bramble-stock-boot.img --boot-output
/tmp/bb-probe-checkN.img --qemu-preflight ...` exits 0 and the Bramble boot audit passes.

* `fullerene-kernel/src/blackbox.rs` - 32-byte fixed records in a ring, header with
  magic/version/boots/head/session-head, `dc cvac` + `dsb sy` on aarch64 and a no-op
  elsewhere, no allocator, callable from early boot.
* `fdt::find_reserved_region_named()` - the region is matched by node name under
  `/reserved-memory` from the tree the bootloader hands over, never assumed, because the
  same container also holds modem, PIL and CDSP windows.
* `usb2-live-blackbox`, `-read`, `-recent`, `-stage`, `-state`, `-count`, `-probe`,
  `-boots`, `-bootsn`, `-why`, `-code0..2`, and `usb2-live-pulse-cal`.
* USB stage instrumentation: `device_soft_reset()`, `try_arm_setup()`,
  `send_ep_command_result()`, `start_transfer()`.

## The premise, measured three ways

The design rested on reserved RAM surviving a boot. It was tested in three locations and
the answer was the same each time.

1. **The ramoops pmsg zone.** Record count was five or more within one session and one
   after Android had booted in between. Android's ramoops driver owns that zone and
   re-initialises a zone whose header it does not recognise, so Android was clearing it.
2. **`ramdumplog_region` (0xb8642000).** `usb2-live-blackbox-why` reported
   `session_head == 0` on two consecutive boots: the header was re-created every boot.
   That buffer is bootloader-owned.
3. **The ramoops dmesg tail (0xb7e41000 + 0x270000).** Three consecutive runs, all
   `state 2` - "nothing before this boot". Nothing in a normal Android boot writes those
   zones, so this home should have worked.
4. **`ramoops_meta_region` (0xb8641000, 4 KiB).** Two consecutive runs, both `state 2`.
   The last candidate that is neither a pstore zone nor the ramdump buffer.

The correction that matters: every earlier reading of "records survived" was a single
boot's own five records being read back by the same boot. A count threshold of "five or
more" cannot distinguish a surviving ring from a ring created this boot, because one boot
writes about five records by itself. That mistake was made and repeated in this work; the
boot counter, which starts at one and can only grow if the header survives, is the
measurement that has no such defect.

**Conclusion, stated as a finding rather than a failure:** on this device, with `fastboot
boot` between runs, these reserved regions are not preserved across a boot - four were
tried, in four different places, with the same result. The black box
is therefore a *within-boot* instrument here, not a cross-boot recorder. The premise was
the plan's own precondition, it was checked three times in three ways, and the plan asked
for exactly that check before instrumenting into it.

## What the plan asked for and what can be reported instead

The plan's minimum success was: *identify the exact last Fullerene checkpoint before the
Bramble USB path hangs/fails, even though native UART hardware is unavailable.* That is
answerable from the existing Run/Stop field readouts, which need no persistence and have
been working all day:

| # | checkpoint | instrument | measured |
|---|---|---|---|
| 1 | PHY clocks/resets complete | `--hsphy-*` A/B flags | yes |
| 2 | gadget publishes the pull-up | host CCS capture | yes, host sees the attach |
| 3 | EP0 armed | `precheck`, `triage` | `EP0_SETUP_ARMED = true` |
| 4 | host SETUP accepted | `fresh-gates` | HWO cleared, TRBCTL `CONTROL_SETUP`, size 0, TRBSTS OK |
| 5 | descriptor built | `response-bytes`, `setup-buffer` | bLength 18, type 1, `1234`/`0001`, `bMaxPacketSize0` 64 |
| 6 | data-stage TRB prepared | `trb-after` | HWO set, TRBCTL 5, correct shape |
| 7 | data-stage `STARTTRANSFER` issued | `data-cmd` | bit31 timeout, status 0 or 1, command never retires |
| 8 | did the controller touch the TRB | `trb-after` | **no** - TRBSTS stays 0 |
| 9 | host view | host log | `error -71` on the control transfer |

**Last checkpoint reached: step 6, the data-stage TRB is prepared and its `STARTTRANSFER`
is issued; it is never retired and the controller never writes TRBSTS.** The boundary is
between the setup stage completing and the data stage executing. That is the plan's
minimum success.

## Sharpening that boundary (17:43-18:00, new measurements)

The black box work produced three runs that narrowed the data-stage failure further than it
had been. All three use the same EP0 command word, TRB slot and endpoint; they differ only
in the TRB type, the length, and the buffer.

| probe | TRB type | length | buffer | result |
|---|---|---|---|---|
| `usb2-live-status-try` | `CONTROL_STATUS2` | 0 | a TRB slot | **retired** |
| `usb2-live-data-zero` | `CONTROL_DATA` | 0 | a TRB slot | **retired** |
| `usb2-live-data-alt` | `CONTROL_DATA` | 18 | a TRB slot | see the run |
| the production flow | `CONTROL_DATA` | 18 | the response buffer | never retires |

What the first two already establish:

* the controller is **not wedged** after the setup TRB - a command issued after a failed
  data attempt is accepted and retired, so "the controller stopped responding after the
  setup stage" is dead;
* the data command that never retires was **rejected, not pending** - a later command
  retires while it is outstanding;
* `CONTROL_DATA` as a command type is **not** refused: it retires with length zero, so the
  failing element is the payload (a length, a buffer address, or both).

`usb2-live-data-alt` changes only the buffer, holding the length at the descriptor's own
18 bytes, and points it at a TRB slot in the same DMA arena every other transfer in this
driver already uses. Retiring means the response buffer's address or mapping is the fault;
not retiring means any non-zero length is.

## Hazards found, each now written where it happened

* A u64 `read_volatile` at header offset 20 is an alignment fault on aarch64. It killed
  five boots, survived a power-cycle, and looked like a device fault because the harness's
  own log said `Fastboot boot accepted: OKAY`. The first aligned spare offset after
  magic(0)/version(8)/boots(12)/head(16) is 24.
* An absorbed Run/Stop mark shifts every later field by one. Multi-code words are
  therefore ambiguous; the code readout is one field per run.
* Android's ramoops driver re-initialises an unrecognised pmsg zone, so a ring cannot
  live inside a pstore zone.
* The region base must not travel through a `static`: this module compiles into two crate
  roots (`usb_probe.rs` is the second) and a `static` can arrive uninitialised in one of
  them. `base()`/`length()` are constants and `init()` refuses to write unless the tree
  names exactly that region.
* `--hsphy-dtbo-bramble-pvt` selects the board's own HS-PHY tuning pairs
  (`0x67/0x6c`, `0xc8/0x70`) over the generic Lito triple. A six-run A/B found *no*
  effect on the DEPCMD status nibble: the status varies run to run with and without it,
  so it is untested rather than rejected, and it is not the determinant.

## Then what, for the black box

Two honest options rather than more attempts at the same thing:

* keep it as a within-boot recorder and use it for what it can do here - stage sequence
  *inside* a boot, which the field words already do with less machinery;
* or find a region that is genuinely preserved. The candidates not yet tried are the
  4 KiB `ramoops_meta` region and the `debug_info` region at 0xb7e01000; both are small
  or Google-owned, and neither is more promising than the three that failed.

## Appendix: the USB narrowing, and why it is the day's most useful result

The black box did not survive a reboot on this device. The probes built while testing it
did, however, produce four runs that narrow the data-stage stall much further than it had
been, and those are worth more than the recorder would have been.

All four issue a command on EP0 after the setup stage has completed. They share the
endpoint, the TRB slot (0) and the command word; they differ in the TRB type, the length,
the buffer, and above all the *moment*.

| probe | TRB type | length | buffer | when | result |
|---|---|---|---|---|---|
| production flow | `CONTROL_DATA` | 18 | response buffer | as soon as setup completes | **never retires** |
| `usb2-live-status-try` | `CONTROL_STATUS2` | 0 | a TRB slot | 2.5 s | retired |
| `usb2-live-data-zero` | `CONTROL_DATA` | 0 | a TRB slot | 2.5 s | retired |
| `usb2-live-data-alt` | `CONTROL_DATA` | 18 | a TRB slot | 2.5 s | retired |
| `usb2-live-data-late` | `CONTROL_DATA` | 18 | **the response buffer** | 2.5 s | **retired** |

`usb2-live-data-late` is the one that settles it: it re-issues the *production TRB
unchanged* - same buffer, same length, same slot - and it retires. The only variable left
is the moment. The command, the length and the buffer are all exonerated, and the reading
is that the gadget asks the controller to move data before the controller is ready, and
never learns it asked too early because the event ring cannot be read on this handoff
(`read(GEVNTCOUNT0)` never returns).

That is consistent with everything else measured today: the setup stage works because it is
armed *before* the host's SETUP and needs no event; a command issued later than the
controller needs it works; only the command issued in the narrow window right after the
setup completion fails.

The obvious fix follows from it: re-issue the data stage until it retires, which is what a
driver with working events would not need to do. `usb2-live-data-persist` does that. Its
first two runs reported zero retirements, and the reason turned out to be the probe's own
poll budget: the single late re-issue that worked was polled for 2500 ms, so a 400 ms or
1200 ms budget reports a command that is still in flight as a refusal. The third run uses
two 2000 ms polls starting at 1.5 s.

If the retry works, the next question is whether the host gets its descriptor at all -
`grep idVendor=1234` in the run's host log, and `descriptor_max_len > 0` in the night's
record, are the two signals that say yes.

## Update, same evening: one of the home's earlier conclusions was wrong

This document was written mid-afternoon, before the last two experiments, and one of its
conclusions has since been reversed by measurement. Left in place rather than edited away, with
the correction here.

**Wrong:** "the black box cannot persist across boots on this device", which this document
supports with four candidate regions. Three of the four are right. The fourth - the ramoops
pmsg zone - was measured badly: the ring was written there *without* Linux's
`persistent_ram_buffer` header, so Android's ramoops probe saw an invalid zone and
re-initialised it, and the ring read back as a single record. That is where the "5+ records,
then 1 after an Android boot" observation came from, and it was our fault, not the zone's.

**Right, and measured since:** text written into that zone's data area survives a reboot. The
black box now works as intended, through a split:

* the record ring lives in `ramoops_meta_region` and is a *within-boot* instrument, read in
  the boot that wrote it;
* the durable copy of the sequence is a byte-per-record text log appended after the zone
  header in the pmsg zone, which a later boot reads. `usb2-live-blackbox-prev-record`,
  `-prev-boot` and `-prev-region` all answered 1 on the device, i.e. the previous boot's
  records come back in order with the right contents.

**Also found:** Android's pstore is root-only on this device (`ls -l /sys/fs/pstore` gives
`Permission denied` from an unrooted `adb` shell, and there is no `/proc/last_kmsg`), so the
plan's assumption that stock userspace could read our mirror does not hold. The reader has to be
our own kernel.

**And found by the tests, in this document's own subject matter:** the durable log stores stage
codes as the characters `0`..`8`, and the first reader filtered out digits while looking for the
segment ordinal, which deleted exactly those codes. `prev-boot` and `prev-region` still looked
correct because their markers are letters `b` and `r`, while `prev-stage` would have answered
"no" forever. The ordinal is now located by position, and seven unit tests in `blackbox.rs` pin
the framing down.
