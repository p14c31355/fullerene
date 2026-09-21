# Bramble display - register and sequence reference

Extracted 2026-09-20 from primary sources. This is the porting spec for
`fullerene-kernel/src/arch/aarch64/display/`. Nothing here is guessed: every
offset and value is quoted from the source named in the section header.

## Addresses (vendor DT `lito-sde.dtsi`)

| Block | Base | Size | Source |
| --- | --- | --- | --- |
| MDSS top | `0x0ae00000` | `0x84208` (node), `0xac000` (full) | `lito-sde.dtsi:6,371` |
| DSI controller 0 | `0x0ae94000` | `0x400` | `lito-sde.dtsi:441,445` |
| DSI controller 1 | `0x0ae96000` | `0x400` | `lito-sde.dtsi:491,495` |
| DSI PHY 0 (CMN) | `0x0ae94400` | `0x800` | `lito-sde.dtsi:642-647` |
| DSI PHY 0 dyn refresh | `0x0ae94200` | `0x100` | same |

Both DSI controllers are `qcom,dsi-ctrl-hw-v2.4`; the PHY is `qcom,dsi-phy-v4.1`.

## Vendor DT values the driver consumes (`lito-sde.dtsi:649-660`)

```
vdda-0p9-supply               = <&L5A>       (880000 uV, enable load 36000)
qcom,platform-strength-ctrl   = 55 03  55 03  55 03  55 03  55 00
qcom,platform-lane-config     = 00 00 0a 0a   00 00 0a 0a   00 00 0a 0a
                                00 00 0a 0a   00 00 8a 8a
qcom,platform-regulator-settings = 1d 1d 1d 1d 1d
```

`platform-lane-config` is 4 bytes per lane in the order
`CFG0 CFG1 CFG2 TX_DCTRL` for lanes 0..4 (lane 4 is the clock lane, hence `8a`).
This is the device-authoritative lane setup - the upstream driver's hard-coded
`tx_dctrl_1 = {0x40,0x40,0x40,0x46,0x41}` does **not** match it, so use the DT.

## DSI PHY CMN registers (`dsi_phy_7nm.xml`, domain `DSI_7nm_PHY_CMN`)

```
REVISION_ID0                 0x00000    REVISION_ID1   0x00004
REVISION_ID2                 0x00008    REVISION_ID3   0x0000c
CLK_CFG0                     0x00010    CLK_CFG1       0x00014
GLBL_CTRL                    0x00018    RBUF_CTRL      0x0001c
VREG_CTRL_0                  0x00020    CTRL_0         0x00024
CTRL_1                       0x00028    CTRL_2         0x0002c
CTRL_3                       0x00030    LANE_CFG0      0x00034
LANE_CFG1                    0x00038    PLL_CNTRL      0x0003c
DPHY_SOT                     0x00040    LANE_CTRL0     0x000a0
LANE_CTRL1                   0x000a4    LANE_CTRL2     0x000a8
LANE_CTRL3                   0x000ac    LANE_CTRL4     0x000b0
TIMING_CTRL_0                0x000b4    TIMING_CTRL_1  0x000b8
TIMING_CTRL_2                0x000bc    TIMING_CTRL_3  0x000c0
TIMING_CTRL_4                0x000c4    TIMING_CTRL_5  0x000c8
TIMING_CTRL_6                0x000cc    TIMING_CTRL_7  0x000d0
TIMING_CTRL_8                0x000d4    TIMING_CTRL_9  0x000d8
TIMING_CTRL_10               0x000dc    TIMING_CTRL_11 0x000e0
TIMING_CTRL_12               0x000e4    TIMING_CTRL_13 0x000e8
GLBL_HSTX_STR_CTRL_0         0x000ec    GLBL_HSTX_STR_CTRL_1 0x000f0
GLBL_RESCODE_OFFSET_TOP_CTRL 0x000f4    GLBL_RESCODE_OFFSET_BOT_CTRL 0x000f8
GLBL_RESCODE_OFFSET_MID_CTRL 0x000fc    GLBL_LPTX_STR_CTRL 0x00100
GLBL_PEMPH_CTRL_0            0x00104    GLBL_PEMPH_CTRL_1 0x00108
GLBL_STR_SWI_CAL_SEL_CTRL    0x0010c    VREG_CTRL_1    0x00110
CTRL_4                       0x00114    PHY_STATUS     0x00140
LANE_STATUS0                 0x00148    LANE_STATUS1   0x0014c
GLBL_DIGTOP_SPARE10          0x001ac
```

