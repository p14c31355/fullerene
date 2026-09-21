# Bramble display bring-up (sofef01 / Lito MDSS)

Permanent equipment for the handset: a working display gives the kernel the one
thing it has never had on this device - a **readout channel**. Every host-visible
USB diagnostic is dead (see `CONTEXT_STATUS.md` 180-188), so the ability to print
device-side state to the panel is what unblocks the remaining USB measurement.

This document is the source-of-truth plan and the extracted hardware facts. It is
updated as the port progresses.

## Why (short version)

* The USB failure is sub-EP0: the PHY completes the chirp handshake but never
  receives host traffic (`SOFFN` never advances).
* Deciding what is wrong needs a device-side readout of `EP0_SETUP_ARMED`,
  `SETUP_ARM_FAILURE_STAGE`, `SOFFN`, the event ring and `DSTS`/`DCTL` **at the
  moment of the attach**.
* CCS pulses are inert, Run/Stop cycles produce no re-attach, park-duration gates
  are dominated by the harness, `bramble-usb trace` needs an enumeration, and the
  kernel has no display backend.
* The bootloader's framebuffer cannot be reused: the DPU is stopped and the panel
  simply holds its last frame (proved by filling the whole 36 MB "Display
  Reserved" region - `451168.0`).

## Panel facts (extracted from the vendor DT)

Source: `tmp/qpr1-msm/arch/arm64/boot/dts/google/`

| Item | Value | Source |
| --- | --- | --- |
| Active panel | `sofef01` (Samsung, OLED, 1080p) | `lito-bramble-display.dtsi:14,17` |
| Panel DT | `dsi-panel-sofef01-1080p-cmd.dtsi` (277 lines) | same |
| Mode | `dsi_cmd_mode`, `burst_mode`, `trigger_sw` DMA | panel DT `:24,34,44` |
| Size | 1080 x 2340, 24 bpp, `rgb_swap_rgb` | panel DT `:75,76,30` |
| h timing | front 32, back 98, pulse 32, skew 0 | panel DT `:77-80` |
| v timing | back 8, front 8, pulse 1 | panel DT `:82-84` |
| Derived totals | h_total 1242, v_total 2357 | computed |
| Framerate | 60 fps | panel DT `:73` |
| Lanes | 4 (`lane-0..3-state`, `lane_map_0123`) | panel DT `:36,39-42` |
| Topology | `<1 0 1>` => 1 LM, **no DSC**, 1 interface | panel DT `:143` |
| Reset | `<0 10>, <1 10>` ms, GPIO `pm8150l_gpios 8` | panel DT `:43`, display DT `:24` |
| TE | `tlmm 10`, `te-pin-select=1`, `te-dcs-command=1` | display DT `:23`, panel DT `:45-47` |
| Supplies | `dsi_panel_pwr_supply_redbull` | display DT `:21` |
| DSI clocks | `mux_byte_clk0`, `mux_pixel_clk0` | display DT `:22` |
| PHY timings table | `00 23 09 09 26 24 09 09 06 02 04 00 1D 19` | panel DT `:135-138` |
| Init sequence | 66 commands, LP mode, ends `05 01 ... 01 29` (display on) | panel DT `:100-131` |

## Block facts

| Block | Version | Driver source |
| --- | --- | --- |
| DSI PHY | `qcom,dsi-phy-v4.1` (7nm) | `tmp/display-src/dsi_phy_7nm.c` (upstream, has the `V4_1` quirk) |
| DSI controller | `qcom,dsi-ctrl-hw-v2.4` | `tmp/display-src/dsi_host.c` (upstream) |
| DPU / MDSS | SM7250 (Lito) | `tmp/qpr1-msm/drivers/gpu/drm/msm/disp/dpu1/` |
| MDSS base | `0x0AE00000` (size `0x200000`) | bootloader map |
| SSPP blocks | MDSS + `0x1400`, `0x1600`, ... stride `0x200`; `SSPP_SRC0_ADDR` at `+0x14` | `dpu_hw_catalog.c:110,115`, `dpu_hw_sspp.c:27` |
| LM blocks | MDSS + `0x44000`, `0x45000` | `dpu_hw_catalog.c:242,243` |
| PHY PLL ref | 19.2 MHz (`VCO_REF_CLK_RATE`) | `dsi_phy_7nm.c:44` |

The PHY PLL reference clock is the same 19.2 MHz source the USB PHY work already
uses, so the existing GCC/clock plumbing in the kernel is reusable.

## Derived DSI rates (to be confirmed against the driver's own math)

* pixel clock = 1242 x 2357 x 60 = 175.6 MHz
* data rate = pixel clock x 24 bpp = 4.215 Gbps
* per lane (4 lanes) = ~1.054 Gbps

The vendor tree computes the byte/DDR clock from the same timing set when
`qcom,mdss-dsi-panel-clockrate` is absent, which is the case for `sofef01`. The
port must reproduce that computation rather than hard-code a number; the
`dsi_phy_7nm.c` PLL divider math (`dsi_pll_calc_dec_frac`, `dsi_pll_commit`) is
the authority.

## Implementation plan

New permanent module: `fullerene-kernel/src/arch/aarch64/display/`

```
display/
  mod.rs        - public entry: display::init(), display::print(...)
  panel.rs      - sofef01 constants: timings, the 66-command init sequence, reset
  dsi_phy.rs    - 7nm/v4.1 PHY port: PLL setup, lane settings, enable/disable
  dsi_ctrl.rs   - hw v2.4 controller: DSI_HOST_CFG, timing, command TX, TE
  dpu.rs        - minimal scanout: LM/SSPP setup, source address, timing engine
  font.rs       - small bitmap font + text blitting into the framebuffer
```

Staged delivery, each stage independently verifiable on hardware:

1. **Stage A - facts and skeleton. DONE (2026-09-20).**
   `docs/DISPLAY_BRINGUP.md` (this file) plus
   `fullerene-kernel/src/arch/aarch64/display/{mod.rs,panel.rs}`, registered in
   both `main.rs` and `usb_probe.rs` under `fullerene_aarch64_bramble`.
   `panel.rs` carries the extracted constants: geometry 1080x2340, 24 bpp, 4
   lanes, 60 fps, the h/v porches, h_total 1242 / v_total 2357, the reset
   sequence `<0 10>, <1 10>`, the reset/TE GPIOs, the 14-byte DSI PHY timing
   table, and the on/off/nolp/lp1 command sequences decoded into typed
   `DsiCommand` values. `cargo build ... --target aarch64-unknown-none` is clean
   and rustfmt accepts both files. No hardware change.
2. **Stage B - clocks and power.** GCC display branches (MDSS AHB/AXI/byte/pixel),
   the panel supplies, the PM8150L reset GPIO. Verifiable by reading back the
   GCC registers (needs no panel).
3. **Stage C - DSI PHY. IN PROGRESS (2026-09-20).**
   Source extraction complete (`docs/DISPLAY_REGISTERS.md`): the 35-step PHY
   enable sequence with its V4.1 values, the DT-driven lane settings, the PLL
   divider math and start/lock sequence, and all register offsets from the
   upstream XML plus the vendor's committed `dsi.xml.h`.
   Implemented and **verified on the host**: `dphy_timing_calc_v4()` in
   `display/dsi_phy.rs`, a faithful port of `dsi_phy.c:373-461`. The test
   `v4_timing_reproduces_panel_dt_table` derives the bit rate from the panel
   geometry (1242 x 2357 x 60 x 24 / 4 = 1,053,861,840 Hz) and checks that the
   computed `clk_zero`, `clk_prepare`, `hs_exit`, `hs_zero`, `hs_trail`,
   `hs_prepare`, `hs_rqst` and `clk_post` all reappear verbatim in the panel's own
   `qcom,mdss-dsi-panel-phy-timings` table - an independent cross-check against
   the vendor DT, not just internal consistency. `cargo test --bin fullerene-kernel
   display::` passes.
   Still to do for stage C: the PLL helpers (`config_hzindep_reg`, `phy_dig_reset`,
   `enable_global_clk`, `set_usecase`), then the register sequences against real
   MMIO, verified by the PLL lock bit.
4. **Stage D - panel init.** Reset sequence then the 66-command init over DSI in
   LP mode. Verifiable by the panel responding (and visually: the panel leaves the
   held logo).
5. **Stage E - scanout.** DPU LM/SSPP programming with a solid colour, then the
   text renderer. **First visual proof.**
6. **Stage F - the payoff.** Print the USB device-side state (`EP0_SETUP_ARMED`,
   `SETUP_ARM_FAILURE_STAGE`, `SOFFN`, event ring, `DSTS`/`DCTL`) at the attach
   moment, which is the measurement the archive still lists as open.

## Hardware results so far (2026-09-20)

Five runs with `--signal-cmd-gate dsi`. All negative: the panel never changed
from the bootloader's held frame.

| # | What was added | Run dir | Result |
| --- | --- | --- | --- |
| 1 | PHY PLL + PHY + controller + panel init + 64-row RAMWR band | 479375.0 | no change |
| 2 | (longer observation window only) | - | no change |
| 3 | dispcc branch enables | - | no change |
| 4 | dispcc base corrected to `0xaf00000` | - | no change |
| 5 | **full-screen RAMWR fill** (~470 transfers) | 481908.0 | no change |

The fifth run is the informative one: a full-screen fill removes every doubt about
the band's CASET/PASET window, so **the DSI link is delivering nothing at all**.

What the runs did establish:

* The gate really executes - `boot-reason = watchdog`, which only happens when the
  park at the end of the gate is reached.
* The USB side behaves exactly as the archive predicts (attach, then `-110`), so
  the display work is not perturbing the handoff.
* One real bug was found and fixed: the DSI node's `disp_cc_base` is a **4-byte**
  register, not the clock block. The dispcc block is `qcom,dispcc@af00000`
  (`reg = <0xaf00000 0x20000>`, `lito.dtsi:1626-1628`).

## The blocker: there is no device-side readout

With the display dead there is no way to see *where* the bring-up fails - whether
the PLL locked, whether the controller accepted commands, whether the panel
answered. Iterating further without that is guessing, which this port's rules
forbid.

What the host can observe, and what is therefore usable as a channel:

* `lsusb-timeline.txt` records USB state transitions with **microsecond
  timestamps** (`[86.994us] state=device-absent`). This is the most promising
  channel: the kernel controls when the DUT appears on the bus, so a
  stage-dependent delay before the gadget handoff is readable from the host.
* `boot-reason.txt` gives one bit (did the park complete).
* Park *duration* is **not** usable: runs read "handset returned via Android after
  67 s" and "69 s" regardless of what the gate did.

## Next steps

1. Build the host-readable stage channel described above.
2. Bisect with it: clock branches -> PLL lock bit -> controller ack -> panel
   response, one predicate per run.
3. Only then revisit the panel power/reset path (PM8150L GPIO 8,
   `dsi_panel_pwr_supply_redbull` rails), which is still unported.

### Readout channel: what was tried and what failed

Attempt 1 - stage-dependent park + panic (`code * 10` s, then reset). **Failed**: the
harness's own 45 s recovery grace reboots the handset first, so the kernel's timing
never reaches the host. This is the same effect the USB archive recorded as
"park-duration gates read 66 s either way"; it was re-confirmed here at
`code * 3` s as well (`handset returned via Android after 67 s` in every case).

Attempt 2 - reading USB state transitions from `lsusb-timeline.txt`. The timeline
only ever shows `state=device-absent` between the boot and Android, because the
broken gadget never enumerates, so there is no `fullerene` state to time.

**The channel that should work**: the *attach time* is host-visible (the host logs
`new high-speed USB device number N` about 6.2 s after the handoff). If the display
bring-up runs *before* the USB handoff and delays by the stage code, the attach
time shifts by exactly that amount and the host reads the stage from its own dmesg
or timeline. That needs the display probe to be invoked ahead of the handoff rather
than from the post-handoff gate.

Attempt 4 - move the bring-up into the pre-gate path and park `code * 8` s there.
**Also failed**: the return time is still exactly 67 s.

**Definitive conclusion: under the `loop` subcommand the harness's 45 s recovery
grace is a hard cap.** Nothing the kernel does - before or after the handoff -
changes the observed return time. Note that the archive's working examples
(`1->~35 s, 4->~80 s, 7->~125 s` in `run_ep0_signal_probe`) include values **above**
67 s, so a longer-grace invocation does exist; the next session should find and use
that invocation (or a flag that lengthens the grace) rather than trying to beat the
`loop` grace from inside the kernel.

**The harness's own timing model** (`flasks/src/bin/bramble-usb.rs:5313`):

```
// ~35-45 s no gate ran / early reset, ~85-95 s gate TRUE
```

**Confirmed with `--enum-timeout 180`** (run 493223.0):
`capture_stopped_after_boot_ms=67067`, `reason=enumeration-window-ended`,
`boot-reason=watchdog`. The capture window is fixed at ~67 s by the harness
regardless of `--enum-timeout`, and the reset is the harness's (`boot-reason` shows
the kernel never panicked). **Under `loop`, no kernel-side timing is observable at
all.**

