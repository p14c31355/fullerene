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
3. **Stage C - DSI PHY.** PLL to the computed rate, lane settings from the panel's
   PHY timings table, PHY enable sequence. Verifiable by the PLL lock status.
4. **Stage D - panel init.** Reset sequence then the 66-command init over DSI in
   LP mode. Verifiable by the panel responding (and visually: the panel leaves the
   held logo).
5. **Stage E - scanout.** DPU LM/SSPP programming with a solid colour, then the
   text renderer. **First visual proof.**
6. **Stage F - the payoff.** Print the USB device-side state (`EP0_SETUP_ARMED`,
   `SETUP_ARM_FAILURE_STAGE`, `SOFFN`, event ring, `DSTS`/`DCTL`) at the attach
   moment, which is the measurement the archive still lists as open.

## Rules for this port

* Source before hypothesis: every register sequence comes from the vendor/upstream
  driver listed above, not from a guessed value. Cite the file and line.
* One stage at a time, each independently verifiable; do not stack unverified
  stages.
* Keep it out of the USB path: the display must never perturb the handoff. Gate
  display init behind its own build flag until it is proven.
* RAM-only `fastboot boot`; no flash, no persistent writes.