Other domains in the same file: `DSI_7nm_PHY` (7 regs, per-lane block:
`CFG0 0x00, CFG1 0x04, CFG2 0x08, TEST_DATAPATH 0x0c, PIN_SWAP 0x10,
LPRX_CTRL 0x14, ...`) and `DSI_7nm_PHY_PLL` (155 regs). The per-lane block is
instantiated 5 times with a `0x100` stride (freedreno convention), giving
`LN_<reg>(i) = lane_base + i*0x100 + off`.

Full dump: `tmp/display-src/REGISTER_MAP.md` (277 lines).

## DSI controller registers (`dsi.xml.h`, 438 offsets)

```
CTRL                 0x00000000    CLK_CTRL            0x00000118
RESET                0x00000114    PHY_RESET           0x00000128
INTR_CTRL            0x0000010c    ERR_INT_MASK0       0x00000108
LANE_CTRL            0x000000a8    LANE_SWAP_CTRL      0x000000ac
TRIG_CTRL            0x00000080    TRIG_DMA            0x0000008c
CMD_DMA_CTRL         0x00000038    DMA_BASE            0x00000044
DMA_LEN              0x00000048    CLKOUT_TIMING_CTRL  0x000000c0
CMD_MDP_STREAM_CTRL  0x00000054    EOT_PACKET_CTRL     0x000000c8
DLN0_PHY_ERR         0x000000b0    RDBK_DATA_CTRL      0x000001d0
```

## PHY enable sequence for Lito (`dsi_phy_7nm.c:960-1193`, V4.1, D-PHY, <1.5 GHz)

The bit clock for this panel is ~1.054 Gbps/lane (see `DISPLAY_BRINGUP.md`), so
`less_than_1500_mhz = true` and the V4.1 branch applies:

```
vreg_ctrl_0                = 0x53
vreg_ctrl_1                = 0x5c
glbl_hstx_str_ctrl_0       = 0x88
glbl_pemph_ctrl_0          = 0x00
lane_ctrl0                 = 0x1f
glbl_str_swi_cal_sel_ctrl  = 0x00
glbl_rescode_top_ctrl      = 0x3d
glbl_rescode_bot_ctrl      = 0x39
```

Ordered writes (`dsi_phy_7nm.c:1001-1188`):

