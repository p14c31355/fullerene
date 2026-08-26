//! Minimal polled DWC3 device-mode bring-up for the Bramble USB-C port.
//!
//! This is deliberately a descriptor-only gadget for the first hardware
//! milestone.  It is enough to make the Fullerene handoff observable from the
//! host without depending on Android's ADB implementation.  The event ring
//! and EP0 control transfers are polled because the early boot image does not
//! install a DWC3 interrupt route yet.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use super::uart;

#[inline]
fn log_puts(message: &str) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::puts(message);
}

#[inline]
fn log_hex(prefix: &str, value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::put_hex(prefix, value);
}

#[inline]
fn log_hex_value(value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::put_hex_value(value);
}

unsafe extern "C" {
    static __usb_dma_start: u8;
    static __usb_dma_end: u8;
}

const DWC3_BASE: usize = 0x0a60_0000;
// Lito/SM7250's Apps SMMU owns the DWC3 stream ID declared by the board DT.
// The early Bramble path installs a small identity map in a context bank so
// the USB buffers remain inside the IOVA pool declared by the vendor DT.
// Google’s Bramble/Lito DTS places apps-smmu at 0x0c600000; 0x15000000 was
// an incorrect address and could send the probe into its exception fallback
// before the first EP0 transfer.
const APPS_SMMU_BASE: usize = 0x0c60_0000;
const DWC3_STREAM_ID: u32 = 0xe0;
const SMMU_ID0: usize = 0x20;
const SMMU_ID1: usize = 0x24;
const SMMU_ID0_NUMSMRG_MASK: u32 = 0xff;
const SMMU_ID1_PAGESIZE: u32 = 1 << 31;
const SMMU_ID1_NUMPAGENDXB_SHIFT: u32 = 28;
const SMMU_ID1_NUMPAGENDXB_MASK: u32 = 0x7;
const SMMU_ID1_NUMS2CB_SHIFT: u32 = 16;
const SMMU_ID1_NUMS2CB_MASK: u32 = 0xff;
const SMMU_ID1_NUMCB_MASK: u32 = 0xff;
const SMMU_SMR_BASE: usize = 0x800;
const SMMU_S2CR_BASE: usize = 0xc00;
const SMMU_TLB_ALL_H: usize = 0x6c;
const SMMU_TLB_SYNC: usize = 0x70;
const SMMU_TLB_STATUS: usize = 0x74;
const SMMU_TLB_STATUS_ACTIVE: u32 = 1;
const SMMU_SMR_VALID: u32 = 1 << 31;
const SMMU_SMR_MASK_SHIFT: u32 = 16;
const SMMU_S2CR_TYPE_MASK: u32 = 0x3 << 16;
const SMMU_S2CR_TYPE_TRANS: u32 = 0;
const SMMU_S2CR_CBNDX_MASK: u32 = 0xff;
const SMMU_GR1_CBAR_BASE: usize = 0x00;
const SMMU_GR1_CBA2R_BASE: usize = 0x800;
const SMMU_CBA2R_VA64: u32 = 1;
const SMMU_CBAR_S1_TRANS_S2_BYPASS: u32 = 1 << 16;
const SMMU_CBAR_S1_MEMATTR_WB: u32 = 0xf << 12;
const SMMU_CBAR_S1_BPSHCFG_NSH: u32 = 3 << 8;
const SMMU_CB_SCTLR: usize = 0x00;
const SMMU_CB_TCR2: usize = 0x10;
const SMMU_CB_TTBR0: usize = 0x20;
const SMMU_CB_TTBR1: usize = 0x28;
const SMMU_CB_TCR: usize = 0x30;
const SMMU_CB_CONTEXTIDR: usize = 0x34;
const SMMU_CB_MAIR0: usize = 0x38;
const SMMU_CB_MAIR1: usize = 0x3c;
const SMMU_SCTLR_S1_ASIDPNE: u32 = 1 << 12;
const SMMU_SCTLR_CFIE: u32 = 1 << 6;
const SMMU_SCTLR_CFRE: u32 = 1 << 5;
const SMMU_SCTLR_AFE: u32 = 1 << 2;
const SMMU_SCTLR_TRE: u32 = 1 << 1;
const SMMU_SCTLR_M: u32 = 1;
const SMMU_TCR_EPD1: u32 = 1 << 23;
const SMMU_TCR_SH0_INNER: u32 = 3 << 12;
const SMMU_TCR_ORGN0_WBWA: u32 = 1 << 10;
const SMMU_TCR_IRGN0_WBWA: u32 = 1 << 8;
const SMMU_TCR_T0SZ_32BIT: u32 = 32;
const SMMU_TCR2_SEP_UPSTREAM: u32 = 0x7 << 15;
const SMMU_TCR2_AS: u32 = 1 << 4;
const SMMU_TCR2_PASIZE_40BIT: u32 = 2;

#[repr(C, align(4096))]
struct SmmuTable([u64; 512]);

// With T0SZ=32 and a 4 KiB granule, TTBR0 points at a level-1 table. Four
// 1 GiB block descriptors cover the complete 32-bit IOVA space, including
// the vendor DT's 0x90000000..0xf0000000 USB pool and our 0x9b800000 DMA
// section. This table is cleared together with the other USB DMA objects.
#[unsafe(link_section = ".usb_dma")]
static mut SMMU_L1: SmmuTable = SmmuTable([0; 512]);

const SMMU_DESC_VALID: u64 = 1;
const SMMU_DESC_AF: u64 = 1 << 10;
const SMMU_DESC_SH_INNER: u64 = 3 << 8;
const SMMU_DESC_ATTR_NORMAL: u64 = 0;
const SMMU_DESC_XN: u64 = (1 << 53) | (1 << 54);
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
const QSCRATCH_GENERAL_CFG_XHCI_REV: u32 = 1 << 2;

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
const GUCTL1: usize = 0xc360;
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
const GCTL_CORESOFTRESET: u32 = 1 << 11;
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
const GUSB2PHYCFG_PHYSOFTRST: u32 = 1 << 31;
const GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST: u32 = 1 << 8;
const GUSB3PIPECTL_SUSPHY: u32 = 1 << 17;
const GUSB3PIPECTL_PHYSOFTRST: u32 = 1 << 31;

const DCTL_CSFTRST: u32 = 1 << 30;
const DCTL_TRGTULST_MASK: u32 = 0x0f << 17;
const DCTL_TRGTULST_RX_DET: u32 = 5 << 17;
// Linux applies the RxDetect reconnect workaround only through DWC3 1.87a.
// GSNPSID carries the same full revision value used by the upstream driver.
const DWC3_REVISION_187A: u32 = 0x5533_187a;

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
const DSTS_DCNRD: u32 = 1 << 23;
const DSTS_SUPERSPEED: u32 = 4;