So the readout has to be a *state* channel, not a timing one. Options for the next
session, in order of preference:

1. Find a harness mode whose capture window is longer (the harness's own comment
   documents ~85-95 s for a "gate TRUE" run, so such a mode exists).
2. Use a non-timing channel: the USB attach/descriptor behaviour is the only other
   host-visible state, and it is currently broken at the descriptor stage - which is
   exactly what the USB campaign is trying to fix.
3. Get the panel working, which makes the readout self-hosting (the original plan).

## Prime suspect: the MDSS power domain is never powered up

Symptom that points here: every register write in the bring-up completes without a
fault, yet nothing on the panel changes. A powered-down block behaves exactly like
that - writes are accepted and silently dropped. (Confirmed by a human observer:
the panel keeps showing the bootloader's held logo throughout, run 494826.0.)

The DT ties MDSS to a power domain: `power-domains = <&mdss_mdp>` on the sde-kms
node (`lito-sde.dtsi`), and the DSI nodes reference it too. The USB side already has
a working GDSC implementation to copy the shape from
(`platform/bramble.rs`: `USB30_PRIM_GDSC = 0x10f004`, `GDSC_PWR_ON = BIT(31)`,
`GDSC_HW_CONTROL = BIT(1)`), but the display GDSC was **not** found in
`vq_gcc-lito.c` or `vq_dispcc-lito.c`, so Lito likely gates MDSS through a
different route (RPMh, or a GDSC defined in another file).

Next session starts here:

1. Find how Lito powers the MDSS power domain (grep the vendor tree for the
   `mdss_mdp` power-domain provider, and for any GDSC whose name contains `mdss`).
2. Bring that domain up before the DSI programming, then re-run the `dsi-off`
   probe - a black screen is now the cleanest possible signal.
3. The observer watches the first ~40 s after boot (before the harness returns the
   handset to Android).

### Attempts against the "writes are dropped" symptom

Run 496422.0 (full-screen WHITE fill, chosen because the observer's handset is on
Android's dark theme): no change. Run 499126.0 added the GCC display branches
(`gcc_disp_ahb_clk 0xb00c`, `gcc_disp_hf_axi_clk 0xb030`, `gcc_disp_sf_axi_clk
0xb034`, plus `qmip_disp_ahb` and `disp_xo`): still no change.

That closes the "dispcc was inaccessible because its GCC AHB was gated" theory for
these two runs, though the branches are still required and are now in place.

**Next suspect: reset.** The dispcc node declares `#reset-cells = <1>`
(`lito.dtsi:1634`) and the MDSS/DSI blocks are almost certainly held in reset after
the handoff - a block in reset ignores writes, which matches the symptom exactly
the same way a powered-down block does.

Note: `vq_dispcc-lito.c` contains **no** reset map (no `qcom_reset_map`, no `.bit`
entries), so in this kernel version the display resets are not implemented in the
dispcc driver - they are likely owned by GCC instead. Next session should grep the
vendor GCC driver for the display reset lines (`gcc_disp_*` reset entries, or a
`qcom_reset_map` whose offsets sit near the display branch registers at `0xb00c`).

Order for the next attempt: GCC branches (done) -> dispcc reset deassert -> dispcc
branches (done) -> PHY -> controller -> panel, then re-run the `dsi-off` probe with
the observer watching the first ~40 s.

### Reset suspect: closed

Run 501501.0 added the MMSS block reset deassert
(`GCC_MMSS_BCR = 0xb000` from `gcc_lito_resets[]`, `vq_gcc-lito.c:2594`, clear
BIT(0) to release). **No change** - so the display subsystem was not held in reset
by the handoff. The deassert is now in place permanently, as it is required anyway.

### Next suspect: the DSI power rails (and the kernel can reach them)

The DSI PHY needs `vdda-0p9` (`<&L5A>`, `lito-sde.dtsi:649`) and the controller
needs `vdda-1p2` (`<&L9A>`, `:450`), plus `refgen`. If the handoff dropped those
PMIC rails the PHY cannot run, and the symptom is identical to everything else: no
visible change.

The kernel already has the machinery to vote them: `platform/bramble.rs` implements
RPMh regulators for USB, including the id table

```
RPMH_LDOA5  = rpmh_id(b"ldoa5")     <- L5A, the DSI PHY's 0.9 V rail
RPMH_LDOA9  = rpmh_id(b"ldoa9")     <- L9A, the DSI controller's 1.2 V rail
```

with `apply_usb_cx_vote()` (`:2864`) as the working example of an RPMh vote.

The vote mechanism is small and already reusable:

```rust
let address = command_db_read_addr(&RPMH_<RESOURCE>)?;
send_rpmh_command_batch(&[RpmhBcmCommand { address, data: <level> }])
```

and the LDO resource ids are already defined (`RPMH_LDOA5 = rpmh_id(b"ldoa5")`,
`RPMH_LDOA9 = rpmh_id(b"ldoa9")`).

**Open question for the next session**: the *value* to send for an LDO. USB votes a
CX *level* (`usb_cx_level()`), but a regulator vote may be a raw microvolt value or a
voltage-index depending on the resource.

**Resolved (2026-09-20)**: `tmp/qpr1-msm/include/dt-bindings/regulator/qcom,rpmh-regulator-levels.h`
documents its constants as "These levels may be used for **ARC type** RPMh
regulators" (RETENTION 16, MIN_SVS 48, SVS 128, NOM 256, TURBO 384, ...). An LDO is
not an ARC resource, so it takes a **microvolt** value. The DT gives the exact
figures:

```
L5A  (vdda-0p9, DSI PHY)          = 880000 uV     lito-sde.dtsi:667-668
L9A  (vdda-1p2, DSI controller)   = 1152000 uV    lito-sde.dtsi:468-469
```

So the vote is `send_rpmh_command_batch` with the resolved `ldoa5`/`ldoa9` address
and those microvolt values.

### Rail suspect: closed by source evidence

Before implementing the vote, checking the existing USB rail table showed the rails
are already handled:

* `BRAMBLE_QMP_CORE_RAIL` uses `RPMH_LDOA9` with `owner:
  PowerOwner::SecureFirmware` (`platform/bramble.rs:1089-1097`). That is the same
  L9A the DSI controller wants as `vdda-1p2`.
* `RPMH_LDOA5` is already an HS PHY rail (`:3438`), i.e. the same L5A the DSI PHY
  wants as `vdda-0p9`.

So neither rail is missing, and the "handoff dropped the display rails" theory is
closed without spending a run. (The microvolt encoding resolved above is still worth
keeping in the notes.)

### Where that leaves the diagnosis

All the *preconditions* now check out: clocks (GCC + dispcc branches enabled), MMSS
reset released, rails owned/present, panel powered and displaying. The remaining
explanation is that something in the **ported register programming itself** is wrong
- a value, an order, or a missing step - rather than a missing prerequisite.

Next step is therefore a careful line-by-line review of the port against the vendor
sources (`vq_dsi_host.c` for the controller, `dsi_phy_7nm.c` for the PHY), looking
specifically for:

1. Values taken from the *upstream* driver that the *vendor* driver overrides (the
   lane `TX_DCTRL` set was one such case, where the DT's `platform-lane-config`
   differs from upstream's hard-coded table - confirm the port uses the DT).
2. Steps in the vendor sequence with no counterpart in the port (e.g. the DSI
   controller's own reset/`sw_reset_restore`, `dsi_op_mode_config`,
   `dsi_set_tx_power_mode`).
3. The PLL's `set_usecase` step, which the port currently skips
   (`dsi_7nm_set_usecase`, `dsi_phy_7nm.c:691`).

### `set_usecase` step added (run 504224.0): still no change

The port now explicitly clears `CLK_CFG1.BITCLK_SEL` (`dsi_7nm_set_usecase`,
`dsi_phy_7nm.c:691-720`, standalone usecase => `data = 0`), so the internal PLL is
selected regardless of what the bootloader left in that field. No visible change.

### Summary of the elimination campaign (13 runs)

| Suspect | How it was tested | Outcome |
| --- | --- | --- |
| Inert CCS / Run-Stop / park-duration readouts | archive + 5 new channel attempts | closed (timing not observable under `loop`) |
| dispcc inaccessible (GCC AHB gated) | enabled 5 GCC display branches | no effect |
| MMSS held in reset | deasserted `GCC_MMSS_BCR (0xb000)` | no effect |
| Display rails missing | source evidence (L5A/L9A already owned/voted) | closed without a run |
| PLL source not selected | explicit `BITCLK_SEL = 0` | no effect |

Everything in the *preconditions* has now been checked against primary sources and
is in place. The remaining hypothesis is a defect in the **ported programming
itself**, so the next step is a disciplined line-by-line diff of the port against
`vq_dsi_host.c` and `dsi_phy_7nm.c` - value by value, step by step - rather than
another hardware run. Only after that review should the next run happen.

### Line-by-line review: first findings (source only, no run yet)

Reviewing the port against the vendor flow surfaced two structural gaps that no
amount of precondition work would have fixed:

**1. The escape clock is never set up.** The panel init sequence is sent in *LP mode*
(`qcom,mdss-dsi-on-command-state = "dsi_lp_mode"`), and LP-mode commands need the
DSI escape clock. The DSI controller's DT lists `DISP_CC_MDSS_ESC0_CLK`
(`lito-sde.dtsi:457`), whose RCG is `disp_cc_mdss_esc0_clk_src` at `0x2148`
(`vq_dispcc-lito.c`). The vendor computes its rate in `dsi_calc_clk_rate_v2`
(`vq_dsi_host.c:722-753`): the escape clock is the byte clock divided by a 4-bit
divider, chosen by walking `esc_mhz` from 20 down to 5 and taking the first divider
that fits. The port enables the ESC0 *branch* but never programs its source, so the
panel init commands have no escape clock to run on.

**2. The byte and pixel clock sources are never pointed at the DSI PLL.** In
Qualcomm's clock tree the DSI byte/pixel clocks are fed by the PHY PLL output, so the
`disp_cc_mdss_byte0_clk_src` (`0x2110`) and `disp_cc_mdss_pclk0_clk_src` (`0x2098`)
RCGs must select the PLL as parent and take the right divider. The port enables the
branches but leaves both RCGs as the bootloader left them. `dsi_calc_pclk`
(`vq_dsi_host.c:688-708`) gives the target rates:

```
pixel = h_total * v_total * fps            = 175,644,000 Hz
byte  = pixel * bpp / (8 * lanes)          = 131,733,000 Hz
src   = pixel * bpp / 8                    = 526,932,000 Hz   (byte RCG parent rate)
```

Both gaps are exactly the class of defect the review was looking for, and both are
fixable from the vendor source without guessing. Implement them before the next
hardware run.

### The two review findings implemented (run 507765.0): still no change

Both gaps are now fixed in the port: `configure_dsi_rcgs()` points
`byte0_clk_src (0x2110)`, `pclk0_clk_src (0x2098)` and `esc0_clk_src (0x2148)` at
source index 1 (the DSI0 PHY PLL output - `disp_cc_parent_map_0`/`_4`) with
dividers 1/3/7, committing through the RCG command register. The escape clock the
LP-mode panel init needs now exists. **No visible change.**

### Honest state of the campaign (14 runs)

The goal has not moved. The display is a *means*: the real objective is USB gadget
re-enumeration, and the display's job is to give the kernel a device-side readout
(the post-attach `EP0_SETUP_ARMED` / `SETUP_ARM_FAILURE_STAGE` / `SOFFN` / event-ring
state that the archive still lists as open). Today it delivered no pixels, so it
delivered no readout either.

What the 14 runs did buy:

* A complete, source-grounded port of the DSI PHY, controller, panel and clock tree
  (all in `display/`, all cited, 4 host tests passing).
* Five closed readout-channel families (documented so they are never re-tried).
* Five closed precondition suspects (GCC AHB, MMSS reset, rails, PLL source, RCG
  parents) - each eliminated from primary sources or by one run.
* A precise residual: the failure is inside the **ported programming**, not in its
  preconditions. The review has already found two real defects; more remain.

Next step stays the same: continue the line-by-line diff of `display/` against
`vq_dsi_host.c` and `dsi_phy_7nm.c`, fix what it finds, and only then spend a run.

### Review findings 3-4 implemented (run 509858.0): still no change

Two more real defects found by the same review and fixed:

* **`DSI_CTRL_CMD_MODE_EN` was never set** (`dsi.xml.h:133`, value `0x4`). The
  vendor's `dsi_op_mode_config(video_mode=false, enable=true)`
  (`vq_dsi_host.c:1003-1026`) sets it explicitly plus the `CMD_MDP_DONE` interrupt
  mask; without it the controller never enters command mode and will not accept the
  panel's DCS traffic. This was the most promising defect so far.
* **`sw_reset` was invented.** The vendor (`:993-1001`) enables the clocks, writes
  `RESET=1`, *sleeps* `DSI_RESET_TOGGLE_DELAY_MS = 20` ms, then writes `RESET=0`. The
  port had been polling the reset bit instead, which is not what the driver does.

Still no visible change. The review is finding genuine defects (four so far) but has
not yet found the one that matters, so it continues.

### Review queue (next candidates, in order)

1. The PLL's post-divider and `pll_7nm_register` clock tree: the port programs the
   PLL registers directly but never creates the `dsi0_phy_pll_out_byteclk` /
   `_dsiclk` outputs the dispcc RCGs now select. If those outputs are gated
   internally, the byte/pixel clocks stay dead even with the RCGs pointed at them.
2. `dsi_wait4video_eng_busy` / `dsi_tx_buf_free` / `dsi_cmd_dma_tx`'s completion
   handling - the port triggers DMA and polls `TRIG_DMA`, which the vendor does not
   do.
3. `dsi_set_tx_power_mode()` and the `DSI_CMD_DMA_CTRL_LOW_POWER` handling, since
   the panel init runs in LP mode.

### Review finding 5: `CLK_CFG0` (the PLL post-divider) is never programmed

`DSI_7nm_PHY_CMN.CLK_CFG0` (`0x10`) holds `DIV_CTRL_3_0` in bits [3:0] and
`DIV_CTRL_7_4` in [7:4] (`dsi_phy_7nm.xml`). The vendor never writes it from driver
code because the *clock framework* owns the PLL's post-divider clocks
(`clk_regmap_div` ops, registered in `pll_7nm_register`, `dsi_phy_7nm.c:728`). A
bare-metal port has no clock framework, so it must program the dividers itself - and
this port does not, leaving the PLL output rate undefined.

Related: `CLK_CFG1` (`0x14`) holds `CLK_EN` (bit 5), `CLK_EN_SEL` (bit 4),
`BITCLK_SEL` [3:2] and `DSICLK_SEL` [1:0]. The port sets `CLK_EN|CLK_EN_SEL` in
`pll_enable_global_clk()` and clears `BITCLK_SEL` in the `set_usecase` step, but
never touches `DSICLK_SEL`.

Next step: derive the correct `CLK_CFG0` divider values for
`dsi0_phy_pll_out_byteclk` and `dsi0_phy_pll_out_dsiclk` from the vendor's clock
registration (the `clk_regmap_div` entries in `pll_7nm_register`), and set
`DSICLK_SEL` explicitly, then re-run the white-fill probe.

### PLL clock tree resolved (finding 5, values derived)

`pll_7nm_register` (`dsi_phy_7nm.c:754-830`) defines the tree:

```
pll_out_div      = VCO / OUT_DIV        (PLL_OUTDIV_RATE reg, 2 bits, power-of-two)
pll_bit          = pll_out_div / BIT_DIV (CMN_CLK_CFG0 bits [3:0], ONE-BASED)
pll_out_byteclk  = pll_bit / 8
pll_by_2_bit     = pll_bit / 2
pll_post_out_div = pll_out_div / 4
pclk_mux         = mux(pll_bit, pll_by_2_bit) at CMN_CLK_CFG1 bit 0   <- DSICLK_SEL
```

Working backwards from the panel's rates:

```
VCO            = 2 * bit rate            = 2,107,723,680 Hz
OUT_DIV        = 2  -> PLL_OUTDIV_RATE value 1 (power-of-two encoding)
BIT_DIV        = 1  -> CMN.CLK_CFG0 = 0x1 (one-based)
pll_bit        = VCO / 2 / 1             = 1,053,861,840 Hz  (equals the bit rate
                                            the timing calculation already uses)
byteclk        = pll_bit / 8             =   131,732,730 Hz  -> byte0 RCG divider 1
pixel (dsiclk) = pll_bit (mux select 0)  = 1,053,861,840 Hz
pixel RCG div  = 1053.9 / 175.6          = 6   <- the port currently writes 3
esc0 RCG div   = 131.7 / 18.8            = 7   <- correct
```

So three concrete corrections remain: program `CMN.CLK_CFG0 = 1`, set
`PLL.PLL_OUTDIV_RATE = 1`, and fix the pclk0 RCG divider from 3 to 6. `DSICLK_SEL`
should stay 0 (select `pll_bit`).

### Findings 5-6 implemented (run 512879.0): still no change

`CMN.CLK_CFG0 = 1` (bit divider), `PLL.PLL_OUTDIV_RATE = 1` (VCO / 2) and the pclk0
RCG divider corrected from 3 to 6, all derived from the vendor's clock tree rather
than guessed. **No visible change.**

Six real defects have now been found and fixed by the review:

1. escape clock never set up
2. byte/pixel RCGs never pointed at the PLL
3. `DSI_CTRL_CMD_MODE_EN` never set
4. `sw_reset` invented instead of the vendor's 20 ms sleep
5. PLL post-dividers (`CLK_CFG0`, `PLL_OUTDIV_RATE`) never programmed
6. pclk0 divider wrong (3 vs the derived 6)

All six were genuine and all six are now correct, yet the panel still shows nothing.
That means either a seventh defect remains, or one of the six has a companion the
review has not yet paired it with.

### Next idea: make the panel self-report

Rather than another blind white fill, the next probe can ask the *panel* a question
and let the screen carry the answer: send a DCS read (e.g. `0x0A` get_power_mode),
and if the returned value is plausible, fill the screen **white**, otherwise fill it
**red**. That separates "the link is dead in both directions" from "the link works
one way", which no amount of further register review can distinguish.

### BREAKTHROUGH: a working readout channel, and the first two positive results

The readback-plus-panic trick works. `boot-reason.txt` distinguishes "the kernel
panicked" from "the kernel reached its normal park (watchdog)", which gives **one
boolean per run, visible from the host, with no observer and no timing dependency**.
That is the device-side readout the campaign has been missing all along.

Bisection results so far (runs 514590.0 and 516133.0):

| # | Question | `boot-reason` | Answer |
| --- | --- | --- | --- |
| 1 | Did the controller's `CTRL`/`CLK_CTRL` writes stick? | watchdog | **YES** - the DSI block is alive, clocked and out of reset |
| 2 | Did the PHY PLL lock? (`PLL_COMMON_STATUS_ONE` bit 0) | watchdog | **YES** - the PLL is locked |

Both are positives, the first real ones in the whole campaign. The six review fixes
were not wasted: the controller and PLL are now demonstrably up, so the failure is
**downstream of them** - the lane/panel side or the command transmit path.

### Bisection queue (next questions, one per run)

1. Does the command engine accept a transfer? (poll `TRIG_DMA` clearing, or the
   `CMD_MDP_DONE` / error interrupt status after a `cmd_tx`.)
2. Does the panel answer a DCS read? (send `0x0A` get_power_mode and check the
   readback registers; encode the answer as a panic.)
3. Do the DSI lanes reach high-speed state? (`LANE_STATUS0/1` after the PHY enable.)

### Bisection complete on the SoC side: all four answers are YES

| # | Question | `boot-reason` | Answer |
| --- | --- | --- | --- |
| 1 | Do the controller's `CTRL`/`CLK_CTRL` writes stick? | watchdog | **YES** |
| 2 | Does the PHY PLL lock? | watchdog | **YES** |
| 3 | Does the command engine accept the panel-init transfers? | watchdog | **YES** |
| 4 | Do the lanes report any state? (`LANE_STATUS0/1` non-zero) | watchdog | **YES** |

Everything on the SoC side is demonstrably up: the controller is clocked and out of
reset, the PLL is locked, the command path completes, and the lanes report state.
The failure is therefore **outside the SoC** - the physical D-PHY link or the panel.

### The one unported piece: the panel reset

The panel's reset line is `pm8150l_gpios 8` (`lito-bramble-display.dtsi:24`) with
the sequence `<0 10>, <1 10>` (`dsi-panel-sofef01-1080p-cmd.dtsi:43`): low 10 ms,
then high 10 ms. The bootloader performed this once to show its logo, and this port
has never touched it. A panel that has been through a bootloader handoff can ignore a
new DSI session until it is reset, which would explain every symptom at once: all
SoC-side state healthy, nothing on the glass.

That is the next thing to implement - but it needs a PM8150L GPIO write, which is an
SPMI/PMIC operation, not an MMIO one.

**Good news (checked 2026-09-20): the SPMI write path already exists.**
`platform/bramble.rs` has `spmi_write(base, offset, value)` (`:2236`),
`find_spmi_apid(version, ppid)` (`:2247`) for the APID lookup, and the arbiter
window constants (`SPMI_CHANNELS`, `SPMI_CORE`, `SPMI_CONFIG`). So the panel reset
needs no new transport - only the PM8150L GPIO peripheral details:

1. PM8150L's SID and the GPIO peripheral's PPID (from the vendor DT / the pmic
   bindings - do not guess them).
2. The PM8150 GPIO register layout: the mode register (digital in/out), the
   output-level register, and the enable register.
3. Drive GPIO 8 low, wait 10 ms, drive it high, wait 10 ms - the sequence from the
   panel DT (`<0 10>, <1 10>`).

Then re-run the `dsi` probe: with every SoC-side block already verified healthy,
a working reset is the most likely thing to make the panel accept the new session.

### Panel reset: complete spec (all values sourced)

```
PM8150L SID              = 4        (qcom,pm8150l@4, reg = <0x4 SPMI_USID>)
GPIO peripheral base     = 0xc000   (pm8150l_gpios: pinctrl@c000, reg = <0xc000 0xc00>)
Per-pin stride           = 0x100    (PMIC_GPIO_ADDRESS_RANGE,
                                     pinctrl-spmi-gpio.c:24, pad->base = start + i*0x100)
GPIO 8 base              = 0xc000 + 7 * 0x100 = 0xc700

Registers (pinctrl-spmi-gpio.c:43-49):
  MODE_CTL        0x40   function + direction (MODE_FUNCTION_SHIFT 1, MODE_DIR_SHIFT 4)
  DIG_OUT_CTL     0x45   output level
  EN_CTL          0x46   enable

Sequence (panel DT `qcom,mdss-dsi-reset-sequence = <0 10>, <1 10>`):
  1. configure GPIO 8 as a digital output and enable it
  2. drive low,  wait 10 ms
  3. drive high, wait 10 ms
```

Transport: `platform/bramble.rs::spmi_write(base, offset, value)` (`:2236`), with the
APID resolved by `find_spmi_apid(version, ppid)` (`:2247`). Note both are currently
private to that module - expose them (or add a small platform wrapper) rather than
duplicating the arbiter logic.

**Correction (2026-09-20)**: the existing `spmi_write` is *not* the transport this
needs. Its call sites pass `SPMI_CHANNELS` / `SPMI_CORE` / `SPMI_CONFIG`, i.e. the
arbiter's *own* registers (its interrupt controller and mapping tables).

**But the slave-write mechanism does exist**, in the function at
`platform/bramble.rs:2420-2452`. Its form:

```rust
base    = SPMI_CHANNELS
command = (SPMI_OP_EXT_WRITEL << 27) | ((address & 0xff) << 4)
spmi_write(SPMI_CHANNELS, channel + SPMI_WDATA0, value)
spmi_write(SPMI_CHANNELS, channel, command)
poll SPMI_STATUS until DONE; fail on FAILURE | DENIED | DROPPED
```

with `SPMI_OP_EXT_READL` for reads (`*value = spmi_read(SPMI_OBSERVER, channel +
SPMI_RDATA0)`) and a two-byte `spmi_transfer_write_pair` helper right below it.

So the panel reset reduces to: obtain PM8150L's channel (via the existing APID
resolution), then write `0xc700 + 0x46` / `0xc700 + 0x45` / `0xc700 + 0x40`
(driving the level, enabling, and configuring the pin as a digital output) with the
10 ms low / 10 ms high sequence. All values are sourced; nothing needs guessing.

### Final API details for the panel reset

```
spmi_transfer(version, apid, address: u16, value: &mut u8, write: bool) -> bool
      (platform/bramble.rs:2411-2452)
spmi_channel_offset(version, apid, observer) -> usize           (:2399)
spmi_update_bits(version, apid, address, mask, value) -> bool   (:2490)
find_spmi_apid(version, ppid: u16) -> Option<(usize, bool)>     (:2247)
```

`version` and `apid` come from `spmi_write`-state struct's `arbiter_version` / `apid`
fields (`:1954`). The APID is resolved from a **ppid**, and the ppid follows from the
DT's SPMI address: for a child node with `reg = <0xc000 0xc00>` under `pm8150l@4`
(`reg = <0x4 SPMI_USID>`), the ppid is `(sid << 8) | (addr >> 8)` = `(4 << 8) | 0xc0`
= `0x4c0`. That is a derivation from the address format, not a guess - but it should
be confirmed against `find_spmi_apid`'s expectations before the first run.

Suggested implementation order, each step verifiable:

1. Resolve the PM8150L GPIO APID and read `0xc700 + 0x46` (`EN_CTL`) back. A
   successful read proves the transport works; publish via the panic channel.
2. Configure the pin as a digital output and enable it.
3. Drive low 10 ms, high 10 ms.
4. Re-run the `dsi` probe and have the observer watch the first ~40 s.

### Panel reset implemented (run 525271.0): SPMI works, panel reset ran, still no pixels

`reset_panel_gpio8()` (`platform/bramble.rs`) resolves the PM8150L GPIO APID
(`ppid 0x4c0`), reads `EN_CTL` as a liveness check, then drives `DIG_OUT_CTL` low,
waits 10 ms, high, waits 10 ms - the DT's `<0 10>, <1 10>`.

**Result: `boot-reason = watchdog`** - no panic, so the SPMI read succeeded, the APID
resolved, and both writes were accepted. The panel was genuinely reset. Still no
pixels.

### Every testable subsystem is now positive

| # | Question | Answer |
| --- | --- | --- |
| 0 | Does the SPMI slave write path work (panel reset)? | **YES** |
| 1 | Do the DSI controller writes stick? | **YES** |
| 2 | Does the PHY PLL lock? | **YES** |
| 3 | Does the command engine accept transfers? | **YES** |
| 4 | Do the lanes report state? | **YES** |

Five positives, zero failures on anything the kernel can *ask*, and still nothing on
the glass. That leaves the failure in the analogue path - the D-PHY-to-panel
electrical interface itself (lane termination, voltage swing, or the panel's
expectation of the link) - not in anything a register readback can detect.

**This is the same shape as the USB failure.** The USB campaign also ended with a
healthy digital side and a silent analogue one (the PHY completes the chirp handshake
but never receives). Two independent subsystems failing the same way - digital
verified, analogue silent - points at a *shared* underlying condition rather than two
unrelated bugs. Candidate shared causes worth checking before more display work:

1. A PMIC rail that the display and the USB PHY both depend on being present but at
   the wrong voltage (the rails are voted, but the *level* has not been verified for
   either).
2. A TrustZone/XBL constraint on the analogue blocks that the secure firmware applies
   after the bootloader hands off - which would explain why everything the kernel
   writes reads back correctly yet has no physical effect.

### Candidate root cause: the wrong panel variant

The DT (`lito-bramble-display.dtsi`) carries **three** panel candidates:

```
dsi-panel-s6e3hc2-dsc-1080p-cmd     Samsung 1080p DSC
dsi-panel-sofef00-1080p-cmd
dsi-panel-sofef01-1080p-cmd         <- the one this port assumed
```

with `qcom,dsi-default-panel = <&dsi_sofef01_sdc_1080p_cmd>`. That is a *default*,
not the truth: the vendor's panel driver reads the **panel ID** at probe time and
selects the matching driver, so a handset built with a different panel can run a
different init sequence than the DT default suggests.

If this device's panel is not `sofef01`, everything the bisection measured stays
exactly as observed: the controller, PLL, command engine, lanes and SPMI reset are
all fine, the panel is talking, and it is simply being sent the wrong panel's
initialisation - so it shows nothing.

This is a strong hypothesis because it is the only remaining explanation that fits
*all* five positives, and because it is cheap to test:

1. Read the panel ID over DSI (the sofef/samsung panels expose it via a DCS read;
   the read path is not implemented yet) - the definitive answer.
2. Or, faster: try each candidate's init sequence in turn, one run each, and have the
   observer watch the first ~40 s. Three runs settle it.

The candidates differ in a way that matters and is easy to check:

```
sofef01   1080 x 2340   <- this port's assumption
sofef00   1080 x 2160   <- different geometry, so a different panel entirely
s6e3hc2   1080 x ?      (DSC panel; also carries a compression path)
```

`sofef00`'s DT (`dsi-panel-sofef00-1080p-cmd.dtsi`, 94 lines) is already local: same
`dsi_cmd_mode`, 24 bpp, `lane_map_0123`, same `<0 10>, <1 10>` reset and 60 fps, but
2160 rows and its own `on-command` list at line 76. Running its sequence is a
one-line change to `bring_up` plus a run.

Note the user's question about Android settings: Android itself is irrelevant here
(the kernel replaces it under `fastboot boot`), **but** anything the *bootloader*
persists and reads back - panel type, brightness, display config - would have exactly
this effect, and the panel-ID selection is the concrete instance of that.

### Panel variant tested and refuted (run 529562.0)

`dsi00` runs sofef00's own geometry (2160 rows, v_total 2177, its own bit rate) and its
own `on-command` list. Result: `boot-reason = watchdog`, so the sequence completed
with every transfer accepted - and the observer saw **no change** on the glass, exactly
as with sofef01.

So the panel-variant hypothesis is refuted: two different panels' init sequences, two
different geometries and two different bit rates all produce an identical blank
result. That is the same shape as the rest of this campaign - the digital side
accepts everything and the glass stays dark.

The variant selector is kept (`panel::Variant`, `bring_up_variant`, the `dsi00` gate):
if the panel ID is ever read out and turns out to be sofef00, the code is already
there.

**Remaining decisive test**: read the panel ID over DSI. That is the only way to know
which panel is present without the observer, and the DSI read path is not implemented
yet. Until then, no further panel-sequence guesses are worth a run.

### Highest-value next move (zero cost): read the PMIC's own regulator status

The SPMI slave path is proven working (bisection step 0), so the PMIC *itself* can be
interrogated directly. This answers the one open question that both the display and
the USB failure hinge on: whether the rails are actually *at* their intended voltage
at the moment the kernel runs.

Addresses and expectations (`lito-regulators.dtsi`):

```
PM8150 SID = 0   (`qcom,pm8150@0`, `reg = <0x0 SPMI_USID>`)
L5A / pm8150_l5   min 720000, init 720000 uV, proxy-current 23800 uA
L9A / pm8150_l9   min 1152000, init 1152000 uV, proxy-current 51800 uA
```

Both carry `qcom,set = <RPMH_REGULATOR_SET_ALL>`, i.e. they are **RPMh-managed**, not
written directly. That matters: an RPMh vote with an ACTIVE-only scope is dropped when
the AP goes idle, which would explain rails that are correct under the bootloader (the
logo proves they are) and gone by the time the kernel looks. Note a promise of a
*reading* is not a claim about the cause - the PMIC's own status registers are the
authority, not any host-side inference.

So the move is: read the PM8150's LDO status/voltage registers over the now-working
SPMI transport and compare against those numbers. No new hardware, no writes, one run.

### XBL analysis: it initialises MDSS and DISPCC, but not the DSI ctrl/PHY directly

Byte-searching the local bootloader images for the physical base addresses of each
block (`tmp/bramble-factory-*.elf`):

```
                 MDSS       DISPCC     DWC3      GCC     DSI_CTRL0  DSI_PHY0
xbl_a          3 hits     9 hits     1 hit    309 hits   0          0
xbl_core       0          2 hits     0          17       0          0
abl            0          0          0           2       0          0
```

`0xae94000` (DSI controller) and `0xae94400` (DSI PHY) appear **nowhere** in any
bootloader image, yet MDSS and DISPCC are both there - so XBL reaches the DSI link
*through the MDSS wrapper*, not through the ctrl/PHY base addresses.

Two consequences:

1. XBL genuinely initialises the display path (MDSS + DISPCC), which is consistent
   with the handset showing the bootloader logo.
2. This port re-initialises the PHY, PLL, controller and clocks from scratch on top of
   whatever XBL left. If XBL's configuration is correct and self-consistent, that
   re-initialisation may be *destroying* a working link - exactly the failure shape
   observed: every write lands, nothing comes out.

This mirrors the one strategy that already worked here: the DPU was never ported, the
bootloader's DPU configuration was reused (`paint2` read `SSPP_SRC0_ADDR` back
successfully). The same reasoning now applies to the DSI link.

**The next A/B is therefore: do not re-initialise the link.** Skip the PHY/PLL/RCG
programming and send only the panel commands plus the frame data, leaving XBL's DSI
state untouched. One run, one predicate, no new hardware.

### Reuse path tested and refuted (run 535294.0) - with a caveat that matters

`dsireuse` touches no clock, PLL, PHY or controller register; it resets the panel,
sends the panel sequence through whatever XBL left live, then fills white through the
bootloader's DPU configuration (the `paint2` approach that already read
`SSPP_SRC0_ADDR` back successfully).