```
 1. poll CMN.PHY_STATUS bit0 == 1                  (REFGEN ready; V4.1 needs no
                                                    DIGTOP_SPARE10 write)
 2. CMN.CTRL_0      = DIGTOP_PWRDN_B | PLL_SHUTDOWNB
 3. CMN.PLL_CNTRL   = 0x00                          (assert PLL core reset)
 4. CMN.RBUF_CTRL   = 0x00                          (resync FIFO off)
 5. CMN.CTRL_4      = 0x04  only if REVISION_ID0[7:4] == 0x20
 6. CMN.LANE_CFG0   = 0x21
 7. CMN.LANE_CFG1   = 0x84
 8. CMN.VREG_CTRL_0 = 0x53
 9. CMN.VREG_CTRL_1 = 0x5c
10. CMN.CTRL_3      = 0x00
11. CMN.GLBL_STR_SWI_CAL_SEL_CTRL = 0x00
12. CMN.GLBL_HSTX_STR_CTRL_0      = 0x88
13. CMN.GLBL_PEMPH_CTRL_0         = 0x00
14. CMN.GLBL_RESCODE_OFFSET_TOP_CTRL = 0x3d
15. CMN.GLBL_RESCODE_OFFSET_BOT_CTRL = 0x39
16. CMN.GLBL_LPTX_STR_CTRL        = 0x55
17. CMN.CTRL_0      = 0x7f                          (all power downs removed)
18. CMN.LANE_CTRL0  = 0x1f
19. CMN.CTRL_2      = 0x40                          (full-rate mode)
20. PLL usecase setup (dsi_7nm_set_usecase -> pll/ pll_commit)
21. CMN.TIMING_CTRL_0  = 0x00
22-29. TIMING_CTRL_1..8 = clk_zero, clk_prepare, clk_trail, hs_exit,
                          hs_zero, hs_prepare, hs_trail, hs_rqst
30. CMN.TIMING_CTRL_9  = 0x02
31. CMN.TIMING_CTRL_10 = 0x04
32. CMN.TIMING_CTRL_11 = 0x00
33. CMN.TIMING_CTRL_12 = clk_pre
34. CMN.TIMING_CTRL_13 = clk_post
35. lane settings (below)
```

The timing values in steps 22-34 come from
`msm_dsi_dphy_timing_calc_v4()` (`tmp/display-src/dsi_phy.c`), which derives them
from the panel's 14-byte `qcom,mdss-dsi-panel-phy-timings` table. Port that
function rather than hard-coding its output.

## Lane settings (`dsi_phy_7nm.c:927-958`)

Per-lane block, 5 lanes (0..3 data, 4 clock), stride `0x100`:

```
for i in 0..5:  LN_LPRX_CTRL(i) = 0
                LN_PIN_SWAP(i)  = 0
LN_LPRX_CTRL(0) = 0x3                      (LPCDRX on logical lane 0)
for i in 0..5:  LN_CFG0(i)     = platform-lane-config[i].CFG0   (DT: 0x00)
                LN_CFG1(i)     = platform-lane-config[i].CFG1   (DT: 0x00)
                LN_CFG2(i)     = platform-lane-config[i].CFG2   (DT: 0x0a / 0x8a)
                LN_TX_DCTRL(i) = platform-lane-config[i].TX_DCTRL (DT: 0x0a / 0x8a)
```

## 7nm PLL setup (`dsi_phy_7nm.c:116-570`)

Reference is `VCO_REF_CLK_RATE = 19200000` (19.2 MHz), the same source the USB PHY
work already uses. Spread spectrum is **disabled** (`dsi_pll_setup_config` sets
`enable_ssc = false`), which removes a whole branch from the port.

Divider math (`dsi_pll_calc_dec_frac`, `:127-199`), with `FRAC_BITS = 18`:

```
fref          = 19200000
divider       = fref * 2                       = 38400000
multiplier    = 1 << 18
dec_multiple  = vco_rate * multiplier / divider
dec           = dec_multiple / multiplier
frac          = dec_multiple % multiplier
pll_clock_inverters (V4.1 branch, :176-184):
    vco <= 1000000000  -> 0xa0
    vco <= 2500000000  -> 0x20
    vco <= 3020000000  -> 0x00
    else               -> 0x40
```

For this panel the per-lane bit rate is ~1.054 Gbps, so the VCO is ~2.1 GHz and
`pll_clock_inverters = 0x20`.

Commit (`dsi_pll_commit`, `:326-345`) writes, in order:

```
PLL_CORE_INPUT_OVERRIDE      = 0x12
PLL_DECIMAL_DIV_START_1      = dec
PLL_FRAC_DIV_START_LOW_1     = frac & 0xff
PLL_FRAC_DIV_START_MID_1     = (frac & 0xff00) >> 8
PLL_FRAC_DIV_START_HIGH_1    = (frac & 0x30000) >> 16
PLL_LOCKDET_RATE_1           = 0x40
PLL_LOCK_DELAY               = 0x06
PLL_CMODE_1                  = 0x10                (D-PHY; 0x00 for CPHY)
PLL_CLOCK_INVERTERS_1        = pll_clock_inverters
```

