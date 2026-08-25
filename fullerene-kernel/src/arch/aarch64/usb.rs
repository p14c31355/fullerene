//! Minimal polled DWC3 device-mode bring-up for the Bramble USB-C port.
//!
//! This is deliberately a descriptor-only gadget for the first hardware
//! milestone.  It is enough to make the Fullerene handoff observable from the
//! host without depending on Android's ADB implementation.  The event ring
//! and EP0 control transfers are polled because the early boot image does not
//! install a DWC3 interrupt route yet.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use super::uart;

const DWC3_BASE: usize = 0x0a60_0000;
const GCC_BASE: usize = 0x0010_0000;
const HSPHY_BASE: usize = 0x088e_3000;
const QMP_BASE: usize = 0x088e_8000;
// SM7250 exposes the Qualcomm glue/QSCRATCH block immediately above the
// DWC3 core.  The glue must report the cable's VBUS/session to the core when
// we take over directly from the bootloader.
const QSCRATCH_BASE: usize = 0x0a6f_8800;
const QSCRATCH_HS_PHY_CTRL: usize = 0x10;
const QSCRATCH_CGCTL: usize = 0x28;
const QSCRATCH_SS_PHY_CTRL: usize = 0x30;
const QSCRATCH_GENERAL_CFG: usize = 0x08;

const GCC_USB30_PRIM_BCR: usize = 0xf000;
const GCC_USB30_PRIM_MASTER_CLK: usize = 0xf010;
const GCC_USB30_PRIM_SLEEP_CLK: usize = 0xf018;
const GCC_USB30_PRIM_MOCK_UTMI_CLK: usize = 0xf01c;
const GCC_USB3_PRIM_CLKREF_CLK: usize = 0x8c010;
const GCC_CFG_NOC_USB3_PRIM_AXI_CLK: usize = 0xf07c;
const GCC_AGGRE_USB3_PRIM_AXI_CLK: usize = 0xf080;
const GCC_QUSB2PHY_PRIM_BCR: usize = 0x12000;
const GCC_USB3_PHY_PRIM_BCR: usize = 0x50000;
const GCC_USB3_DP_PHY_PRIM_BCR: usize = 0x50008;
const GCC_USB3_PRIM_PHY_AUX_CLK: usize = 0xf054;
const GCC_USB3_PRIM_PHY_COM_AUX_CLK: usize = 0xf058;
const GCC_USB3_PRIM_PHY_PIPE_CLK: usize = 0xf05c;

const GCTL: usize = 0xc110;
const GUCTL: usize = 0xc12c;
const GSNPSID: usize = 0xc120;
const GFLADJ: usize = 0xc630;
const GUSB2PHYCFG0: usize = 0xc200;
const GUSB3PIPECTL0: usize = 0xc2c0;
const GEVNTADRLO0: usize = 0xc400;
const GEVNTADRHI0: usize = 0xc404;
const GEVNTSIZ0: usize = 0xc408;
const GEVNTCOUNT0: usize = 0xc40c;
const DCFG: usize = 0xc700;
const DCTL: usize = 0xc704;
const DEVTEN: usize = 0xc708;
const DSTS: usize = 0xc70c;
const DALEPENA: usize = 0xc720;
const DEP_BASE: usize = 0xc800;

const GCTL_PRTCAPDIR_MASK: u32 = 3 << 12;
const GCTL_PRTCAP_DEVICE: u32 = 2 << 12;
const GCTL_DSBLCLKGTNG: u32 = 1;
const GUCTL_REFCLKPER_MASK: u32 = 0xffc0_0000;
const GUCTL_REFCLKPER_19_2MHZ: u32 = 52 << 22;
const GFLADJ_REFCLK_FLADJ_MASK: u32 = 0x003f_ff00;
const GFLADJ_REFCLK_LPM_SEL: u32 = 1 << 23;
const GFLADJ_REFCLK_240MHZ_DECR: u32 = 12 << 24;
const GFLADJ_REFCLK_240MHZDECR_PLS1: u32 = 1 << 31;
const GFLADJ_REFCLK_FLADJ_19_2MHZ: u32 = 200 << 8;
const GUSB2PHYCFG_SUSPHY: u32 = 1 << 6;
const GUSB2PHYCFG_ENBLSLPM: u32 = 1 << 8;
const GUSB3PIPECTL_SUSPHY: u32 = 1 << 17;

const HSPHY_UTMI_CTRL0: usize = 0x3c;
const HSPHY_UTMI_CTRL5: usize = 0x50;
const HSPHY_COMMON0: usize = 0x54;
const HSPHY_COMMON1: usize = 0x58;
const HSPHY_COMMON2: usize = 0x5c;
const HSPHY_CTRL1: usize = 0x60;
const HSPHY_CTRL2: usize = 0x64;
const HSPHY_CFG0: usize = 0x94;
const HSPHY_REFCLK_CTRL: usize = 0xa0;
const HSPHY_RTUNE_SEL: usize = 0xb4;
const HSPHY_TEST0: usize = 0x80;
const HSPHY_TEST1: usize = 0x84;

const HSPHY_UTMI_SLEEPM: u32 = 1 << 0;
const HSPHY_UTMI_ATE_RESET: u32 = 1 << 0;
const HSPHY_UTMI_POR: u32 = 1 << 1;
const HSPHY_COMMON0_FSEL_MASK: u32 = 0x7 << 4;
const HSPHY_COMMON0_VATESTENB_MASK: u32 = 0x3;
const HSPHY_COMMON1_VBUSVLDEXTSEL0: u32 = 1 << 4;
const HSPHY_COMMON1_PLLBTUNE: u32 = 1 << 5;
const HSPHY_COMMON2_VREGBYPASS: u32 = 1 << 0;
const HSPHY_CTRL1_VBUSVLDEXT0: u32 = 1 << 0;
const HSPHY_CTRL2_SUSPEND_N: u32 = 1 << 2;
const HSPHY_CTRL2_SUSPEND_N_SEL: u32 = 1 << 3;
const HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN: u32 = 1 << 1;
const HSPHY_TEST1_TESTDATAOUTSEL: u32 = 1 << 4;
const HSPHY_TEST1_TOGGLE_2WR: u32 = 1 << 6;
const HSPHY_TEST0_DATA_MASK: u32 = 0xff;