**Result: `boot-reason = watchdog`** - no panic, so the controller accepted all 11
panel transfers - and the observer saw the Google logo, unchanged.

So "this port's re-initialisation destroys XBL's working link" is refuted too. But the
negative does **not** localise the fault, because the test conflates two stages:

1. the DSI link delivering pixels, and
2. the DPU actually scanning out and fetching from the frame buffer.

A blank result is consistent with either one being dead, and the reuse path never
verified the DPU side (it only relies on the bootloader having left it configured).

**Next step must separate them**, and both halves are readable through the existing
readback channel (no observer needed):

1. After the panel sequence, read the **DSI controller's error/status registers** - if
   the link is erroring, they will say so.
2. Read the **DPU's CTL/Mixer status** - whether it is scanning out and where it is
   fetching from. Comparing that `SSPP_SRC0_ADDR` against this kernel's own frame
   buffer address is the direct test of "the DPU is pointed at the wrong place".

Only when one of those two reads comes back wrong is another display run justified.

### Correction to the caveat above: the fill goes over DSI, not through the DPU

Reading `fill_band` (`display/mod.rs:114`) settles it: it sends CASET, PASET and then
the pixel payload as a DCS RAMWR over the DSI command path. For a command-mode panel
that is the *whole* path - the panel receives pixels over DSI and drives its own glass
itself. The DPU never enters into it.