Sequence (`dsi_pll_7nm_vco_set_rate`, `:347-376`):

```
1. enable_pll_bias()      CMN.CTRL_0 |= PLL_SHUTDOWNB;
                          PLL.SYSTEM_MUXES = 0xc0; ndelay(250)
2. setup_config()         ssc_freq 31500, ssc_offset 4800, ssc_adj_per 2, ssc off
3. calc_dec_frac()        (above)
4. calc_ssc()             no-op when ssc disabled
5. commit()               (above)
6. config_hzindep_reg()   (:262)
7. ssc_commit()           no-op when ssc disabled
8. disable_pll_bias()     CMN.CTRL_0 &= ~PLL_SHUTDOWNB;
                          PLL.SYSTEM_MUXES = 0; ndelay(250)
9. wmb()
```

Start and lock (`dsi_pll_7nm_vco_prepare`, `:494-540`):

```
1. enable_pll_bias()
2. CMN.PLL_CNTRL = BIT(0)                     <- start the PLL
3. wmb()
4. poll PLL.PLL_COMMON_STATUS_ONE bit0 (100 us interval, 5000 us timeout)
5. dsi_pll_phy_dig_reset()                    (digital power-on reset)
6. dsi_pll_enable_global_clk()
7. CMN.RBUF_CTRL = 0x1                        <- resync FIFO on
```

PLL register offsets live in `tmp/display-src/REGISTER_MAP.md`, domain
`DSI_7nm_PHY_PLL` (155 regs), read against the PHY's `pll_base`.

## PLL hzindep register table (`dsi_phy_7nm.c:262-324`)

For V4.1 at VCO ~2.1 GHz: `analog_controls_five_1 = 0x01` (only raised to `0x03`
at >= 3.1 GHz on non-V4.0 parts), `vco_config_1 = 0x01` (V4.1: `0x08` below
1.52 GHz, `0x01` below 2.99 GHz).

Then this fixed table, in order (`:294-323`):

```
ANALOG_CONTROLS_FIVE_1            = 0x01 (analog_controls_five_1)
VCO_CONFIG_1                      = 0x01 (vco_config_1)
ANALOG_CONTROLS_FIVE              = 0x01
ANALOG_CONTROLS_TWO               = 0x03
ANALOG_CONTROLS_THREE             = 0x00
DSM_DIVIDER                       = 0x00
FEEDBACK_DIVIDER                  = 0x4e
CALIBRATION_SETTINGS              = 0x40
BAND_SEL_CAL_SETTINGS_THREE       = 0xba
FREQ_DETECT_SETTINGS_ONE          = 0x0c
OUTDIV                            = 0x00
CORE_OVERRIDE                     = 0x00
PLL_DIGITAL_TIMERS_TWO            = 0x08
PLL_PROP_GAIN_RATE_1              = 0x0a
PLL_BAND_SEL_RATE_1               = 0xc0
PLL_INT_GAIN_IFILT_BAND_1         = 0x84, then 0x82
PLL_FL_INT_GAIN_PFILT_BAND_1      = 0x4c
PLL_LOCK_OVERRIDE                 = 0x80
PFILT                             = 0x29, then 0x2f
IFILT                             = 0x2a, then 0x3f   (0x22 only for V4.0)
PERF_OPTIMIZE                     = 0x22            (non-V4.0 only)
```

## Remaining PLL helpers (`dsi_phy_7nm.c:473-492`)

```
dsi_pll_enable_global_clk():
    CMN.CTRL_3 = 0x04
    CMN.CLK_CFG1 |= CLK_EN | CLK_EN_SEL      (bits 5 and 4 -> 0x30)

dsi_pll_phy_dig_reset():
    CMN.GLBL_DIGTOP_SPARE4 (0x00128) = BIT(0); wmb()
    CMN.GLBL_DIGTOP_SPARE4           = 0;      wmb()
```