const DEVTEN_DISCONNECT: u32 = 1 << 0;
const DEVTEN_USB_RESET: u32 = 1 << 1;
const DEVTEN_CONNECT_DONE: u32 = 1 << 2;
// DWC3 event words use bit 0 to distinguish endpoint events from
// device-specific events.  For a device event, bits 1..7 are zero and the
// Reset/Connect Done kind is stored in the four-bit type field at bits 8..11.
const DEVICE_EVENT_KIND_SHIFT: u32 = 8;
const DEVICE_EVENT_KIND_MASK: u32 = 0x0f;

const DEPCMD_CMDACT: u32 = 1 << 10;
const DEPCMD_HIPRI_FORCERM: u32 = 1 << 11;
const DEPCMD_PARAM_SHIFT: u32 = 16;
const DEPCMD_DEPSTARTCFG: u32 = 0x09;
const DEPCMD_ENDTRANSFER: u32 = 0x08;
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

#[unsafe(link_section = ".usb_dma")]
static mut EVENTS: EventBuffer = EventBuffer([0; EVENT_BUFFER_SIZE]);
#[repr(C, align(64))]
struct SetupPacket([u8; 8]);

#[unsafe(link_section = ".usb_dma")]
static mut SETUP_PACKET: SetupPacket = SetupPacket([0; 8]);
#[unsafe(link_section = ".usb_dma")]
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
#[unsafe(link_section = ".usb_dma")]
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; 512]);
static mut EVENT_OFFSET: usize = 0;
static mut EP0_STATE: Ep0State = Ep0State::Setup;
static mut CONTROL_IN: bool = false;
static mut CONTROL_HAS_DATA: bool = false;
static mut CONFIGURED: bool = false;
static mut ENDPOINTS_READY: bool = false;

const USB_TRACE_CAPACITY: usize = 256;

// Numeric events keep the early USB path independent of UART, locks, and
// formatting. The buffer is CPU-owned; it is placed beside the DMA objects so
// a probe can preserve the same identity-mapped address discipline.
const TRACE_INIT: u32 = 1;
const TRACE_DEVICE_RESET: u32 = 2;
const TRACE_DEVICE_CONNECT: u32 = 3;
const TRACE_EP_COMMAND_ISSUE: u32 = 4;
const TRACE_EP_COMMAND_DONE: u32 = 5;
const TRACE_EP_COMMAND_TIMEOUT: u32 = 6;
const TRACE_SETUP_QUEUED: u32 = 7;
const TRACE_SETUP_RECEIVED: u32 = 8;
const TRACE_DESCRIPTOR_QUEUED: u32 = 9;
const TRACE_STATUS_QUEUED: u32 = 10;
const TRACE_TRANSFER_COMPLETE: u32 = 11;
const TRACE_USB_RESET: u32 = 12;
pub const TRACE_BOOT_USB_ENTRY: u32 = 13;
pub const TRACE_TYPEC_BEGIN: u32 = 14;
pub const TRACE_TYPEC_DONE: u32 = 15;
pub const TRACE_USB_HANDOFF_BEGIN: u32 = 16;
const TRACE_DWC3_RESET_BEGIN: u32 = 17;
const TRACE_QSCRATCH_BEGIN: u32 = 18;
pub const TRACE_EXCEPTION_SYNC: u32 = 19;

#[repr(C)]
#[derive(Clone, Copy)]
struct UsbTraceEntry {
    sequence: u32,
    event: u32,
    request: u32,
    value: u32,
    index: u32,
    length: u32,
    ep0_state: u32,
    status: u32,
}

const EMPTY_USB_TRACE: UsbTraceEntry = UsbTraceEntry {
    sequence: 0,
    event: 0,
    request: 0,
    value: 0,
    index: 0,
    length: 0,
    ep0_state: 0,
    status: 0,
};

const USB_TRACE_MAGIC: u32 = 0x4655_5452; // "FUTR"
const USB_TRACE_VERSION: u32 = 1;

#[repr(C, align(4096))]
struct UsbTraceBuffer {
    magic: u32,
    version: u32,
    head: u32,
    reserved: u32,
    entries: [UsbTraceEntry; USB_TRACE_CAPACITY],
}

#[unsafe(link_section = ".usb_trace")]
static mut USB_TRACE: UsbTraceBuffer = UsbTraceBuffer {
    magic: 0,
    version: 0,
    head: 0,
    reserved: 0,
    entries: [EMPTY_USB_TRACE; USB_TRACE_CAPACITY],
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

/// Clear the linker-reserved DWC3 DMA region before enabling the controller.
///
/// The USB probe enters with caches/MMU disabled, so this is intentionally a
/// volatile byte/word clear rather than a normal Rust slice operation. The
/// normal Fullerene path also calls it after installing its identity map.
pub fn clear_dma_memory() {
    let mut current = addr_of!(__usb_dma_start) as usize;
    let end = addr_of!(__usb_dma_end) as usize;
    while current < end {
        unsafe {
            write_volatile(current as *mut u64, 0);
        }
        current += core::mem::size_of::<u64>();
    }
    trace_begin();
}

/// Initialize the retained trace header and append a boot boundary marker.
/// The entry array is intentionally not cleared, so a subsequent boot can
/// inspect the last attempt after a warm reset.
fn trace_begin() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>(), USB_TRACE_MAGIC);
            write_volatile(
                addr_of_mut!(USB_TRACE).cast::<u32>().add(1),
                USB_TRACE_VERSION,
            );
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(2), 0);
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(3), 0);
        }
    }
    trace_event(TRACE_BOOT_USB_ENTRY, 0, 0, 0, 0, 0);
}

#[inline(always)]
fn ep0_state_code(state: Ep0State) -> u32 {
    match state {
        Ep0State::Setup => 1,
        Ep0State::Data => 2,
        Ep0State::Status => 3,
    }
}

#[inline(always)]
fn trace_event(event: u32, request: u32, value: u32, index: u32, length: u32, status: u32) {
    unsafe {
        let head_ptr = addr_of_mut!(USB_TRACE).cast::<u32>().add(2);
        let head = read_volatile(head_ptr);
        let slot = (head as usize) % USB_TRACE_CAPACITY;
        let entry = UsbTraceEntry {
            sequence: head.wrapping_add(1),
            event,
            request,
            value,
            index,
            length,
            ep0_state: ep0_state_code(EP0_STATE),
            status,
        };
        write_volatile(
            addr_of_mut!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot),
            entry,
        );
        write_volatile(head_ptr, head.wrapping_add(1));
    }
}