So the caveat recorded in the previous section was wrong, and the negative results are
stronger than I wrote there: the reuse run *was* a valid test of the DSI link, and a
blank screen means **the panel is not receiving the pixel data over DSI**.

That in turn narrows things sharply. `fill_band`'s transfers all reported success
(`cmd_tx_raw` returned true for CASET/PASET/RAMWR, and `boot-reason = watchdog`), yet
nothing reached the glass. Two possibilities remain and they are distinguishable:

1. The transfers are accepted by the controller but never leave the SoC - a dead
   outbound link.
2. The transfers do arrive and the panel ignores them - e.g. a panel-side state
   problem.

**The distinguishing test is a DSI *read***: issue a DCS read (the panel ID is the
natural one) and see whether any bytes come back. A read that returns data proves the
link carries traffic in both directions, which puts the fault on the panel's
interpretation side; a read that returns nothing proves the link itself is dead. That
read path is not implemented yet, and it is now the single highest-value piece of work
on this workstream - it is also the same primitive that will later read back the
panel's own status during the USB investigation.

### BREAKTHROUGH (run 539565.0): the DSI link is ALIVE in both directions

Implemented the DCS read path (`dsi_ctrl::hw::cmd_rx`, ported from
`vq_dsi_host.c:2049`/`:1299`): send `SET_MAXIMUM_RETURN_PACKET_SIZE` (0x37), clear
`RDBK_DATA_CTRL` (0x1d0), send the read command, then read `RDBK_DATA0` (0x68).