## DSI controller v2.4 init (`vq_dsi_host.c:823-991`, vendor bramble 4.19 qpr1)

Command-mode path. `phy_shared_timings` are the values from the PHY timing
calculation, which this port already produces (`clk_post`, `clk_pre`).

```
CMD_CFG0          = RGB_SWAP(SWAP_RGB) | DST_FORMAT(cmd_fmt)
CMD_CFG1          = WR_MEM_START(0x2C) | WR_MEM_CONTINUE(0x3C) | INSERT_DCS_COMMAND
CMD_DMA_CTRL      = FROM_FRAME_BUFFER | LOW_POWER
TRIG_CTRL         = TE | MDP_TRIGGER(NONE) | DMA_TRIGGER(SW) | STREAM(channel)
                    | BLOCK_DMA_WITHIN_FRAME        (6G >= v1.2, so v2.4 yes)
CLKOUT_TIMING_CTRL= T_CLK_POST(clk_post) | T_CLK_PRE(clk_pre)
EOT_PACKET_CTRL   = TX_EOT_APPEND                   (unless EOT disabled)
ERR_INT_MASK0     = 0x13ff3fe0
INTR_CTRL         = error mask enabled
CLK_CTRL          = ENABLE_CLKS
CTRL              = CLK_EN | ((LANE0 << lanes) - LANE0) | ENABLE
LANE_SWAP_CTRL    = DLN_SWAP_SEL(dlane_swap)
LANE_CTRL         = CLKLN_HS_FORCE_REQUEST          (if continuous clock)
```

Timing (`dsi_timing_setup`, `:926-991`), command mode branch:

```
wc = hdisplay * bpp / 8 + 1 = 1080 * 24 / 8 + 1 = 3241
CMD_MDP_STREAM_CTRL  = WORD_COUNT(wc) | VIRTUAL_CHANNEL(0) | DATA_TYPE(DCS_LONG_WRITE)
CMD_MDP_STREAM_TOTAL = H_TOTAL(1080) | V_TOTAL(2340)
```

Video mode is not used by this panel, so the `ACTIVE_H`/`ACTIVE_V`/`TOTAL`/
`ACTIVE_HSYNC`/`ACTIVE_VSYNC_*` branch does not apply.

Command TX path: `dsi_cmd_dma_add()` (`:1151`) builds the packet in a TX buffer and
`dsi_cmd_dma_tx()` (`:1266`) writes `DMA_BASE`/`DMA_LEN` and triggers
`TRIG_DMA`; `dsi_cmds2buf_tx()` (`:1349`) walks a `mipi_dsi_msg` array, which is
how the panel's init sequence is sent.

## DPU strategy: reuse the bootloader's configuration, do not port the catalog

The vendor DPU catalog is **not** in `kernel/msm` (the bramble 4.19 qpr1 tree's
`drivers/gpu/drm/msm/disp/dpu1/dpu_hw_catalog.c` is the old SDM845-only upstream
file, 511 lines, zero sm7250/lito entries; `techpack/display` in that tree is a
stub pointing at a separate repository). The local `tmp/qpr1-msm` copy is the same
SDM845-era file.

That is acceptable, because a full DPU port is not the cheapest path to a visible
pixel. Evidence that the DPU register block is still alive and readable after the
handoff: the `paint2` probe successfully **read** `SSPP_SRC0_ADDR` from MDSS and
painted the address it reported (run `448822.0`, no crash). The DPU was *stopped*
(no scanout: filling all 36 MB changed nothing) but its registers survived.

So the intended approach for stage E:

1. Read back the bootloader's DPU state (LM, SSPP, INTF, CTL, top) - the register
   offsets can be discovered from the hardware itself rather than from a catalog.
2. Keep everything that already matches the panel (timings, formats, interface
   mapping) and change only the SSPP source address to our own framebuffer.