/// Start a retained trace for the standalone handoff probe without clearing
/// the previous attempt. A subsequent normal Fullerene boot can dump it over
/// UART before starting a new trace.
pub fn trace_probe_begin() {
    trace_begin();
}

/// Add a marker without touching the controller. This is used around PMIC
/// and platform transitions where the next MMIO access itself may abort.
pub fn trace_marker(event: u32, status: u32) {
    trace_event(event, 0, 0, 0, 0, status);
}

/// Dump the post-mortem USB trace after the controller has reached a safe
/// UART-visible stage. The hot path above never calls this or formats text.
pub fn dump_trace() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            uart::puts("usb trace: no retained record\n");
            return;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        let count = (head as usize).min(USB_TRACE_CAPACITY);
        let start = (head as usize).saturating_sub(count);
        uart::puts("usb trace begin\n");
        for offset in 0..count {
            let slot = (start + offset) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            uart::put_hex("usb trace event=", entry.event as u64);
            uart::put_hex(" request=", entry.request as u64);
            uart::put_hex(" value=", entry.value as u64);
            uart::put_hex(" index=", entry.index as u64);
            uart::put_hex(" length=", entry.length as u64);
            uart::put_hex(" state=", entry.ep0_state as u64);
            uart::put_hex(" status=", entry.status as u64);
        }
        uart::puts("usb trace end\n");
    }
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
static MANUFACTURER_DESCRIPTOR: [u8; 20] = [
    20, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0,
    b'e', 0,
];
static PRODUCT_DESCRIPTOR: [u8; 36] = [
    36, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0, b' ',
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
unsafe fn smmu_reg(offset: usize) -> *mut u32 {
    (APPS_SMMU_BASE + offset) as *mut u32
}

#[inline]
unsafe fn smmu_page_reg(page_size: usize, page: usize, offset: usize) -> *mut u32 {
    (APPS_SMMU_BASE + page * page_size + offset) as *mut u32
}

#[inline]
unsafe fn smmu_page_write(page_size: usize, page: usize, offset: usize, value: u32) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset), value) };
}

#[inline]
unsafe fn smmu_page_write64(page_size: usize, page: usize, offset: usize, value: u64) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset).cast::<u64>(), value) };
}

unsafe fn smmu_tlb_sync() {
    unsafe {
        write_volatile(smmu_reg(SMMU_TLB_ALL_H), 0);
        write_volatile(smmu_reg(SMMU_TLB_SYNC), 0);
        for _ in 0..100_000u32 {
            if read_volatile(smmu_reg(SMMU_TLB_STATUS)) & SMMU_TLB_STATUS_ACTIVE == 0 {
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
}

unsafe fn install_smmu_identity_table() {
    unsafe {
        for index in 0..4usize {
            let physical = (index as u64) << 30;
            let descriptor = physical
                | SMMU_DESC_VALID
                | SMMU_DESC_AF
                | SMMU_DESC_SH_INNER
                | SMMU_DESC_ATTR_NORMAL
                | SMMU_DESC_XN;
            write_volatile(addr_of_mut!(SMMU_L1.0[index]), descriptor);
        }
        for index in 4..512usize {
            write_volatile(addr_of_mut!(SMMU_L1.0[index]), 0);
        }
        cache_clean(
            addr_of!(SMMU_L1) as usize,
            core::mem::size_of::<SmmuTable>(),
        );
    }
}

/// Install an AArch64 stage-1 identity mapping for DWC3's stream ID.
///
/// Bramble's vendor DT assigns DWC3 stream ID 0xe0 to the Apps SMMU and puts
/// USB buffers in the 0x90000000..0xf0000000 IOVA pool. Qualcomm's SMMU-500
/// firmware can reject a direct BYPASS write by turning it into FAULT, so a
/// real context-bank map is required here. We preserve the existing SMR and
/// route it to a context bank configured as S1 translation + S2 bypass.
pub fn configure_dwc3_smmu() -> bool {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            log_puts("usb: Apps SMMU identification unavailable\n");
            return false;
        }

        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let page_size = if id1 & SMMU_ID1_PAGESIZE != 0 {
            0x10000
        } else {
            0x1000
        };
        if page_size != 0x1000 {
            // The table below is intentionally 4 KiB-granule LPAE. Do not
            // enable a mismatched table on a future 64 KiB-only SMMU.
            log_puts("usb: Apps SMMU requires unsupported 64K tables\n");
            return false;
        }

        let num_pages =
            1usize << (((id1 >> SMMU_ID1_NUMPAGENDXB_SHIFT) & SMMU_ID1_NUMPAGENDXB_MASK) + 1);
        let num_s2_context_banks =
            ((id1 >> SMMU_ID1_NUMS2CB_SHIFT) & SMMU_ID1_NUMS2CB_MASK) as usize;
        let num_context_banks = (id1 & SMMU_ID1_NUMCB_MASK) as usize;
        if num_pages == 0 || num_context_banks == 0 {
            log_puts("usb: Apps SMMU has no usable context banks\n");
            return false;
        }
        // The GR0 window is page 0 and GR1 is page 1. Context-bank pages start
        // after the implementation-defined number of global pages.
        let gr1_page = 1usize;
        let cb_base_page = num_pages;
        log_hex("usb: Apps SMMU ID0=", id0 as u64);
        log_hex("usb: Apps SMMU ID1=", id1 as u64);
        log_hex("usb: Apps SMMU pages=", num_pages as u64);

        let mut matched = None;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if ((DWC3_STREAM_ID ^ id) & !mask) == 0 {
                matched = Some((index, read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4))));
                break;
            }
        }
        let Some((smr_index, old_s2cr)) = matched else {
            log_puts("usb: DWC3 stream 0xe0 has no SMMU match\n");
            return false;
        };

        let old_type = old_s2cr & SMMU_S2CR_TYPE_MASK;
        let old_cb = (old_s2cr & SMMU_S2CR_CBNDX_MASK) as usize;
        let cbndx = if old_type == SMMU_S2CR_TYPE_TRANS {
            if old_cb >= num_context_banks || old_cb < num_s2_context_banks {
                log_puts("usb: DWC3 SMMU context bank is out of range\n");
                return false;
            }
            old_cb
        } else {
            // This is the same reserved-last-context-bank strategy used by
            // Linux's qcom_smmu bypass-quirk path for firmware that refuses
            // BYPASS S2CR values.
            num_context_banks - 1
        };
        log_hex("usb: DWC3 SMMU SMR=", smr_index as u64);
        log_hex("usb: DWC3 SMMU CB=", cbndx as u64);

        install_smmu_identity_table();

        // Stop the bank before changing its format and page-table pointer.
        smmu_page_write(page_size, cb_base_page + cbndx, SMMU_CB_SCTLR, 0);
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBA2R_BASE + cbndx * 4,
            SMMU_CBA2R_VA64,
        );
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBAR_BASE + cbndx * 4,
            SMMU_CBAR_S1_TRANS_S2_BYPASS | SMMU_CBAR_S1_MEMATTR_WB | SMMU_CBAR_S1_BPSHCFG_NSH,
        );

        let cb_page = cb_base_page + cbndx;
        // 4 KiB granule, 32-bit IOVA, inner-shareable WBWA walks, and a
        // 40-bit output address size. TCR2 selects the AArch64 format.
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR2,
            SMMU_TCR2_SEP_UPSTREAM | SMMU_TCR2_AS | SMMU_TCR2_PASIZE_40BIT,
        );
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR,
            SMMU_TCR_EPD1
                | SMMU_TCR_SH0_INNER
                | SMMU_TCR_ORGN0_WBWA
                | SMMU_TCR_IRGN0_WBWA
                | SMMU_TCR_T0SZ_32BIT,
        );
        smmu_page_write64(
            page_size,
            cb_page,
            SMMU_CB_TTBR0,
            addr_of!(SMMU_L1) as usize as u64,
        );
        smmu_page_write64(page_size, cb_page, SMMU_CB_TTBR1, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_CONTEXTIDR, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR0, 0xff);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR1, 0);
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_SCTLR,
            SMMU_SCTLR_S1_ASIDPNE
                | SMMU_SCTLR_CFIE
                | SMMU_SCTLR_CFRE
                | SMMU_SCTLR_AFE
                | SMMU_SCTLR_TRE
                | SMMU_SCTLR_M,
        );

        // S2CR type TRANS is zero; preserve privilege and EXID bits from the
        // firmware entry while replacing only the context-bank selector.
        let new_s2cr = (old_s2cr & !SMMU_S2CR_CBNDX_MASK) & !SMMU_S2CR_TYPE_MASK
            | ((cbndx as u32) & SMMU_S2CR_CBNDX_MASK)
            | SMMU_S2CR_TYPE_TRANS;
        let s2cr_address = SMMU_S2CR_BASE + smr_index * 4;
        write_volatile(smmu_reg(s2cr_address), new_s2cr);
        core::arch::asm!("dsb sy", options(nostack));
        let readback = read_volatile(smmu_reg(s2cr_address));
        if readback & SMMU_S2CR_TYPE_MASK != SMMU_S2CR_TYPE_TRANS
            || (readback & SMMU_S2CR_CBNDX_MASK) as usize != cbndx
        {
            log_puts("usb: DWC3 SMMU S2CR translation rejected\n");
            return false;
        }
        smmu_tlb_sync();
        true
    }
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
    log_puts("usb: QMP PHY initialization timeout\n");
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