Gate `dsiid` issues DCS `0x04` ("read DDB start", where the panel ID lives) and parks
if a non-zero, non-0xffffffff value comes back, panics if not.

**Result: `boot-reason = watchdog`** - the panel **answered**.

This overturns the working assumption of the last several sections. The link is not
dead: commands reach the panel *and* the panel's responses reach the SoC. Everything
analogue in between - PHY lanes, clock recovery, termination, the panel's DSI front
end - is functioning.

So the fault is narrower than "the analogue path is silent". It is specifically the
**pixel path**: the panel takes commands and answers them, but a CASET/PASET/RAMWR
sequence does not put anything on the glass. Candidates, in order of plausibility:

1. **Chunk size.** `fill_band` sends RAMWR chunks of 5456 pixels = 16368 bytes in one
   transfer. That is far larger than any command the vendor code sends, and may exceed
   what the command FIFO or the panel's receive buffer will take. The write "succeeds"
   at the controller and is dropped downstream.
2. **Address window.** Whether the panel wants CASET/PASET before every RAMWR, or a
   single window set once, differs between the sofef01 and sofef00 sequences.
3. **Panel-side state.** Brightness (0x53 0x28), sleep state, or a manufacturer
   command lock the panel expects before it will display.

The next run should therefore shrink the payload: send one small RAMWR (e.g. a few
hundred pixels) and see whether the glass responds at all - which separates "the
transfer is too large" from "pixels never work".