const PIPE_UTMI_CLK_SEL: u32 = 1 << 0;
const PIPE3_PHYSTATUS_SW: u32 = 1 << 3;
const PIPE_UTMI_CLK_DIS: u32 = 1 << 8;

const QMP_COM_PHY_MODE_CTRL: usize = 0x0000;
const QMP_COM_SW_RESET: usize = 0x0004;
const QMP_COM_POWER_DOWN_CTRL: usize = 0x0008;
const QMP_COM_RESET_OVRD_CTRL: usize = 0x001c;
const QMP_PCS_STATUS1: usize = 0x1c14;
const QMP_PCS_POWER_DOWN_CONTROL: usize = 0x1c40;
const QMP_PCS_SW_RESET: usize = 0x1c00;
const QMP_PCS_START_CONTROL: usize = 0x1c44;
const QMP_PHYSTATUS: u32 = 1 << 6;

const QMP_INIT: [(usize, u32); 146] = [
    (0x1010, 0x01), // USB3_DP_QSERDES_COM_SSC_EN_CENTER
    (0x101c, 0x31), // USB3_DP_QSERDES_COM_SSC_PER1
    (0x1020, 0x01), // USB3_DP_QSERDES_COM_SSC_PER2
    (0x1024, 0xde), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE1_MODE0
    (0x1028, 0x07), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE2_MODE0
    (0x1030, 0xde), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE1_MODE1
    (0x1034, 0x07), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE2_MODE1
    (0x1050, 0x0a), // USB3_DP_QSERDES_COM_SYSCLK_BUF_ENABLE
    (0x1060, 0x20), // USB3_DP_QSERDES_COM_CMN_IPTRIM
    (0x1074, 0x06), // USB3_DP_QSERDES_COM_CP_CTRL_MODE0
    (0x1078, 0x06), // USB3_DP_QSERDES_COM_CP_CTRL_MODE1
    (0x107c, 0x16), // USB3_DP_QSERDES_COM_PLL_RCTRL_MODE0
    (0x1080, 0x16), // USB3_DP_QSERDES_COM_PLL_RCTRL_MODE1
    (0x1084, 0x36), // USB3_DP_QSERDES_COM_PLL_CCTRL_MODE0
    (0x1088, 0x36), // USB3_DP_QSERDES_COM_PLL_CCTRL_MODE1
    (0x1094, 0x1a), // USB3_DP_QSERDES_COM_SYSCLK_EN_SEL
    (0x10a4, 0x04), // USB3_DP_QSERDES_COM_LOCK_CMP_EN
    (0x10ac, 0x14), // USB3_DP_QSERDES_COM_LOCK_CMP1_MODE0
    (0x10b0, 0x34), // USB3_DP_QSERDES_COM_LOCK_CMP2_MODE0
    (0x10b4, 0x34), // USB3_DP_QSERDES_COM_LOCK_CMP1_MODE1
    (0x10b8, 0x82), // USB3_DP_QSERDES_COM_LOCK_CMP2_MODE1
    (0x10bc, 0x82), // USB3_DP_QSERDES_COM_DEC_START_MODE0
    (0x10c4, 0x82), // USB3_DP_QSERDES_COM_DEC_START_MODE1
    (0x10cc, 0xab), // USB3_DP_QSERDES_COM_DIV_FRAC_START1_MODE0
    (0x10d0, 0xea), // USB3_DP_QSERDES_COM_DIV_FRAC_START2_MODE0
    (0x10d4, 0x02), // USB3_DP_QSERDES_COM_DIV_FRAC_START3_MODE0
    (0x10d8, 0xab), // USB3_DP_QSERDES_COM_DIV_FRAC_START1_MODE1
    (0x10dc, 0xea), // USB3_DP_QSERDES_COM_DIV_FRAC_START2_MODE1
    (0x10e0, 0x02), // USB3_DP_QSERDES_COM_DIV_FRAC_START3_MODE1
    (0x110c, 0x02), // USB3_DP_QSERDES_COM_VCO_TUNE_MAP
    (0x1110, 0x24), // USB3_DP_QSERDES_COM_VCO_TUNE1_MODE0
    (0x1118, 0x24), // USB3_DP_QSERDES_COM_VCO_TUNE1_MODE1
    (0x111c, 0x02), // USB3_DP_QSERDES_COM_VCO_TUNE2_MODE1
    (0x1158, 0x01), // USB3_DP_QSERDES_COM_HSCLK_SEL
    (0x116c, 0x08), // USB3_DP_QSERDES_COM_CORECLK_DIV_MODE1
    (0x11ac, 0xca), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE1_MODE0
    (0x11b0, 0x1e), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE2_MODE0
    (0x11b4, 0xca), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE1_MODE1
    (0x11b8, 0x1e), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE2_MODE1
    (0x11bc, 0x11), // USB3_DP_QSERDES_COM_BIN_VCOCAL_HSCLK_SEL
    (0x1234, 0x00), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_TX
    (0x1238, 0x00), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_RX
    (0x123c, 0x16), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_OFFSET_TX
    (0x1240, 0x05), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_OFFSET_RX
    (0x1284, 0x55), // USB3_DP_QSERDES_TXA_LANE_MODE_1
    (0x1288, 0x02), // USB3_DP_QSERDES_TXA_LANE_MODE_2
    (0x1290, 0x2a), // USB3_DP_QSERDES_TXA_LANE_MODE_4
    (0x1294, 0x3f), // USB3_DP_QSERDES_TXA_LANE_MODE_5
    (0x12a4, 0x12), // USB3_DP_QSERDES_TXA_RCV_DETECT_LVL_2
    (0x12e4, 0x20), // USB3_DP_QSERDES_TXA_PI_QEC_CTRL
    (0x1414, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SO_GAIN
    (0x1430, 0x2f), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_FO_GAIN
    (0x1434, 0x7f), // USB3_DP_QSERDES_RXA_UCDR_SO_SATURATION_AND_ENABLE
    (0x143c, 0xff), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_COUNT_LOW
    (0x1440, 0x0f), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_COUNT_HIGH
    (0x1444, 0x99), // USB3_DP_QSERDES_RXA_UCDR_PI_CONTROLS
    (0x144c, 0x04), // USB3_DP_QSERDES_RXA_UCDR_SB2_THRESH1
    (0x1450, 0x08), // USB3_DP_QSERDES_RXA_UCDR_SB2_THRESH2
    (0x1454, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SB2_GAIN1
    (0x1458, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SB2_GAIN2
    (0x14d4, 0x54), // USB3_DP_QSERDES_RXA_VGA_CAL_CNTRL1
    (0x14d8, 0x08), // USB3_DP_QSERDES_RXA_VGA_CAL_CNTRL2
    (0x14ec, 0x0f), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL2
    (0x14f0, 0x4a), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL3
    (0x14f4, 0x0a), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL4
    (0x14f8, 0xc0), // USB3_DP_QSERDES_RXA_RX_IDAC_TSETTLE_LOW
    (0x14fc, 0x00), // USB3_DP_QSERDES_RXA_RX_IDAC_TSETTLE_HIGH
    (0x1510, 0x77), // USB3_DP_QSERDES_RXA_RX_EQ_OFFSET_ADAPTOR_CNTRL1
    (0x151c, 0x04), // USB3_DP_QSERDES_RXA_SIGDET_CNTRL
    (0x1524, 0x0e), // USB3_DP_QSERDES_RXA_SIGDET_DEGLITCH_CNTRL
    (0x155c, 0xbf), // USB3_DP_QSERDES_RXA_RX_MODE_00_LOW
    (0x1560, 0xbf), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH
    (0x1564, 0x3f), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH2
    (0x1568, 0x7f), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH3
    (0x156c, 0x94), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH4
    (0x1570, 0x5b), // USB3_DP_QSERDES_RXA_RX_MODE_01_LOW
    (0x1574, 0x1b), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH
    (0x1578, 0xd2), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH2
    (0x157c, 0x13), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH3
    (0x1580, 0xa9), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH4
    (0x15a0, 0x04), // USB3_DP_QSERDES_RXA_DFE_EN_TIMER
    (0x15a4, 0x00), // USB3_DP_QSERDES_RXA_DFE_CTLE_POST_CAL_OFFSET
    (0x1460, 0xa0), // USB3_DP_QSERDES_RXA_AUX_DATA_TCOARSE_TFINE
    (0x15a8, 0x0c), // USB3_DP_QSERDES_RXA_DCC_CTRL1
    (0x14dc, 0x00), // USB3_DP_QSERDES_RXA_GM_CAL
    (0x15b0, 0x10), // USB3_DP_QSERDES_RXA_VTH_CODE
    (0x1634, 0x00), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_TX
    (0x1638, 0x00), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_RX
    (0x163c, 0x16), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_OFFSET_TX
    (0x1640, 0x05), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_OFFSET_RX
    (0x1684, 0x55), // USB3_DP_QSERDES_TXB_LANE_MODE_1
    (0x1688, 0x02), // USB3_DP_QSERDES_TXB_LANE_MODE_2
    (0x1690, 0x2a), // USB3_DP_QSERDES_TXB_LANE_MODE_4
    (0x1694, 0x3f), // USB3_DP_QSERDES_TXB_LANE_MODE_5
    (0x16a4, 0x12), // USB3_DP_QSERDES_TXB_RCV_DETECT_LVL_2
    (0x16e4, 0x02), // USB3_DP_QSERDES_TXB_PI_QEC_CTRL
    (0x1814, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SO_GAIN
    (0x1830, 0x2f), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_FO_GAIN
    (0x1834, 0x7f), // USB3_DP_QSERDES_RXB_UCDR_SO_SATURATION_AND_ENABLE
    (0x183c, 0xff), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_COUNT_LOW
    (0x1840, 0x0f), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_COUNT_HIGH
    (0x1844, 0x99), // USB3_DP_QSERDES_RXB_UCDR_PI_CONTROLS
    (0x184c, 0x04), // USB3_DP_QSERDES_RXB_UCDR_SB2_THRESH1
    (0x1850, 0x08), // USB3_DP_QSERDES_RXB_UCDR_SB2_THRESH2
    (0x1854, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SB2_GAIN1
    (0x1858, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SB2_GAIN2
    (0x18d4, 0x54), // USB3_DP_QSERDES_RXB_VGA_CAL_CNTRL1
    (0x18d8, 0x08), // USB3_DP_QSERDES_RXB_VGA_CAL_CNTRL2
    (0x18ec, 0x0f), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL2
    (0x18f0, 0x4a), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL3
    (0x18f4, 0x0a), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL4
    (0x18f8, 0xc0), // USB3_DP_QSERDES_RXB_RX_IDAC_TSETTLE_LOW
    (0x18fc, 0x00), // USB3_DP_QSERDES_RXB_RX_IDAC_TSETTLE_HIGH
    (0x1910, 0x77), // USB3_DP_QSERDES_RXB_RX_EQ_OFFSET_ADAPTOR_CNTRL1
    (0x191c, 0x04), // USB3_DP_QSERDES_RXB_SIGDET_CNTRL
    (0x1924, 0x0e), // USB3_DP_QSERDES_RXB_SIGDET_DEGLITCH_CNTRL
    (0x195c, 0xbf), // USB3_DP_QSERDES_RXB_RX_MODE_00_LOW
    (0x1960, 0xbf), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH
    (0x1964, 0x3f), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH2
    (0x1968, 0x7f), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH3
    (0x196c, 0x94), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH4
    (0x1970, 0x5b), // USB3_DP_QSERDES_RXB_RX_MODE_01_LOW
    (0x1974, 0x1b), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH
    (0x1978, 0xd2), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH2
    (0x197c, 0x13), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH3
    (0x1980, 0xa9), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH4
    (0x19a0, 0x04), // USB3_DP_QSERDES_RXB_DFE_EN_TIMER
    (0x19a4, 0x00), // USB3_DP_QSERDES_RXB_DFE_CTLE_POST_CAL_OFFSET
    (0x1860, 0xa0), // USB3_DP_QSERDES_RXB_AUX_DATA_TCOARSE_TFINE
    (0x19a8, 0x0c), // USB3_DP_QSERDES_RXB_DCC_CTRL1
    (0x18dc, 0x00), // USB3_DP_QSERDES_RXB_GM_CAL
    (0x19b0, 0x10), // USB3_DP_QSERDES_RXB_VTH_CODE
    (0x1cc4, 0xd0), // USB3_DP_PCS_LOCK_DETECT_CONFIG1
    (0x1cc8, 0x07), // USB3_DP_PCS_LOCK_DETECT_CONFIG2
    (0x1ccc, 0x20), // USB3_DP_PCS_LOCK_DETECT_CONFIG3
    (0x1cd8, 0x13), // USB3_DP_PCS_LOCK_DETECT_CONFIG6
    (0x1cdc, 0x21), // USB3_DP_PCS_REFGEN_REQ_CONFIG1
    (0x1d88, 0xaa), // USB3_DP_PCS_RX_SIGDET_LVL
    (0x1db0, 0x0f), // USB3_DP_PCS_CDR_RESET_TIME
    (0x1dc0, 0x88), // USB3_DP_PCS_ALIGN_DETECT_CONFIG1
    (0x1dc4, 0x13), // USB3_DP_PCS_ALIGN_DETECT_CONFIG2
    (0x1dd0, 0x0c), // USB3_DP_PCS_PCS_TX_RX_CONFIG
    (0x1ddc, 0x4b), // USB3_DP_PCS_EQ_CONFIG1
    (0x1dec, 0x10), // USB3_DP_PCS_EQ_CONFIG5
    (0x1f18, 0xf8), // USB3_DP_PCS_USB3_LFPS_DET_HIGH_COUNT_VAL
    (0x1f38, 0x07), // USB3_DP_PCS_USB3_RXEQTRAINING_DFE_TIME_S2
];

const DCTL_RUN_STOP: u32 = 1 << 31;
const DCFG_SPEED_MASK: u32 = 7;
const DCFG_DEVADDR_MASK: u32 = 0x7f << 3;
const DCFG_HIGHSPEED: u32 = 0;
const DCFG_SUPERSPEED: u32 = 4;
const DSTS_CONNECTSPD_MASK: u32 = 7;
const DSTS_DEVCTRLHLT: u32 = 1 << 22;
const DSTS_SUPERSPEED: u32 = 4;

const DEVTEN_DISCONNECT: u32 = 1 << 0;
const DEVTEN_USB_RESET: u32 = 1 << 1;
const DEVTEN_CONNECT_DONE: u32 = 1 << 2;

const DEPCMD_CMDACT: u32 = 1 << 10;
const DEPCMD_DEPSTARTCFG: u32 = 0x09;
const DEPCMD_STARTTRANSFER: u32 = 0x06;
const DEPCMD_SETTRANSFRESOURCE: u32 = 0x02;
const DEPCMD_SETEPCONFIG: u32 = 0x01;
const DEPCMD_ACTION_MODIFY: u32 = 2 << 30;

const DEPCFG_XFER_COMPLETE_EN: u32 = 1 << 8;
const DEPCFG_XFER_NOT_READY_EN: u32 = 1 << 10;
const DEPCFG_EP_NUMBER_SHIFT: u32 = 25;
const DEPCFG_EP_TYPE_CONTROL: u32 = 0;
const DEPCFG_MAX_PACKET_SHIFT: u32 = 3;

const TRB_HWO: u32 = 1 << 0;
const TRB_LST: u32 = 1 << 1;
const TRB_ISP_IMI: u32 = 1 << 10;
const TRB_IOC: u32 = 1 << 11;
const TRB_CONTROL_SETUP: u32 = 2 << 4;
const TRB_CONTROL_STATUS2: u32 = 3 << 4;
const TRB_CONTROL_STATUS3: u32 = 4 << 4;
const TRB_CONTROL_DATA: u32 = 5 << 4;

const EVENT_BUFFER_SIZE: usize = 4096;
const MAX_PACKET_SIZE: u32 = 512;

#[repr(C, align(4096))]
struct EventBuffer([u8; EVENT_BUFFER_SIZE]);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Trb {
    bpl: u32,
    bph: u32,
    size: u32,
    ctrl: u32,
}

#[repr(C, align(64))]
struct ResponseBuffer([u8; 512]);

static mut EVENTS: EventBuffer = EventBuffer([0; EVENT_BUFFER_SIZE]);
#[repr(C, align(64))]
struct SetupPacket([u8; 8]);

static mut SETUP_PACKET: SetupPacket = SetupPacket([0; 8]);
static mut EP0_TRBS: [Trb; 2] = [
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
];
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; 512]);
static mut EVENT_OFFSET: usize = 0;
static mut EP0_STATE: Ep0State = Ep0State::Setup;
static mut CONTROL_IN: bool = false;
static mut CONTROL_HAS_DATA: bool = false;
static mut CONFIGURED: bool = false;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

// USB 2.0 device descriptor: vendor/product IDs are intentionally Fullerene
// development IDs and must not be treated as a released USB allocation.
static DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x01, 0x00, 0, 1, 1, 2, 0, 1,
];

// One vendor-class interface with no data endpoints.  EP0 is sufficient for
// host-side identification and avoids pretending that ADB is implemented.
static CONFIG_DESCRIPTOR: [u8; 18] = [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0];

static LANGID_DESCRIPTOR: [u8; 4] = [4, 3, 0x09, 0x04];
static MANUFACTURER_DESCRIPTOR: [u8; 18] = [
    18, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0,
];
static PRODUCT_DESCRIPTOR: [u8; 36] = [
    34, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0, b' ',
    0, b'A', 0, b'A', 0, b'r', 0, b'c', 0, b'h', 0, b'6', 0, b'4', 0,
];

#[inline]
fn reg(offset: usize) -> *mut u32 {
    (DWC3_BASE + offset) as *mut u32
}

#[inline]
fn qscratch_reg(offset: usize) -> *mut u32 {
    (QSCRATCH_BASE + offset) as *mut u32
}

#[inline]
fn gcc_reg(offset: usize) -> *mut u32 {
    (GCC_BASE + offset) as *mut u32
}

#[inline]
fn hsphy_reg(offset: usize) -> *mut u32 {
    (HSPHY_BASE + offset) as *mut u32
}

#[inline]
fn qmp_reg(offset: usize) -> *mut u32 {
    (QMP_BASE + offset) as *mut u32
}

#[inline]
unsafe fn read_qscratch(offset: usize) -> u32 {
    unsafe { read_volatile(qscratch_reg(offset)) }
}

#[inline]
unsafe fn write_qscratch(offset: usize, value: u32) {
    unsafe { write_volatile(qscratch_reg(offset), value) };
    let _ = unsafe { read_volatile(qscratch_reg(offset)) };
}

#[inline]
unsafe fn gcc_set(offset: usize, mask: u32) {
    let value = unsafe { read_volatile(gcc_reg(offset)) } | mask;
    unsafe { write_volatile(gcc_reg(offset), value) };
    let _ = unsafe { read_volatile(gcc_reg(offset)) };
}

#[inline]
unsafe fn gcc_clear(offset: usize, mask: u32) {
    let value = unsafe { read_volatile(gcc_reg(offset)) } & !mask;
    unsafe { write_volatile(gcc_reg(offset), value) };
    let _ = unsafe { read_volatile(gcc_reg(offset)) };
}

#[inline]
unsafe fn hsphy_update(offset: usize, mask: u32, value: u32) {
    let current = unsafe { read_volatile(hsphy_reg(offset)) };
    unsafe { write_volatile(hsphy_reg(offset), (current & !mask) | (value & mask)) };
    let _ = unsafe { read_volatile(hsphy_reg(offset)) };
}

unsafe fn init_qmp_phy() -> bool {
    unsafe {
        // Match msm_ssphy_qmp_init(): put the combo PHY in USB+DP mode,
        // power its common and PCS blocks, then apply the Lito table.
        write_volatile(qmp_reg(QMP_COM_RESET_OVRD_CTRL), 0x0f);
        write_volatile(qmp_reg(QMP_COM_PHY_MODE_CTRL), 0x03);
        write_volatile(qmp_reg(QMP_COM_RESET_OVRD_CTRL), 0x00);
        write_volatile(qmp_reg(QMP_COM_POWER_DOWN_CTRL), 0x01);
        write_volatile(qmp_reg(QMP_PCS_POWER_DOWN_CONTROL), 0x01);

        for &(offset, value) in QMP_INIT.iter() {
            write_volatile(qmp_reg(offset), value);
        }

        write_volatile(qmp_reg(QMP_COM_SW_RESET), 0x00);
        write_volatile(qmp_reg(QMP_PCS_SW_RESET), 0x00);
        write_volatile(qmp_reg(QMP_PCS_START_CONTROL), 0x03);
        let _ = read_volatile(qmp_reg(QMP_PCS_STATUS1));
        for _ in 0..1_000_000 {
            if read_volatile(qmp_reg(QMP_PCS_STATUS1)) & QMP_PHYSTATUS == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    uart::puts("usb: QMP PHY initialization timeout\n");
    false
}

unsafe fn gcc_reset(offset: usize) {
    unsafe { gcc_set(offset, 1) };
    // The Qualcomm HS PHY driver waits 100--150 us between asserting and
    // deasserting its reset.  A bootloader handoff can leave the reset branch
    // in a partially settled state, so keep the same margin here for both the
    // PHY and DWC3 reset branches.  This is deliberately a calibrated-free
    // lower bound: on the slowest supported Bramble CPUs it is still longer
    // than the documented PHY interval.
    for _ in 0..250_000 {
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    unsafe { gcc_clear(offset, 1) };
    for _ in 0..250_000 {
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
}

/// Apply the small, non-calibration portion of the SM7250 USB2 PHY setup.
///
/// The full Linux driver also obtains regulators and a 19.2 MHz reference
/// clock from the board description. Those are already left on by the
/// Pixel boot chain; the register sequence below is the actual PHY setup
/// from the `qcom,usb-hsphy-snps-femto` driver and the Bramble override
/// sequence in its device tree.
unsafe fn init_hsphy() {
    unsafe {
        hsphy_update(
            HSPHY_CFG0,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
        );
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, HSPHY_UTMI_POR);
        hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_FSEL_MASK, 0);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_PLLBTUNE,
            HSPHY_COMMON1_PLLBTUNE,
        );
        hsphy_update(HSPHY_REFCLK_CTRL, 0x3, 0x2);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
        );
        hsphy_update(
            HSPHY_CTRL1,
            HSPHY_CTRL1_VBUSVLDEXT0,
            HSPHY_CTRL1_VBUSVLDEXT0,
        );

        // qcom,param-override-seq is encoded as (value, register offset).
        write_volatile(hsphy_reg(0x6c), 0x63);
        write_volatile(hsphy_reg(0x70), 0x85);
        write_volatile(hsphy_reg(0x74), 0x17);

        // Bramble does not declare an external-calibration resistor, so the
        // upstream driver enables the internal RTUNE path.
        hsphy_update(HSPHY_RTUNE_SEL, 1, 1);
        hsphy_update(
            HSPHY_COMMON2,
            HSPHY_COMMON2_VREGBYPASS,
            HSPHY_COMMON2_VREGBYPASS,
        );
        // The SNPS Femto driver uses the ATE/test toggle sequence to commit
        // the PHY's analog override values before releasing POR.
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_ATE_RESET, HSPHY_UTMI_ATE_RESET);
        hsphy_update(
            HSPHY_TEST1,
            HSPHY_TEST1_TESTDATAOUTSEL,
            HSPHY_TEST1_TESTDATAOUTSEL,
        );
        hsphy_update(HSPHY_TEST1, HSPHY_TEST1_TOGGLE_2WR, HSPHY_TEST1_TOGGLE_2WR);
        hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_VATESTENB_MASK, 0);
        hsphy_update(HSPHY_TEST0, HSPHY_TEST0_DATA_MASK, 0);
        hsphy_update(
            HSPHY_CTRL2,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
        );
        hsphy_update(HSPHY_UTMI_CTRL0, HSPHY_UTMI_SLEEPM, HSPHY_UTMI_SLEEPM);
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, 0);
        hsphy_update(HSPHY_CTRL2, HSPHY_CTRL2_SUSPEND_N_SEL, 0);
        hsphy_update(HSPHY_CFG0, HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN, 0);
    }
}