/// Reset the DWC3 core after taking ownership from the bootloader.
///
/// The Qualcomm glue invokes this as part of the DWC3 post-reset path. A
/// `fastboot boot` handoff skips that driver, so leaving the controller in its
/// bootloader device/host state can make endpoint commands retire without
/// ever allowing the peripheral pull-up to become visible.
unsafe fn device_soft_reset() -> bool {
    unsafe {
        trace_event(TRACE_DWC3_RESET_BEGIN, 0, 0, 0, 0, 0);
        trace_event(TRACE_DEVICE_RESET, 0, 0, 0, 0, 0);
        let initial_dctl = read(DCTL);
        // Match Linux's reconnect path: clear stale endpoint/device state
        // without touching the already-running Qualcomm PHY and clock
        // branches. RUN_STOP must be cleared in the same write; preserving
        // Fastboot's RUN_STOP bit can leave the device half-running while
        // CSFTRST is asserted.
        let mut dctl = initial_dctl;
        dctl |= DCTL_CSFTRST;
        dctl &= !DCTL_RUN_STOP;
        write(DCTL, dctl);
        let mut device_reset_complete = false;
        for _ in 0..1_000_000u32 {
            if read(DCTL) & DCTL_CSFTRST == 0 {
                device_reset_complete = true;
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        if !device_reset_complete {
            log_puts("usb: DWC3 device reset timeout\n");
            return false;
        }

        // Newer DWC3 revisions keep the device controller unavailable for a
        // short synchronization window after CSFTRST. Endpoint commands
        // issued before DCNRD clears are rejected even though CSFTRST has
        // already self-cleared.
        for _ in 0..1_000_000u32 {
            if read(DSTS) & DSTS_DCNRD == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        log_puts("usb: DWC3 controller-not-ready timeout\n");
        false
    }
}

/// Reset the DWC3 core and both PHY-facing domains for a cold platform start.
///
/// This is intentionally separate from `device_soft_reset`: a Fastboot
/// handoff must not reset the PHYs that own the Type-C session.
unsafe fn core_soft_reset(super_speed: bool) -> bool {
    unsafe {
        if !device_soft_reset() {
            return false;
        }

        let mut gctl = read(GCTL);
        gctl |= GCTL_CORESOFTRESET;
        write(GCTL, gctl);

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 |= GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }

        // The upstream DWC3 core reset uses a 100 ms delay after releasing
        // both PHY resets. Keep a busy-wait because this early probe has not
        // initialized the generic timer yet.
        for _ in 0..100_000_000u32 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }

        usb2 = read(GUSB2PHYCFG0) & !GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 &= !GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }
        for _ in 0..1_000_000u32 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }

        gctl = read(GCTL) & !GCTL_CORESOFTRESET;
        write(GCTL, gctl);
        true
    }
}