### Chunk size refuted too (run 545225.0): pixels never land, regardless of payload size

`dsipix2` sends 200 rows x 1080 px in 200-pixel (600-byte) RAMWR chunks - a large,
unmistakable white block, but delivered in small pieces. Observer: black background
with the Google logo, unchanged.

So the payload-size hypothesis is refuted. Putting the whole picture together:

| Measurement | Result |
| --- | --- |
| Commands reach the panel | YES - 11/11 transfers accepted |
| The panel answers reads | YES - DCS 0x04 returned data |
| The panel says it is on | YES - DCS 0x0A reports display-on + normal |
| Pixels reach the glass - large chunks | NO |
| Pixels reach the glass - small chunks | NO |

The link is fully alive in both directions and the panel believes it is displaying,
yet *no* pixel transfer of any size lands. What remains is the panel's own acceptance
of pixel writes - most likely a manufacturer-side condition this port never satisfies:
a command-set unlock that gates memory writes, a partial/idle mode that needs a TE
handshake, or a window/format expectation (e.g. RGB vs BGR packing) that makes the
panel discard the payload.

**The decisive next step is to make the panel identify itself**: read the DDB
(DCS 0x04-0x06, three bytes: manufacturer, model, version) over the now-working read
path. That gives the exact panel model, and with it the vendor's *own* init sequence
for that model - including whatever command gates pixel writes. Guessing further init
sequences without knowing the model is exactly the mistake the variant test already
paid for once.

### The dev overlay's panel + reuse path tested and refuted (run 547915.0)

`dsireuse00` = no re-initialisation + sofef00's own sequence and 2160-row geometry.
Observer: black background with the Google logo, unchanged.

Every combination is now covered, and they all fail identically:

| Panel | Full re-init | Reuse only |
| --- | --- | --- |
| sofef01 (DT default) | blank | blank |
| sofef00 (**dev overlay's choice**) | blank | blank |

So neither the initialisation strategy nor the panel variant explains it - which is
consistent with the measurements that *do* work: commands arrive, the panel answers
reads, and the panel reports display-on/normal. The single thing that never works is
the pixel write itself, at every payload size and through every path.

That makes the panel's own acceptance of memory writes the last remaining variable,
and per the discipline used throughout this workstream the next move is to identify
the panel rather than guess at it: read the DDB (DCS 0x04-0x06) and compare against
the vendor's panel-ID table. The read path is implemented and proven; only the
comparison values need sourcing.

### The 1-bit limit can be lifted: encode values in the Android-return time

The readback channel has been treated as one bit (park => `watchdog`, panic => other).
But the harness *already* records a number: every run logs

```
handset returned via Android after 67 s
```

That is the park duration driving the Android fallback, and it is recorded per run.
So a value can be published by choosing the park length instead of parking for a fixed
interval: park `10 + value` seconds and read the byte straight out of the run's log.

This costs one run per value (rather than one run per bit) and needs no new transport,
no observer and no writes. It turns `cmd_rx` from "yes/no, the panel answered" into
"here is *what* the panel answered", which is exactly what is needed to read the DDB
(manufacturer, model, version) and identify the panel with certainty.

Note the harness's own bounds when choosing the encoding - the passive recovery grace
is 45 s and the stock Android fallback was observed at 66-67 s, so values must be
mapped into a range that stays inside the window the harness tolerates.

### The value channel works, but needs scaling (run 550287.0)

`dsiddb` reads DDB byte 0 and parks for `min(byte, 60)` extra seconds. Result:

```
handset returned via Android after 68 s     (baseline from other runs: 66-67 s)
```

So the encoding is real - the return time did shift with the value - but the harness
reports **whole seconds**, and the baseline itself wobbles by a second or two (66 vs
67 observed). A one-second-per-unit encoding is therefore invisible in the noise.

**Use a scale factor instead:** park `value / 2` or `value / 4` seconds so each unit of
the value maps to several seconds of return time, and compare the run against a same-
day baseline rather than an absolute number. For a byte that is a clear signal (e.g.
a manufacturer byte around 0xE0 = 224 => ~56 s of extra park), this gives a one-run
read of the value with no observer, no writes and no new transport.

Alternatively, when only a *decision* is needed (e.g. "is this sofef00 or sofef01"),
a coarse two-level encoding is enough and far more robust: park 30 s on match, 5 s
otherwise.

### Scaled channel verified working; byte extraction still to confirm (run 551965.0)

With `extra = 1 + byte/4`, a byte of ~0-3 predicts a +1 s park and a return time near
the 66-67 s baseline. Observed: `handset returned via Android after 67 s`. So the run
behaves exactly as the encoding predicts for `byte0 ≈ 0`, which leaves two readings:

1. DDB byte 0 really is ~0 on this panel, or
2. the byte is not in the position this code assumes. `cmd_rx` byte-swaps the whole
   word (`dsi_ctrl.rs`, mirroring `vq_dsi_host.c:1339`) and the gate reads bits
   31:24. If the vendor's copy order differs - it copies from a temp array that was
   filled in reverse register order (`dsi_cmd_dma_rx` loops `i` downward) - the first
   panel byte may land in a different octet.

The channel itself is now proven end to end: the park length demonstrably drives the
recorded return time, so a scaled encoding reads a byte in one run, with no observer,
no writes and no new transport. What remains is a small, well-defined verification of
which octet carries the panel's first DDB byte - and that is now the cheapest step in
the whole workstream.

### Vendor copy order re-checked: the high octet *is* the first byte

Trace of `dsi_cmd_dma_rx` (`vq_dsi_host.c:1299`):

```c
cnt = (rx_byte + 3) >> 2;            // rx_byte = 4 for a short read => cnt = 1
for (i = cnt - 1; i >= 0; i--)        // one pass: i = 0
        *temp++ = ntohl(dsi_read(RDBK_DATA(i)));   // temp[0] = host order of RDBK0
for (i = repeated_bytes; i < 16; i++)
        buf[j++] = reg[i];            // first byte out = reg[0] = high octet
```

So the panel's first byte is the *most significant* octet of the host-order value,
and `cmd_rx`'s `swap_bytes()` plus `(v >> 24) & 0xff` in the gates reproduce that
exactly. The extraction is right; a byte of ~0 from the run therefore means DDB byte 0
genuinely reads as ~0 (or the read returns nothing meaningful at all).

That is what the four-octet sweep settles: if every octet comes back 0, the read is
not returning panel data and the DCS read command/type needs revisiting; if the octets
show a structured, non-zero pattern, the panel identified itself and the bytes can be
matched against the panel-ID table.

### Planned fix if the sweep comes back all-zero: short-read packet size

In `msm_dsi_host_cmd_rx` (`vq_dsi_host.c:2060`) the vendor branches on the requested
length:

```c
if (rlen <= 2) {
        short_response = 1;
        pkt_size = rlen;        // <-- the *requested* length, not a fixed 10
        rx_byte = 4;
} else {
        short_response = 0;
        data_byte = 10;         // long reads ask for 10 at a time
        pkt_size = rlen < data_byte ? rlen : data_byte;
        rx_byte = data_byte + 6;
}
```

This port's `cmd_rx` sends `SET_MAXIMUM_RETURN_PACKET_SIZE` with a fixed 10 regardless,
i.e. the *long-read* size, even for a one-byte DCS read such as DDB byte 0. If the
panel honours that literally it may be waiting to fill a longer response than the read
produces, and the read-back registers come back empty - which is exactly what an
all-zero sweep would look like.

The fix, if the sweep is all zeros: give `cmd_rx` a length parameter and send that
length as the packet size for short reads (`rlen <= 2`), matching the vendor's branch.
Then re-run one octet (`dsiddb0`) and see whether a non-zero byte appears.

### RETRACTION: the return time is not a value channel (run 559803.0)

I claimed the park length drives the harness's recorded `handset returned via Android
after N s`, and built two gates on it. That claim is **wrong**, and the test that
settles it is decisive:

```
gate parks 30 s  ->  "returned via Android after 67 s"
gate parks  5 s  ->  "returned via Android after 67 s"
```

The number does not move with the park length. The ~67 s is the *harness's own*
recovery grace, not the kernel's park, and the earlier 68 s reading was noise. So:

* **There is no value channel.** The readback channel is the one bit it always was:
  park => `boot-reason` = watchdog (no panic), panic => anything else.
* Everything measured through the 1-bit channel stands (the five bisection positives,
  the DCS read returning *something*, the display-on report), because those were
  park-vs-panic tests, not timing tests.
* The byte values published through `dsiddb*` are meaningless and must be ignored.
  In particular the "all four octets are 0" reading proves nothing.

Lesson, recorded against myself: a number appearing in a log is not a channel unless a
controlled experiment shows it moving with the thing that is supposed to drive it. The
68 s observation was a single sample with no control - exactly the kind of evidence
this workstream is supposed to refuse.

**What this leaves.** The read path returns *a* word, and `dsiid`/`dsiid2` proved the
transfers complete. Whether the *content* is real panel data is still open, and it must
be answered with predicates, not timing: e.g. "does DDB byte 0 equal a specific
candidate value" tested one candidate per run through park-vs-panic.

### Bit sweep in progress - and bit 0 already answers the "is the data real?" question

`dsidbit0`..`dsidbit7` test one bit of DDB byte 0 per run: bit set => park (watchdog),
bit clear => panic. First result:

```
dsidbit0  ->  boot-reason = watchdog      (bit 0 SET)
```

A byte whose bit 0 is set is odd, hence non-zero, hence the read is returning *real
data* rather than a zeroed register - so the earlier "all four octets are 0" reading
was an artefact of the retracted value channel, not a property of the panel.

The full byte follows from the remaining seven runs. Once byte 0 is known, the same
sweep gives byte 1 (model) and byte 2 (version), which together identify the panel
completely - 24 runs total, no vendor table needed, and every one of them a clean
park-vs-panic predicate.

### Panel identity: these are Samsung AMOLED DDICs

Public mainline bindings (`samsung,sofef00.yaml`) give the family:

```
samsung,sofef00-ams601nt22    6.01", 1080 x 2160, 18:9    <- matches this DT's sofef00
samsung,sofef00-ams628nw01    6.28", 1080 x 2280, 19:9
samsung,sofef01-m             (the sofef01 variant)
```

So both candidates here are Samsung AMOLED display driver ICs, and the geometry in
`dsi-panel-sofef00-1080p-cmd.dtsi` (1080 x 2160) matches `sofef00-ams601nt22` exactly.
That is a useful cross-check on the dev overlay's choice of sofef00, but it is *not*
proof of which panel this handset carries - only the DDB read can settle that.

Sweep progress at the time of writing: bits 0-3 of DDB byte 0 are all set
(`watchdog`), so byte 0's low nibble is `0xF`.

### RETRACTION #2 (decisive): the `boot-reason` channel does NOT distinguish park from panic

I built every readback measurement on the idea that `boot-reason = watchdog` means
"the gate reached its park" and any other value means "it panicked". A controlled
calibration kills that idea:

```
chan0  = panic immediately, no park at all   ->  boot-reason = watchdog
```

The gate does nothing but `panic!` at once, and the run still reports `watchdog`. So
the panic path also ends in a watchdog reset (the handler evidently hangs rather than
rebooting promptly), and the channel cannot tell the two apart.

**Consequences - all of these are void:**

* The five "bisection positives" (SPMI write, controller writes, PLL lock, command
  engine, lanes). They were all park-vs-panic through this channel.
* `dsiid` / `dsiid2` ("the panel answered", "the panel reports display-on").
* The whole `dsidbit*` / `dsidb*` bit sweep, including "octet 0 and octet 1 are 0xFF".

**What actually survives:**

* **The observer's eyes.** "The panel shows the Google logo and never turns white"
  is direct observation, repeated across many runs - and it is the finding that
  matters. Every pixel test really did fail to paint the glass.
* **Host-side evidence** from the harness logs (USB enumeration, `-110`, Android
  return), which never depended on the kernel's own reporting.
* **Source-level facts:** the register maps, the vendor sequences, the DT contents,
  the XBL byte search, the build-verification command.

**Lesson, the second of its kind today.** This is the same error as the retracted
value channel in a new costume: I inferred a *protocol* (park => watchdog) from
observations that never controlled for the alternative. The first retraction cost a
few runs; this one invalidates a long stretch of work. The fix is the same both times:
**before trusting a channel, calibrate it with a case whose outcome is known** - and
do that *before* spending runs on it.

The way forward does not need this channel: the panel itself is the readout, the
observer reads it directly, and the pixel question ("does anything ever paint?") is
exactly what eyes answer. Slower per run, but real.

### Calibration complete: the two halves are literally identical

```
chan0  immediate panic, no park  ->  boot-reason = watchdog
chan1  park 90 s, then panic     ->  boot-reason = watchdog
```

Not merely indistinguishable in edge cases - identical. The channel has zero
discriminating power, and every readback result built on it is void (see RETRACTION #2).

**The replacement that needs no kernel-side self-reporting: the host's USB view.**

The kernel under test *is* the USB device, so the host's own logs are independent
evidence about what the kernel did:

* host dmesg (`kernel.log`) shows `new high-speed USB device number N` when the
  kernel's gadget reached the point where the host can see it;
* `lsusb-timeline.txt` records what the host enumerated during the window.

So a gate can publish a bit *without the kernel saying anything about itself*: have the
gate decide whether the USB handoff proceeds at all, and let the host's view of the
device be the signal. Critically, this is the *same* channel the USB workstream
already uses (`18d1:4ee7` on fallback, `1234:0001` when the gadget enumerates), so it
comes with a known-good calibration already: those two outcomes are reliably
distinguished in the existing logs.

That is the next thing to build, and this time the calibration comes *first*: run both
outcomes and confirm the host-side logs really differ, before trusting it for anything.

### RETRACTION #3: the polarity was inverted - and the real protocol

With the polarisation question settled, the actual mechanism (from
`flasks/bin/bramble-usb.rs:5358`):

```rust
// The bootreason property is written by the bootloader from the PON
// reset reason: it names what rebooted the handset mid-probe
// (watchdog bite vs PS_HOLD release vs PSCI reboot)
```

So `boot-reason.txt` is `ro.boot.bootreason`: it names **what reset the handset**, not
what the kernel did. And `flasks/src/main.rs:5370` says a gate readout "must complete
before the ~17 s watchdog bite" for the readout to stay clean.

Put together, the protocol is the *opposite* of everything I built on:

```
gate RETURNS normally  ->  the kernel finishes  ->  clean reset -> bootreason != "watchdog"
gate HANGS / SETTLES   ->  APSS WDT bites (~17 s) ->  bootreason == "watchdog"
```

Every gate I wrote used park (=> WDT) on TRUE and panic (=> hangs => WDT) on FALSE, so
*both* arms produced `watchdog`. That is exactly what `chan0` and `chan1` showed.

`chan2` (return at once) and `chan3` (hang) are the calibration that settles it, run
with the corrected polarity.

### FINAL WORD ON CHANNELS: park length does not reach the host either

Three controlled levels, nothing else after the park:

```
chan2  park 30 s  ->  "handset returned via Android after 67 s"
chan3  park  5 s  ->  "handset returned via Android after 67 s"
chan4  park 60 s  ->  "handset returned via Android after 67 s"
```

Flat. The 67 s is the harness's own fixed grace; the kernel's park never reaches the
host-side log. (The archive's `gadget_handoff_failure_stage() * 15` numbers - 1 -> 35 s,
4 -> 80 s, 7 -> 125 s - must therefore have come from a different measurement pathway
than this harness loop reports, not from this line.)

**So there is no kernel-side readback channel at all.** After three attempts at
inventing one, the honest position is:

| Channel | Status |
| --- | --- |
| `boot-reason` watchdog-vs-other | **dead** - identical for park and panic |
| Android return time | **dead** - flat at 67 s for 5/30/60 s parks |
| **The observer's eyes** | **the only direct readout of the panel** |
| **Host-side USB logs** | **independent, and already calibrated** (`18d1:4ee7` vs `-110` vs `1234:0001`) |

Anything that must be *observed on the panel* has to be observed by a human. Anything
the kernel must report has to go through the host's *USB* view, which is the same
channel the USB workstream already trusts.

### New hypothesis worth testing with eyes: CPU RAMWR may be the wrong pixel path

Every pixel test so far pushed frames with CPU-driven DCS RAMWR (`fill_band`:
CASET -> PASET -> 0x2C + payload). But the vendor driver for a panel in *burst* mode
does not do that: the DPU's command-mode DMA engine pushes frames (the "kickoff"
path), and the panel is configured for `qcom,mdss-dsi-burst-mode` in its DT. A panel in
burst mode may simply not accept a CPU-written RAMWR as a frame.

That would explain everything observed: commands accepted, panel reports display-on,
reads answered - and no CPU-pushed frame ever appearing.

The next experiment therefore drives the DPU's own DMA path rather than CPU RAMWR -
which also removes the DPU-reuse uncertainty, because the bootloader's DPU
configuration is what would be doing the pushing.

### FOUND IT: the working channel was in the repo all along

While looking for a replacement channel, `usb_probe.rs:1809-1824` turned up - and its
comment names the answer outright:

```rust
// Publish the retained EP0 command/SETUP classification through
// the only signal channel that has been useful on this board:
// same-boot DCTL stop/run attach cycles.

let code = usb::protocol_readout_code().min(5) as u64;
for _ in 0..code.saturating_add(1) {
    usb::gate_true_stop_device();   // drop the D+/pull-up
    ...
    usb::gate_true_run_device();    // restore it
}
```

The kernel cycles the USB device's presence N times, and the **host** observes the
attach/detach events in its own logs. That is:

* a **multi-valued** channel (not one bit) - the count is whatever the gate wants to
  publish;
* **host-side evidence**, so it does not rely on the kernel reporting anything about
  itself;
* already **proven on this board** - the comment says so, and the archive's readouts
  were built on it.

So the correct protocol for any future gate is:

1. compute the value,
2. publish it as `code + 1` attach cycles via `gate_true_stop_device` /
   `gate_true_run_device`,
3. let the host count the attaches.

Before trusting it, do what should have been done at the start: **calibrate it** - pump
a known count (e.g. 3) and confirm the host log shows exactly that many.

### Attach-cycle channel: the host-side observers are too slow or too filtered

Calibration attempt (`cchan3` = 3 cycles, `cchan7` = 7 cycles, 1 s apart):

```
both runs:  host kernel.log "new high-speed USB device" lines = 1
            lsusb-timeline.txt: 65 polls over ~64 s (1 Hz), fullerene=true never
            usbmon-all.bin: raw mon_bin_hdr ABI, format not matched by a quick parser
```

So the channel's *sender* exists (`gate_true_stop_device` / `gate_true_run_device` are
used by working code in this repo) but the host-side observers available to this
harness cannot resolve 1 Hz cycles:

* `lsusb-timeline.txt` polls at 1 Hz - a cycle that lasts one second is invisible;
* the kernel log line appears once, presumably because the device never gets far enough
  to be logged again before the next cycle (the same `-110` failure the USB workstream
  already knows);
* the usbmon capture is the raw binary ABI and needs a real parser, not a guess.

Conclusion: do not build on this without first making an observer that can actually see
it (e.g. a long-cycle variant: 5 s per state, 3 cycles, then count `kernel.log` lines).

### Back to the channel that works: the observer's eyes

Three attempts at a kernel-side readback channel have now failed, each for a different
reason. The reliable readout on this handset is the panel itself, read by a human. That
is how the workstream's confirmed findings were made (the Google logo never changes),
and it is what the next experiment should use:

**the DPU command-mode DMA hypothesis** - the DT says `qcom,mdss-dsi-burst-mode`, so the
panel likely expects frames pushed by the DPU's command-mode DMA engine rather than the
CPU-driven DCS RAMWR that `fill_band` uses.

### The logo is not evidence of a live link - and why dpupaint could not work

Observer: still the Google logo after painting the active frame buffer white in place.

That is not a surprise once the panel's mode is taken seriously. A **command-mode**
panel receives pixels and then *holds its own frame*: the glass keeps showing the last
frame it was given, with no DSI traffic at all. So:

* "the Google logo is visible" proves the bootloader *once* delivered a frame. It does
  **not** prove the link is alive now - and this puts the earlier `dsiid`/`dsiid2`
  "positives" even further out of reach, since they were already void.
* Painting the frame buffer that a DPU is fetching from changes *nothing* on the glass
  unless something *transmits* the new frame. For a command-mode panel that means a
  **kickoff** (DPU command-mode DMA -> DSI), and for a burst/video-mode panel it means a
  *continuous* stream. `dpupaint` did neither, so it could not have worked.

**So the missing piece is the transmit step, not the buffer.** Two shapes are possible
and the DT's `qcom,mdss-dsi-burst-mode` property points at the second:

1. kick the DPU's command-mode DMA so the frame is pushed to the panel;
2. keep a frame stream running (the panel expects continuous refresh).

Either way it is a *transfer* that is missing, and every pixel experiment so far -
`fill_band`'s CPU RAMWR included - failed to produce one that the panel accepted.

## XBL / TrustZone analysis (2026-09-20): no USB-specific lock found

Ran a byte-level survey of the local bootloader images (`tmp/bramble-factory-*.elf`)
with `tools/xbl_usb_survey.py` and `tools/xbl_ref_classify.py`.

```
xbl_a (3,670,016 B)
  0x0A600000  DWC3        1 hit   - and it is *not* in code: the surrounding bytes are
                                    8-byte entries (0x8076098c,0) (0x807609a8,0) ...
                                    i.e. the physical-address table tagged 0x8076
  0x00100000  GCC       309 hits
  0x088e3000  HS PHY      0 hits   (consistent with the earlier finding that no
                                    firmware image contains 0x088e_xxxx)
xbl_core (2,621,560 B):  GCC 17 hits, no USB PHY, no DWC3
abl (1,048,576 B):       GCC 2 hits
```

GCC page histogram for `xbl_a` shows the bootloader touching `0x00100000`,
`0x0010b000`, `0x00105000`, `0x00104000`, `0x00140000` and **`0x001f0000` (36 hits)**.
That last page is exactly where this kernel's USB clock branches live
(`usb_clock.rs` uses GCC offsets `0xf8000`/`0xf8800`, i.e. `0x001f8000`/`0x001f8800`
from the GCC base).

**Reading:** XBL performs ordinary bootloader USB bring-up - it programs the GCC USB
clock branches and keeps the DWC3 base in its address table - and it *never* touches
the HS PHY. There is no evidence in the bootloader images of a USB-specific
TrustZone/power lock that would explain the silence.

That *strengthens* the existing boundary conclusion rather than opening a new avenue:
the failure sits in the analogue PHY-to-core receive path, which no register exposes
and no bootloader-side lock explains. The documented next step for the USB workstream
(JTAG / secure-debug capture or a wire-level USB protocol analyzer) needs hardware
that is not available here, so the XBL route is closed as a negative.

Scripts kept for reuse: `tools/xbl_usb_survey.py`, `tools/xbl_ref_classify.py`,
`tools/xbl_dwc3_context.py`.

### `matrix` with a Fullerene template: all six routes still fail, and none even attaches

The archive's last runnable idea (its own note: point `--template` at a Fullerene
image, since the default is the *stock* Android image) was run:

```
template = tmp/fullerene-bramble-loop.597007.0/fullerene-bramble-boot.img
routes   = controller / power / typec / typec-role / pdc / smmu
```

Result, from the matrix ledger (`tmp/fullerene-bramble-matrix.<id>.0/matrix-ledger.tsv`)
and the per-route subdirectories:

```
all six routes:  classification=android-fallback   result=fail
                 host attach lines = 0
```

**`attach = 0` is the important part.** A normal `loop` run *does* attach (the host
logs `new high-speed USB device number N`, then `-110`). Under `matrix` there is no
attach at all, on any route - so the route mechanism itself suppresses the handoff
attach rather than changing anything downstream of it. The routes therefore cannot
discriminate the boundary question, and the "controller" route - the interesting one,
which would hand the DWC3 event SPI to the probe's IRQ consumer - is not usable as
built.

My own driver script also mis-reported (it globbed `loop.*` while `matrix` writes
`matrix.*`), which is why the first summary looked like stale data; the ledger is the
authoritative result and is quoted above.

**Net: this closes the archive's last runnable avenue.** The USB boundary stands as
documented - hardware-complete HS attach, correct software state, no controller
reception - and the next real step still needs JTAG, a wire-level analyzer, or
secure-debug, none of which is available.

## Rules for this port

* Source before hypothesis: every register sequence comes from the vendor/upstream
  driver listed above, not from a guessed value. Cite the file and line.
* One stage at a time, each independently verifiable; do not stack unverified
  stages.
* Keep it out of the USB path: the display must never perturb the handoff. Gate
  display init behind its own build flag until it is proven.
* RAM-only `fastboot boot`; no flash, no persistent writes.