3. Re-enable the timing engine and issue the CTL flush so the new frame is
   committed.

This turns the DPU stage from "port a catalog plus its programming sequences" into
"reprogram a handful of registers that the bootloader already configured", which
is far closer to the single-colour milestone. If the read-back shows the DPU was
left in a state we cannot reuse, fall back to fetching a newer upstream
`dpu_hw_catalog.c` (which does carry the Lito-family DPU) and porting the LM/SSPP
sequences from it.

## Display clocks (`vq_dispcc-lito.c`, base `0x0af08000` from `disp_cc_base`)

The DSI controller's DT `clocks` list (`lito-sde.dtsi:452-459`) names six dispcc
clocks: `DISP_CC_MDSS_BYTE0_CLK`, `_BYTE0_CLK_SRC`, `_BYTE0_INTF_CLK`,
`_PCLK0_CLK`, `_PCLK0_CLK_SRC`, `_ESC0_CLK`. The MDSS node adds `_MDSS_AHB_CLK`,
`_MDSS_MDP_CLK`, `_MDSS_VSYNC_CLK`.

Branch enable registers (`.enable_reg`, bit 0, `BRANCH_HALT` on the same offset):

```
disp_cc_mdss_ahb_clk          0x2080      disp_cc_mdss_mdp_clk      0x200c
disp_cc_mdss_byte0_clk        0x2028      disp_cc_mdss_pclk0_clk    0x2004
disp_cc_mdss_byte0_intf_clk   0x202c      disp_cc_mdss_vsync_clk    0x2024
disp_cc_mdss_esc0_clk         0x2038      disp_cc_mdss_rot_clk      0x2014
```

RCG command registers (`.cmd_rcgr`; the config word is at `+0x4`, and `BIT(0)` of
the command register commits an update - the same protocol the USB clock layer
already implements in `platform/bramble/usb_clock.rs`):

```
disp_cc_mdss_byte0_clk_src    0x2110      disp_cc_mdss_mdp_clk_src  0x20c8
disp_cc_mdss_pclk0_clk_src    0x2098      disp_cc_mdss_ahb_clk_src  0x22bc
disp_cc_mdss_esc0_clk_src     0x2148
```

The DSI byte clock is the bit clock divided by 8 and the pixel clock follows the
panel timings; `dsi_calc_pclk()` (`vq_dsi_host.c:688`) is the authority for the
exact rates and must be ported rather than guessed.

## What is still to be extracted before implementing

* `dsi_pll_config_hzindep_reg` (`:262-325`), `dsi_pll_phy_dig_reset`,
  `dsi_pll_enable_global_clk`, `dsi_7nm_set_usecase` (`:691`) - the remaining
  small PLL helpers.
* The v2.4 controller init from the vendor `dsi_ctrl` equivalent:
  `REG_DSI_CTRL`, `CLK_CTRL`, `RESET`, `PHY_RESET`, `LANE_CTRL`, and the
  command-mode path (`CMD_DMA_CTRL`, `DMA_BASE`, `DMA_LEN`, `TRIG_CTRL`).
* DPU: LM/SSPP programming for a solid-colour scanout (`tmp/qpr1-msm/.../disp/dpu1/`).
* Panel power: PM8150L GPIO 8 reset, `dsi_panel_pwr_supply_redbull` rails, TE on
  TLMM 10.

## Reusable helpers already ported in spirit

`msm_dsi_dphy_timing_calc_v4` (`dsi_phy.c:373-461`) is pure integer arithmetic
over `bitclk_rate` and `escclk_rate`, using `linear_inter()` and the percentage
constants `pcnt_clk_prep 50, pcnt_clk_zero 2, pcnt_clk_trail 30, pcnt_hs_prep 50,
pcnt_hs_zero 10, pcnt_hs_trail 30, pcnt_hs_exit 10`, `coeff = 1000`. It has no
hardware dependency and translates directly to Rust.