unsafe fn select_utmi_pipe_clock() {
    // This is the Qualcomm glue sequence used when DWC3 operates without a
    // SuperSpeed PHY. It prevents the absent QMP PIPE clock from holding the
    // core in reset while the USB2 UTMI clock is already running.
    unsafe {
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        for _ in 0..100_000 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        for _ in 0..100_000 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        let value = read_qscratch(QSCRATCH_GENERAL_CFG) & !PIPE_UTMI_CLK_DIS;
        write_qscratch(QSCRATCH_GENERAL_CFG, value);
    }
}

#[inline]
unsafe fn read(offset: usize) -> u32 {
    unsafe { read_volatile(reg(offset)) }
}

#[inline]
unsafe fn write(offset: usize, value: u32) {
    unsafe { write_volatile(reg(offset), value) }
}

#[inline]
unsafe fn qscratch_set(offset: usize, mask: u32) {
    let value = unsafe { read_qscratch(offset) } | mask;
    unsafe { write_qscratch(offset, value) };
    // The QCOM glue driver performs a readback to make the peripheral-mode
    // session vote visible before it starts the DWC3 core.
    let _ = unsafe { read_qscratch(offset) };
}

#[inline]
unsafe fn dep_reg(endpoint: usize, offset: usize) -> usize {
    DEP_BASE + endpoint * 0x10 + offset
}