/// Stop a controller that was left running by Fastboot before reusing its
/// device-mode endpoint state. A DWC3 gadget must be halted before
/// DEPSTARTCFG/SETEPCONFIG are issued; a handoff cannot assume that the
/// bootloader performed the normal gadget-stop sequence.
unsafe fn stop_running_device() -> bool {
    unsafe {
        let dctl = read(DCTL);
        if dctl & DCTL_RUN_STOP == 0 {
            return true;
        }
        write(DCTL, dctl & !DCTL_RUN_STOP);
        for _ in 0..1_000_000u32 {
            if read(DSTS) & DSTS_DEVCTRLHLT != 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    log_puts("usb: DWC3 stop timeout during handoff\n");
    false
}

#[inline]
fn run_stop_value(mut dctl: u32, snpsid: u32) -> u32 {
    dctl &= !DCTL_TRGTULST_MASK;
    if (snpsid & 0xffff_0000) == 0x5533_0000 && snpsid <= DWC3_REVISION_187A {
        dctl |= DCTL_TRGTULST_RX_DET;
    }
    dctl | DCTL_RUN_STOP
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
    trace_event(TRACE_QSCRATCH_BEGIN, offset as u32, mask, 0, 0, 0);
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
    // DWC3 and the Apps SMMU consume these objects by DMA.  The probe may be
    // entered with the bootloader's caches enabled, so a no-op here would
    // leave the freshly written TRB/page table only in the CPU cache.
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    {
        // The standalone handoff enters with an unknown cache/MMU regime.
        // Do not turn cache maintenance itself into an exception before the
        // physical pull-up; the normal kernel path uses the real operation.
        let _ = (address, length);
        return;
    }
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
    trace_event(
        TRACE_EP_COMMAND_ISSUE,
        command,
        endpoint as u32,
        param0,
        param1,
        param2,
    );
    unsafe {
        write(dep_reg(endpoint, 0x00), param2);
        write(dep_reg(endpoint, 0x04), param1);
        write(dep_reg(endpoint, 0x08), param0);
        write(dep_reg(endpoint, 0x0c), command | DEPCMD_CMDACT);
    }
    for _ in 0..100_000 {
        let status = unsafe { read(dep_reg(endpoint, 0x0c)) };
        if status & DEPCMD_CMDACT == 0 {
            trace_event(
                TRACE_EP_COMMAND_DONE,
                command,
                endpoint as u32,
                status,
                0,
                unsafe { read(DSTS) },
            );
            return status & 0xf000 == 0;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    trace_event(
        TRACE_EP_COMMAND_TIMEOUT,
        command,
        endpoint as u32,
        unsafe { read(dep_reg(endpoint, 0x0c)) },
        0,
        unsafe { read(DSTS) },
    );
    log_puts("usb: DWC3 endpoint command timeout\n");
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
    // Linux allocates a transfer resource immediately after configuring each
    // endpoint.  DEPSTARTCFG only resets the allocation window; issuing
    // SETTRANSFRESOURCE for every possible endpoint is not equivalent and can
    // make the handoff fail before the first pull-up.
    if !modify {
        return unsafe { send_ep_command(endpoint, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0) };
    }
    true
}

unsafe fn start_transfer(endpoint: usize, trb: *const Trb) -> bool {
    let address = trb as usize as u64;
    unsafe {
        // DWC3's STARTTRANSFER parameters are PAR0=address[63:32] and
        // PAR1=address[31:0]. The endpoint command helper writes the named
        // param0/param1 fields to those registers respectively.
        send_ep_command(
            endpoint,
            DEPCMD_STARTTRANSFER,
            (address >> 32) as u32,
            address as u32,
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

unsafe fn start_setup() -> bool {
    trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, unsafe { read(DSTS) });
    unsafe {
        prepare_trb(0, addr_of!(SETUP_PACKET).cast::<u8>(), 8, TRB_CONTROL_SETUP);
        start_transfer(0, addr_of!(EP0_TRBS).cast::<Trb>())
    }
}

/// Re-arm the control endpoint after the host has issued a USB bus reset.
///
/// A bus reset terminates the setup transfer which was queued before the
/// host began enumeration, but it does not perform a DWC3 core reset.  The
/// transfer resources and endpoint configuration therefore remain usable.
/// Keeping DALEPENA cleared here leaves the device with a pull-up and no EP0,
/// which is indistinguishable from a dead gadget to the host.
unsafe fn restart_control_after_reset() {
    unsafe {
        CONFIGURED = false;
        EP0_STATE = Ep0State::Setup;
        CONTROL_IN = false;
        CONTROL_HAS_DATA = false;

        let mut dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
        let speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
        let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
        dcfg &= !DCFG_SPEED_MASK;
        dcfg |= if speed == DSTS_SUPERSPEED {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);

        // USB reset ends the active EP0 transfer, but the endpoint remains
        // configured on the non-core-reset path.  Reconfigure defensively
        // if a preceding Connect Done event did not get processed.
        if !ENDPOINTS_READY {
            ENDPOINTS_READY = configure_endpoint(0, max_packet, false)
                && configure_endpoint(1, max_packet, false);
        }
        if ENDPOINTS_READY {
            write(DALEPENA, 0b11);
            start_setup();
        }
    }
}

unsafe fn start_status(endpoint: usize) {
    let kind = if unsafe { CONTROL_HAS_DATA } {
        TRB_CONTROL_STATUS3
    } else {
        TRB_CONTROL_STATUS2
    };
    trace_event(TRACE_STATUS_QUEUED, 0, endpoint as u32, kind, 0, unsafe {
        read(DSTS)
    });
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
    trace_event(
        TRACE_SETUP_RECEIVED,
        request as u32,
        value as u32,
        index as u32,
        requested_length as u32,
        unsafe { read(DSTS) },
    );
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
                trace_event(
                    TRACE_DESCRIPTOR_QUEUED,
                    request as u32,
                    value as u32,
                    index as u32,
                    length as u32,
                    unsafe { read(DSTS) },
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
        unsafe {
            EP0_STATE = Ep0State::Status;
            // SET_ADDRESS is an OUT request with no data stage, so the host
            // completes it with a zero-length IN status packet. Queue it
            // explicitly; relying on a later XferNotReady event leaves some
            // DWC3 revisions waiting forever during enumeration.
            start_status(1);
        }
        return;
    }

    if request_type == 0 && request == 9 {
        unsafe { CONFIGURED = value != 0 };
        unsafe {
            EP0_STATE = Ep0State::Status;
            // SET_CONFIGURATION has the same no-data OUT control shape as
            // SET_ADDRESS and must be completed before the next SETUP.
            start_status(1);
        }
        log_puts(if value != 0 {
            "usb: Fullerene configured\n"
        } else {
            "usb: Fullerene deconfigured\n"
        });
        return;
    }

    // Unsupported requests are intentionally left without a transfer. The
    // host will recover with the next bus reset; this keeps the first gadget
    // small while making an accidental ADB claim impossible.
    log_puts("usb: unsupported control request\n");
    unsafe { EP0_STATE = Ep0State::Setup };
    unsafe { start_setup() };
    let _ = index;
}

unsafe fn process_event(raw: u32) {
    let endpoint_event = (raw & 1) == 0;
    if !endpoint_event {
        // DWC3's device event layout is: one_bit[0], device_event[1:7],
        // type[8:11].  The device_event field is zero for ordinary device
        // events; type carries Disconnect, USB Reset, and Connect Done.
        let device_event = (raw >> DEVICE_EVENT_KIND_SHIFT) & DEVICE_EVENT_KIND_MASK;
        match device_event {
            0 => {}
            1 => {
                trace_event(TRACE_USB_RESET, 0, 0, 0, 0, raw);
                unsafe { restart_control_after_reset() }
            }
            2 => {
                trace_event(TRACE_DEVICE_CONNECT, 0, 0, 0, 0, raw);
                let speed = unsafe { read(DSTS) & DSTS_CONNECTSPD_MASK };
                log_puts("usb: connect done, speed=");
                log_hex_value(speed as u64);
                // Linux's DWC3 gadget driver starts with the SuperSpeed EP0
                // size and modifies it after Connect Done.
                let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                unsafe {
                    let first_connect = !ENDPOINTS_READY;
                    let endpoints_ready = if first_connect {
                        configure_endpoint(0, max_packet, false)
                            && configure_endpoint(1, max_packet, false)
                    } else {
                        configure_endpoint(0, max_packet, true)
                            && configure_endpoint(1, max_packet, true)
                    };
                    if endpoints_ready {
                        ENDPOINTS_READY = true;
                        write(DALEPENA, 0b11);
                        start_setup();
                    }
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
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            event,
            endpoint as u32,
            status,
            0,
            raw,
        );
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
    init_with_super_speed(true, true, true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    init_with_super_speed(false, true, true)
}

/// Take over the USB controller without resetting the PHY or clock branches.
/// Fastboot has already completed that hardware bring-up; resetting those
/// blocks during a `fastboot boot` handoff can remove the Type-C pull-up before
/// the new gadget has a chance to enumerate.
pub fn init_usb2_handoff() -> bool {
    // Prefer the handoff sequence that preserves the bootloader-owned PHY and
    // brings EP0 up immediately after the physical USB2 reconnect.  This is
    // also the path used by the Bramble probe, and unlike the cold fallback it
    // does not rewrite the vendor-owned Apps SMMU context before the first
    // descriptor transfer.
    if init_usb2_gadget_handoff() {
        return true;
    }

    // If the bootloader did not leave a usable peripheral session, recover by
    // performing the more invasive DWC3 device reset and SMMU setup.
    init_with_super_speed(false, true, false)
}

/// Connect only the physical USB2 pull-up during a Fastboot handoff.
///
/// This diagnostic intentionally does not touch the event ring, endpoint
/// commands, or SMMU. It answers the narrower hardware question first: can
/// the Qualcomm PHY and DWC3 device controller make the port visible after
/// the bootloader disconnects? A host may report an incomplete USB device
/// because EP0 is not configured; that is expected for this probe.
pub fn init_usb2_pullup_handoff() -> bool {
    unsafe {
        log_hex("usb pullup: DWC3 GSNPSID=", read(GSNPSID) as u64);

        // Qualcomm's glue asserts LANE0_PWR_PRESENT together with the HS
        // VBUS/session override when entering peripheral mode. This is also
        // required on the USB2-only handoff path; it is not gated on QMP PHY
        // calibration in the Linux role-switch path.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);

        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);

        if !device_soft_reset() {
            log_puts("usb pullup: DWC3 device reset failed\n");
            return false;
        }

        qscratch_set(
            QSCRATCH_SS_PHY_CTRL,
            1 << 24, // LANE0_PWR_PRESENT
        );
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        // Fastboot handoff skips the cold-start clock setup above, but a
        // USB2-only device still needs Qualcomm's UTMI-as-PIPE selection.
        // Linux performs this during the DWC3 post-reset callback.
        select_utmi_pipe_clock();

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        write(DCFG, DCFG_HIGHSPEED);
        write(DALEPENA, 0b11);
        // Fastboot leaves the USB2 link in its old negotiated state. Apply
        // the upstream RxDetect workaround only when GSNPSID identifies a
        // DWC3 revision for which that workaround is specified.
        let dctl = run_stop_value(read(DCTL), read(GSNPSID));
        // Keep the Qualcomm glue's VBUS/session override adjacent to the
        // connect transition, matching dwc3_qcom_run_stop_notifier().
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        write(DCTL, dctl);
        for _ in 0..1_000_000u32 {
            if read(DSTS) & DSTS_DEVCTRLHLT == 0 {
                log_puts("usb pullup: DWC3 RUN/STOP active\n");
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        log_hex("usb pullup: DWC3 remained halted, DSTS=", read(DSTS) as u64);
        false
    }
}

/// Perform only the writes needed to request a USB2 device pull-up.
///
/// This is intentionally a last-resort diagnostic. It avoids UART, DWC3
/// reset, event rings, endpoint commands, and SMMU access. The QSCRATCH VBUS
/// writes still use the Qualcomm glue's read-modify-write/readback sequence;
/// that ordering is part of the physical connect contract. If this does not
/// make the phone visible on the host, the failure is below the normal gadget
/// path: entry/exception handling, the Qualcomm USB glue, the PHY/session
/// state, or the bootloader's USB handoff itself.
unsafe fn init_usb2_bare_pullup_handoff_inner(connect: bool) -> bool {
    unsafe {
        // Match dwc3_qcom_vbus_override_enable(): the Qualcomm glue asserts
        // both the SuperSpeed lane power-present vote and the USB2
        // VBUS/session override, even when the gadget is intentionally
        // limited to USB2.  Writing only HS_PHY_CTRL leaves the role change
        // incomplete on platforms whose Type-C glue gates the pull-up with
        // the SS-side vote.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // Fastboot may leave the core in the USB2 suspended state when it
        // tears down its gadget just before jumping to the temporary image.
        // Waking the UTMI block is still below the EP0/DMA boundary and is
        // required before DCTL.Run/Stop can produce a new pull-up.
        let mut usb2 = read_volatile(reg(GUSB2PHYCFG0));
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write_volatile(reg(GUSB2PHYCFG0), usb2);
        let mut usb3 = read_volatile(reg(GUSB3PIPECTL0));
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write_volatile(reg(GUSB3PIPECTL0), usb3);
        let general = read_volatile(qscratch_reg(QSCRATCH_GENERAL_CFG));
        write_volatile(
            qscratch_reg(QSCRATCH_GENERAL_CFG),
            general | QSCRATCH_GENERAL_CFG_XHCI_REV,
        );
        // The bare path intentionally skips DWC3 reset, but it still needs
        // the Qualcomm glue's UTMI-as-PIPE clock selection when the Fastboot
        // session did not leave that mux configured for the temporary image.
        select_utmi_pipe_clock();

        let gctl = read_volatile(reg(GCTL));
        write_volatile(
            reg(GCTL),
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );
        write_volatile(reg(DCFG), DCFG_HIGHSPEED);
        write_volatile(reg(DALEPENA), 0b11);

        let dctl = run_stop_value(read_volatile(reg(DCTL)), read_volatile(reg(GSNPSID)));
        // Qualcomm's glue reasserts the VBUS override immediately before
        // enabling RUN_STOP so a stale Fastboot session cannot suppress the
        // connect-done transition.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // A gadget handoff uses the same proven PHY/session preparation but
        // keeps Run/Stop clear until its event ring and EP0 commands are
        // ready. The standalone bare probe requests the pull-up immediately.
        let dctl = if connect {
            dctl & !DCTL_CSFTRST
        } else {
            dctl & !(DCTL_RUN_STOP | DCTL_CSFTRST)
        };
        write_volatile(reg(DCTL), dctl);
    }
    true
}

pub fn init_usb2_bare_pullup_handoff() -> bool {
    unsafe { init_usb2_bare_pullup_handoff_inner(true) }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn init_usb2_gadget_reuse_fastboot_ep0() -> bool {
    // Use the known-good physical handoff so the Qualcomm PHY/session state
    // is re-established.  Do not halt the controller here: on Bramble that
    // tears down the Fastboot-owned Type-C session and prevents the second
    // USB2 attach entirely.
    if !unsafe { init_usb2_bare_pullup_handoff_inner(true) } {
        return false;
    }

    // The Fastboot session may have left the DWC3 stream behind an SMMU
    // mapping that only covers its own buffers.  Our TRBs/event ring are
    // intentionally identity-addressed in the 0x9b800000 DMA section.  Keep
    // the proven PHY/pull-up transition first, then install the identity map
    // before handing any new DMA object to DWC3.
    let _ = configure_dwc3_smmu();

    let event_address = addr_of!(EVENTS) as usize as u64;
    unsafe {
        // Reusing the bootloader's DMA context must not expose stale event
        // words from the previous Fastboot session to the polled consumer.
        let event_words = addr_of_mut!(EVENTS).cast::<u32>();
        for index in 0..(EVENT_BUFFER_SIZE / core::mem::size_of::<u32>()) {
            write_volatile(event_words.add(index), 0);
        }
        cache_clean(addr_of!(EVENTS) as usize, EVENT_BUFFER_SIZE);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, EVENT_BUFFER_SIZE as u32);
        write(GEVNTCOUNT0, 0);
        EVENT_OFFSET = 0;
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        // Fastboot leaves the control endpoints configured.  Do not issue
        // DEPSTARTCFG/SETEPCONFIG while Run/Stop is still active; that command
        // sequence requires a halted controller and was sending the probe to
        // its bare-pullup fallback.
        ENDPOINTS_READY = true;
        write(DCFG, DCFG_HIGHSPEED);
        write(DALEPENA, 0b11);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE,
        );
        // Fastboot can leave a control transfer active on either EP0
        // direction.  End that transfer in-place while keeping RUN/STOP and
        // the Qualcomm session alive.  Resource index 1 is the EP0 resource
        // used by the DWC3 gadget path; an already-idle endpoint simply
        // reports a command error, which is harmless here.
        let endtransfer = DEPCMD_ENDTRANSFER
            | DEPCMD_HIPRI_FORCERM
            | (1 << DEPCMD_PARAM_SHIFT);
        let _ = send_ep_command(0, endtransfer, 0, 0, 0);
        let _ = send_ep_command(1, endtransfer, 0, 0, 0);
        // Fastboot may have configured these control endpoints for the
        // SuperSpeed session it just ended.  Linux modifies both directions
        // to the USB2 EP0 maximum packet size after Connect Done; retaining
        // 512 bytes while DCFG advertises High-Speed can leave the first
        // 8-byte SETUP transfer unserviceable.
        if !configure_endpoint(0, 64, true) || !configure_endpoint(1, 64, true) {
            log_puts("usb gadget handoff: USB2 EP0 modify failed\n");
            return false;
        }
        // The endpoint configuration may survive the bootloader handoff
        // while its resource allocation does not.  Re-establish one resource
        // for each physical EP0 direction before queueing SETUP.
        let _ = send_ep_command(0, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0);
        let _ = send_ep_command(1, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0);
        if !start_setup() {
            log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
            return false;
        }

        let dctl = run_stop_value(read(DCTL), read(GSNPSID));
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28),
        );
        write(DCTL, dctl);
        for _ in 0..1_000_000u32 {
            if read(DSTS) & DSTS_DEVCTRLHLT == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        log_puts("usb gadget handoff: DWC3 RUN/STOP timeout\n");
    }
    false
}

/// Reuse the physical USB2 handoff, then add the minimum DWC3 gadget state
/// needed to answer USB control transfers. The PHY and Qualcomm session
/// remain untouched; this is the early Bramble handoff path and is also
/// usable as a standalone probe.
pub fn init_usb2_gadget_handoff() -> bool {
    unsafe {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        return init_usb2_gadget_reuse_fastboot_ep0();

        // The bare probe is the proven physical baseline on Bramble. Start
        // the gadget diagnostic from that exact pull-up sequence, then add
        // EP0 state on top of it. This makes a failure in the gadget setup
        // observable instead of hiding the already-working link behind a
        // second, subtly different pre-connect sequence.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !init_usb2_bare_pullup_handoff_inner(true) {
            return false;
        }
        trace_event(TRACE_INIT, 0, 0, 0, 0, 0);
        let snpsid = read(GSNPSID);
        trace_event(TRACE_INIT, 0, 0, 0, 0, snpsid);
        // Keep the Qualcomm session valid while the DWC3 device state is
        // rebuilt. The physical handoff above is deliberately first so the
        // probe preserves the working Bramble reconnect contract; the soft
        // reset below then clears the old Fastboot endpoint state before the
        // complete gadget is connected again.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        let gctl = read(GCTL);
        write(
            GCTL,
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );

        // Fastboot leaves the DWC3 device controller running while its host
        // endpoint is torn down. After the proven PHY/session preparation,
        // follow Linux's soft-connect order and reset the device state before
        // issuing endpoint commands. The gadget probe intentionally omits
        // stop_running_device(): the bare preparation already cleared
        // Run/Stop and that extra ownership transition was the earlier
        // pre-pull-up failure point.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            return false;
        }
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
        if !stop_running_device() || !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 reset failed\n");
            return false;
        }

        // The bootloader can leave the USB2 core in suspend/LPM state even
        // though the Type-C session is valid. Reapply only the
        // controller-side wakeup bits; do not reset the PHY or clocks.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        // Deliberately leave the Apps SMMU untouched in this stage probe.
        // The purpose here is to isolate DWC3 endpoint/event handling from
        // the vendor-owned DMA context.  The production path configures the
        // SMMU below; this probe must still reach the physical pull-up if
        // that context is not writable during a fastboot handoff.

        // The linker-reserved region is identity-mapped by the early AArch64
        // MMU path. Clean it for the same handoff ordering whether this entry
        // is reached from the standalone probe or from the normal kernel.
        let event_address = addr_of!(EVENTS) as usize as u64;
        cache_clean(addr_of!(EVENTS) as usize, EVENT_BUFFER_SIZE);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, EVENT_BUFFER_SIZE as u32);
        write(GEVNTCOUNT0, 0);
        EVENT_OFFSET = 0;
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        ENDPOINTS_READY = false;

        write(DCFG, DCFG_HIGHSPEED);
        write(DALEPENA, 0);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE,
        );

        // DWC3's device-start contract is: reserve the endpoint resources,
        // configure both directions of EP0, queue the first SETUP TRB, then
        // assert Run/Stop. Without this sequence the PHY can advertise a
        // USB2 pull-up while every host descriptor request times out at EP0.
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
            || !configure_endpoint(0, 64, false)
            || !configure_endpoint(1, 64, false)
        {
            log_puts("usb gadget handoff: EP0 configuration failed\n");
            return false;
        }
        ENDPOINTS_READY = true;
        write(DALEPENA, 0b11);
        start_setup();

        // Connect only after the event ring, transfer resources, EP0
        // descriptors, and first SETUP TRB are ready. This produces a fresh
        // USB2 attach without exposing an EP0-less device to the host.
        let dctl = run_stop_value(read(DCTL), read(GSNPSID));
        // Reassert the Qualcomm VBUS/session vote immediately before the
        // final Run/Stop write; this is the glue driver's pre_run_stop hook.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        write(DCTL, dctl);
        for _ in 0..1_000_000u32 {
            if read(DSTS) & DSTS_DEVCTRLHLT == 0 {
                log_puts("usb gadget handoff: DWC3 RUN/STOP active\n");
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        if read(DSTS) & DSTS_DEVCTRLHLT != 0 {
            log_puts("usb gadget handoff: DWC3 RUN/STOP timeout\n");
            return false;
        }

        log_puts("usb gadget handoff: EP0 running\n");
        true
    }
}

fn init_with_super_speed(super_speed: bool, reset_core: bool, reset_platform: bool) -> bool {
    unsafe {
        if reset_platform && !super::platform::bramble::enable_usb30_gdsc() {
            // Fastboot itself proves that the USB power domain was usable
            // immediately before the handoff.  Some Pixel bootloaders keep
            // the GDSC under secure/RPMh ownership, so a direct software
            // poll can time out even though the existing rail vote is still
            // valid.  Treat this as a non-fatal ownership warning and keep
            // the bootloader's vote rather than abandoning the controller.
            uart::puts("usb: USB3 GDSC PWR_ON not observable; preserving vote\n");
        }
        let snpsid = read(GSNPSID);
        uart::put_hex("usb: DWC3 GSNPSID=", snpsid as u64);

        // The Linux lito-usb device tree supplies these clocks and resets to
        // the Qualcomm glue.  A RAM-booted Fullerene image has no clock
        // framework yet, so perform the small branch/reset part directly.
        let qmp_ready = if reset_platform {
            if !super::platform::bramble::configure_usb_clocks() {
                // The same applies to GCC RCG command-update bits: they may
                // be secure-owned after the vendor fastboot handoff.  The
                // active Fastboot link already selected usable rates, so
                // continue with those rates instead of treating ownership as
                // a hardware failure.
                uart::puts("usb: GCC USB RCG update not observable; preserving rates\n");
            }
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
            if super_speed {
                gcc_reset(GCC_USB3_DP_PHY_PRIM_BCR);
                gcc_reset(GCC_USB3_PHY_PRIM_BCR);
                init_qmp_phy()
            } else {
                false
            }
        } else {
            false
        };
        // Match the QCOM DWC3 glue's peripheral-mode VBUS override.  The
        // bootloader's fastboot role is not a complete kernel-side OTG
        // session, so relying on the core alone leaves the device halted.
        if qmp_ready || !reset_platform {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // The legacy Qualcomm DWC3 glue enables the master clocks for the
        // controller RAMs here. Without these votes, DWC3 clock gating can
        // shut the RAM interface off even though the core and PHY clocks are
        // running, leaving the event ring and endpoint commands invisible.
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // Lito/SM7250 supplies the DWC3 reference clock at 19.2 MHz.  A cold
        // platform start must program the values used by the Qualcomm glue.
        // During a Fastboot handoff, however, the bootloader has already
        // selected and calibrated this clock; preserving it avoids changing
        // the clock domain underneath a still-running controller.
        if reset_platform {
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
        }

        // Select peripheral mode before issuing the device soft reset. The
        // DCTL.CSFTRST handshake is only defined while the core is in device
        // capability mode; fastboot may have left the port in host/OTG mode.
        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);

        if !reset_core && !stop_running_device() {
            return false;
        }

        if reset_core {
            let reset_ok = if reset_platform {
                core_soft_reset(qmp_ready)
            } else {
                device_soft_reset()
            };
            if !reset_ok {
                uart::puts("usb: DWC3 reset failed\n");
                return false;
            }
        }

        // Fastboot leaves the USB2 PHY powered, but the DWC3 handoff can
        // clear the PHY's session-valid state while stopping the old gadget.
        // Reapply the non-destructive Femto PHY programming on the USB2
        // handoff path; this does not assert the GCC PHY reset or touch the
        // Type-C power domain.
        if !super_speed && !reset_platform {
            init_hsphy();
        }

        // Core reset restores the QSCRATCH-facing state on some DWC3
        // revisions, so re-apply the Qualcomm glue votes after reset.
        if qmp_ready || !reset_platform {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // SM7250's DWC3 revision is older than 2.50a. The Qualcomm glue
        // advertises the XHCI 1.0 register layout through this QSCRATCH bit
        // during its reset callback.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);

        // USB2-only cold starts need the same post-reset UTMI clock selection
        // as the Qualcomm glue. Bramble has a QMP PIPE clock and does not set
        // the DT's select-utmi-as-pipe-clk property, so a Fastboot handoff
        // must preserve the already-running PIPE clock instead of switching
        // it underneath the live Type-C session.
        if !super_speed && reset_platform {
            select_utmi_pipe_clock();
        }

        if configure_dwc3_smmu() {
            uart::puts("usb: DWC3 SMMU identity map ready\n");
        } else {
            uart::puts("usb: DWC3 SMMU identity map unavailable\n");
        }

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        // Match dwc3_dis_sleep_mode(): the host-side L1 threshold helper is
        // independent of the USB2 PHY sleep bit and can survive a Fastboot
        // handoff with a stale value.
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
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
        ENDPOINTS_READY = true;
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