unsafe fn cache_clean(address: usize, length: usize) {
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc cvac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

unsafe fn cache_invalidate(address: usize, length: usize) {
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc ivac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

unsafe fn send_ep_command(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> bool {
    unsafe {
        write(dep_reg(endpoint, 0x00), param2);
        write(dep_reg(endpoint, 0x04), param1);
        write(dep_reg(endpoint, 0x08), param0);
        write(dep_reg(endpoint, 0x0c), command | DEPCMD_CMDACT);
    }
    for _ in 0..100_000 {
        let status = unsafe { read(dep_reg(endpoint, 0x0c)) };
        if status & DEPCMD_CMDACT == 0 {
            return status & 0xf000 == 0;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    uart::puts("usb: DWC3 endpoint command timeout\n");
    false
}

unsafe fn configure_endpoint(endpoint: usize, max_packet: u32, modify: bool) -> bool {
    let action = if modify { DEPCMD_ACTION_MODIFY } else { 0 };
    let param0 = action | DEPCFG_EP_TYPE_CONTROL | (max_packet << DEPCFG_MAX_PACKET_SHIFT);
    let param1 = DEPCFG_XFER_COMPLETE_EN
        | DEPCFG_XFER_NOT_READY_EN
        | ((endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT);
    if !unsafe { send_ep_command(endpoint, DEPCMD_SETEPCONFIG, param0, param1, 0) } {
        return false;
    }
    if !modify && !unsafe { send_ep_command(endpoint, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0) } {
        return false;
    }
    true
}

unsafe fn start_transfer(endpoint: usize, trb: *const Trb) -> bool {
    let address = trb as usize as u64;
    unsafe {
        send_ep_command(
            endpoint,
            DEPCMD_STARTTRANSFER,
            address as u32,
            (address >> 32) as u32,
            0,
        )
    }
}

unsafe fn prepare_trb(index: usize, buffer: *const u8, length: usize, kind: u32) {
    let address = buffer as usize as u64;
    let trb = unsafe { addr_of_mut!(EP0_TRBS).cast::<Trb>().add(index) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

unsafe fn start_setup() {
    unsafe {
        prepare_trb(0, addr_of!(SETUP_PACKET).cast::<u8>(), 8, TRB_CONTROL_SETUP);
        let _ = start_transfer(0, addr_of!(EP0_TRBS).cast::<Trb>());
    }
}

unsafe fn start_status(endpoint: usize) {
    let kind = if unsafe { CONTROL_HAS_DATA } {
        TRB_CONTROL_STATUS3
    } else {
        TRB_CONTROL_STATUS2
    };
    unsafe {
        prepare_trb(0, addr_of_mut!(EP0_TRBS).cast::<u8>(), 0, kind);
        let _ = start_transfer(endpoint, addr_of!(EP0_TRBS).cast::<Trb>());
    }
}

fn descriptor(kind: u8, index: u8) -> Option<&'static [u8]> {
    match (kind, index) {
        (1, 0) => Some(&DEVICE_DESCRIPTOR),
        (2, 0) => Some(&CONFIG_DESCRIPTOR),
        (3, 0) => Some(&LANGID_DESCRIPTOR),
        (3, 1) => Some(&MANUFACTURER_DESCRIPTOR),
        (3, 2) => Some(&PRODUCT_DESCRIPTOR),
        _ => None,
    }
}

unsafe fn setup_request() -> [u8; 8] {
    let mut packet = [0; 8];
    unsafe {
        cache_invalidate(addr_of!(SETUP_PACKET) as usize, 8);
        core::ptr::copy_nonoverlapping(addr_of!(SETUP_PACKET).cast::<u8>(), packet.as_mut_ptr(), 8);
    }
    packet
}

unsafe fn handle_setup() {
    let packet = unsafe { setup_request() };
    let request_type = packet[0];
    let request = packet[1];
    let value = u16::from_le_bytes([packet[2], packet[3]]);
    let index = u16::from_le_bytes([packet[4], packet[5]]);
    let requested_length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
    let direction_in = request_type & 0x80 != 0;
    unsafe {
        CONTROL_IN = direction_in;
        CONTROL_HAS_DATA = requested_length != 0;
    }

    if request_type == 0x80 && request == 6 {
        let kind = (value >> 8) as u8;
        let descriptor_index = value as u8;
        if let Some(bytes) = descriptor(kind, descriptor_index) {
            let length = requested_length.min(bytes.len());
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    addr_of_mut!(RESPONSE.0).cast::<u8>(),
                    length,
                );
                cache_clean(addr_of!(RESPONSE) as usize, length);
                prepare_trb(
                    0,
                    addr_of!(RESPONSE.0).cast::<u8>(),
                    length,
                    TRB_CONTROL_DATA,
                );
                EP0_STATE = Ep0State::Data;
                let _ = start_transfer(1, addr_of!(EP0_TRBS).cast::<Trb>());
            }
            return;
        }
    }

    if request_type == 0x80 && request == 0 {
        unsafe {
            RESPONSE.0[0] = 0;
            RESPONSE.0[1] = 0;
            cache_clean(addr_of!(RESPONSE) as usize, 2);
            prepare_trb(
                0,
                addr_of!(RESPONSE.0).cast::<u8>(),
                requested_length.min(2),
                TRB_CONTROL_DATA,
            );
            EP0_STATE = Ep0State::Data;
            let _ = start_transfer(1, addr_of!(EP0_TRBS).cast::<Trb>());
        }
        return;
    }

    if request_type == 0x80 && request == 8 {
        unsafe {
            RESPONSE.0[0] = if CONFIGURED { 1 } else { 0 };
            cache_clean(addr_of!(RESPONSE) as usize, 1);
            prepare_trb(
                0,
                addr_of!(RESPONSE.0).cast::<u8>(),
                requested_length.min(1),
                TRB_CONTROL_DATA,
            );
            EP0_STATE = Ep0State::Data;
            let _ = start_transfer(1, addr_of!(EP0_TRBS).cast::<Trb>());
        }
        return;
    }

    if request_type == 0 && request == 5 {
        let mut dcfg = unsafe { read(DCFG) } & !DCFG_DEVADDR_MASK;
        dcfg |= ((value as u32) & 0x7f) << 3;
        unsafe { write(DCFG, dcfg) };
        unsafe { EP0_STATE = Ep0State::Status };
        return;
    }

    if request_type == 0 && request == 9 {
        unsafe { CONFIGURED = value != 0 };
        unsafe { EP0_STATE = Ep0State::Status };
        uart::puts(if value != 0 {
            "usb: Fullerene configured\n"
        } else {
            "usb: Fullerene deconfigured\n"
        });
        return;
    }

    // Unsupported requests are intentionally left without a transfer. The
    // host will recover with the next bus reset; this keeps the first gadget
    // small while making an accidental ADB claim impossible.
    uart::puts("usb: unsupported control request\n");
    unsafe { EP0_STATE = Ep0State::Setup };
    unsafe { start_setup() };
    let _ = index;
}

unsafe fn process_event(raw: u32) {
    let endpoint_event = (raw & 1) == 0;
    if !endpoint_event {
        let device_event = (raw >> 1) & 0x7f;
        match device_event {
            0 => {}
            1 => unsafe {
                CONFIGURED = false;
                EP0_STATE = Ep0State::Setup;
                write(DCFG, read(DCFG) & !DCFG_DEVADDR_MASK);
                start_setup();
            },
            2 => {
                let speed = unsafe { read(DSTS) & DSTS_CONNECTSPD_MASK };
                uart::puts("usb: connect done, speed=");
                uart::put_hex_value(speed as u64);
                // Linux's DWC3 gadget driver starts with the SuperSpeed EP0
                // size and modifies it after Connect Done.
                let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                unsafe {
                    let _ = configure_endpoint(0, max_packet, true);
                    let _ = configure_endpoint(1, max_packet, true);
                }
            }
            _ => {}
        }
        return;
    }

    let endpoint = ((raw >> 1) & 0x1f) as usize;
    let event = (raw >> 6) & 0xf;
    let status = (raw >> 12) & 0xf;
    if event == 1 {
        unsafe {
            match EP0_STATE {
                Ep0State::Setup => handle_setup(),
                Ep0State::Data if endpoint == 0 || endpoint == 1 => {
                    EP0_STATE = Ep0State::Status;
                }
                Ep0State::Status => {
                    EP0_STATE = Ep0State::Setup;
                    start_setup();
                }
                _ => {}
            }
        }
    } else if event == 3 && status == 2 {
        unsafe {
            if EP0_STATE == Ep0State::Status {
                start_status(if CONTROL_HAS_DATA && CONTROL_IN { 0 } else { 1 });
            }
        }
    }
}

/// Initialize the Bramble DWC3 in peripheral mode and connect the pull-up.
pub fn init() -> bool {
    init_with_super_speed(true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    init_with_super_speed(false)
}

fn init_with_super_speed(super_speed: bool) -> bool {
    unsafe {
        let snpsid = read(GSNPSID);
        uart::put_hex("usb: DWC3 GSNPSID=", snpsid as u64);

        // The Linux lito-usb device tree supplies these clocks and resets to
        // the Qualcomm glue.  A RAM-booted Fullerene image has no clock
        // framework yet, so perform the small branch/reset part directly.
        gcc_reset(GCC_USB30_PRIM_BCR);
        gcc_reset(GCC_QUSB2PHY_PRIM_BCR);
        if super_speed {
            gcc_reset(GCC_USB3_PHY_PRIM_BCR);
            gcc_reset(GCC_USB3_DP_PHY_PRIM_BCR);
        }
        for offset in [
            GCC_USB30_PRIM_MASTER_CLK,
            GCC_CFG_NOC_USB3_PRIM_AXI_CLK,
            GCC_AGGRE_USB3_PRIM_AXI_CLK,
            GCC_USB30_PRIM_MOCK_UTMI_CLK,
            GCC_USB30_PRIM_SLEEP_CLK,
            GCC_USB3_PRIM_CLKREF_CLK,
            GCC_USB3_PRIM_PHY_AUX_CLK,
            GCC_USB3_PRIM_PHY_COM_AUX_CLK,
            GCC_USB3_PRIM_PHY_PIPE_CLK,
        ] {
            gcc_set(offset, 1);
        }

        init_hsphy();
        let qmp_ready = if super_speed {
            gcc_reset(GCC_USB3_DP_PHY_PRIM_BCR);
            gcc_reset(GCC_USB3_PHY_PRIM_BCR);
            init_qmp_phy()
        } else {
            false
        };
        // The Qualcomm glue only selects the UTMI fallback clock when no
        // SuperSpeed PHY is available. Applying that sequence after a
        // successful QMP bring-up can hold the SS link in the wrong state.
        if !qmp_ready {
            select_utmi_pipe_clock();
        }

        // Match the QCOM DWC3 glue's peripheral-mode VBUS override.  The
        // bootloader's fastboot role is not a complete kernel-side OTG
        // session, so relying on the core alone leaves the device halted.
        if qmp_ready {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // The Qualcomm glue driver keeps the DWC3 RAM master clock alive;
        // without this, endpoint commands can be accepted but never retire.
        qscratch_set(QSCRATCH_CGCTL, 0x18);

        // Lito/SM7250 supplies the DWC3 reference clock at 19.2 MHz.  These
        // are the same values used by the Qualcomm MSM glue driver; leaving
        // the POR values here prevents the link state machine from reaching
        // Connect Done even when VBUS is valid.
        let guctl = read(GUCTL);
        write(
            GUCTL,
            (guctl & !GUCTL_REFCLKPER_MASK) | GUCTL_REFCLKPER_19_2MHZ,
        );
        let gfladj = read(GFLADJ);
        write(
            GFLADJ,
            (gfladj
                & !(GFLADJ_REFCLK_FLADJ_MASK
                    | GFLADJ_REFCLK_LPM_SEL
                    | GFLADJ_REFCLK_240MHZ_DECR
                    | GFLADJ_REFCLK_240MHZDECR_PLS1))
                | GFLADJ_REFCLK_LPM_SEL
                | GFLADJ_REFCLK_240MHZ_DECR
                | GFLADJ_REFCLK_240MHZDECR_PLS1
                | GFLADJ_REFCLK_FLADJ_19_2MHZ,
        );

        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        if qmp_ready {
            usb3 &= !GUSB3PIPECTL_SUSPHY;
        } else {
            // Keep the USB2 gadget usable if the board-specific SuperSpeed
            // calibration does not reach PHY ready.
            usb3 |= GUSB3PIPECTL_SUSPHY;
        }
        write(GUSB3PIPECTL0, usb3);

        let event_address = addr_of!(EVENTS) as usize as u64;
        // The event ring lives in the normal-cacheable early heap mapping.
        // Evict any CPU-side zero-fill before handing the buffer to DWC3;
        // otherwise a later cache writeback could overwrite an event that the
        // controller has already posted.
        cache_clean(addr_of!(EVENTS) as usize, EVENT_BUFFER_SIZE);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, EVENT_BUFFER_SIZE as u32);
        write(GEVNTCOUNT0, 0);
        EVENT_OFFSET = 0;
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;

        // The bootloader may leave DCFG in the speed/address state of its
        // Fastboot session. Reset both fields explicitly before enabling the
        // pull-up; Linux's gadget path selects the maximum PHY-backed speed
        // at the same point in its start sequence.
        let mut dcfg = read(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK);
        dcfg |= if qmp_ready {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);

        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            uart::puts("usb: DEPSTARTCFG failed\n");
            return false;
        }
        // Use a USB2-sized EP0 for the first probe; the normal kernel starts
        // with the SuperSpeed-sized endpoint and adjusts it on Connect Done.
        let ep0_packet_size = if qmp_ready { MAX_PACKET_SIZE } else { 64 };
        if !configure_endpoint(0, ep0_packet_size, false)
            || !configure_endpoint(1, ep0_packet_size, false)
        {
            uart::puts("usb: EP0 configuration failed\n");
            return false;
        }
        write(DALEPENA, 0b11);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE,
        );
        start_setup();

        let mut dctl = read(DCTL);
        dctl |= DCTL_RUN_STOP;
        write(DCTL, dctl);
        let mut halted = true;
        for _ in 0..100_000 {
            if read(DSTS) & DSTS_DEVCTRLHLT == 0 {
                halted = false;
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        if halted {
            uart::put_hex("usb: DWC3 remained halted, DSTS=", read(DSTS) as u64);
            return false;
        }
        uart::puts("usb: Fullerene DWC3 gadget connected\n");
    }
    true
}

/// Poll the DWC3 event ring. This is intentionally cheap enough to run from
/// the early boot loop until the normal interrupt controller owns the device.
pub fn poll() {
    unsafe {
        let count = read(GEVNTCOUNT0) & 0xfffc;
        if count == 0 {
            return;
        }
        let mut remaining = count as usize;
        while remaining >= 4 {
            let offset = EVENT_OFFSET;
            cache_invalidate(addr_of!(EVENTS) as usize + offset, 4);
            let raw = u32::from_le_bytes([
                EVENTS.0[offset],
                EVENTS.0[offset + 1],
                EVENTS.0[offset + 2],
                EVENTS.0[offset + 3],
            ]);
            process_event(raw);
            EVENT_OFFSET = (offset + 4) % EVENT_BUFFER_SIZE;
            remaining -= 4;
        }
        write(GEVNTCOUNT0, count);
    }
}
