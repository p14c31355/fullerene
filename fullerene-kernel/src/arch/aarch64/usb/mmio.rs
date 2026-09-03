//! Stable USB MMIO base addresses, register offsets, and low-level accessors.

use core::ptr::{read_volatile, write_volatile};

#[inline]
pub(super) fn dwc3_base() -> usize {
    super::super::platform::bramble::usb_resources().dwc3_base
}
// Lito/SM7250's Apps SMMU owns the DWC3 stream ID declared by the board DT.
// The early Bramble path installs a small identity map in a context bank so
// the USB buffers remain inside the IOVA pool declared by the vendor DT.
// Google’s Bramble/Lito DTS places apps-smmu at 0x15000000.  The nearby
// 0x0c600000 range is the SPMI arbiter channel window, not the Apps SMMU;
// confusing the two makes the SMMU identity-map setup target unrelated PMIC
// registers before the first EP0 transfer.
pub(crate) const SMMU_ID0: usize = 0x20;
pub(crate) const SMMU_ID1: usize = 0x24;
pub(crate) const SMMU_ID0_NUMSMRG_MASK: u32 = 0xff;
pub(crate) const SMMU_ID1_PAGESIZE: u32 = 1 << 31;
pub(crate) const SMMU_ID1_NUMPAGENDXB_SHIFT: u32 = 28;
pub(crate) const SMMU_ID1_NUMPAGENDXB_MASK: u32 = 0x7;
pub(crate) const SMMU_ID1_NUMS2CB_SHIFT: u32 = 16;
pub(crate) const SMMU_ID1_NUMS2CB_MASK: u32 = 0xff;
pub(crate) const SMMU_ID1_NUMCB_MASK: u32 = 0xff;
pub(crate) const SMMU_SMR_BASE: usize = 0x800;
pub(crate) const SMMU_S2CR_BASE: usize = 0xc00;
pub(crate) const SMMU_TLB_ALL_H: usize = 0x6c;
pub(crate) const SMMU_TLB_SYNC: usize = 0x70;
pub(crate) const SMMU_TLB_STATUS: usize = 0x74;
pub(crate) const SMMU_TLB_STATUS_ACTIVE: u32 = 1;
pub(crate) const SMMU_SMR_VALID: u32 = 1 << 31;
pub(crate) const SMMU_SMR_MASK_SHIFT: u32 = 16;
pub(crate) const SMMU_S2CR_TYPE_MASK: u32 = 0x3 << 16;
pub(crate) const SMMU_S2CR_TYPE_TRANS: u32 = 0;
pub(crate) const SMMU_S2CR_TYPE_BYPASS: u32 = 1 << 16;
pub(crate) const SMMU_S2CR_CBNDX_MASK: u32 = 0xff;
pub(crate) const SMMU_GR1_CBAR_BASE: usize = 0x00;
pub(crate) const SMMU_GR1_CBA2R_BASE: usize = 0x800;
pub(crate) const SMMU_CBA2R_VA64: u32 = 1;
pub(crate) const SMMU_CBAR_IRPTNDX_MASK: u32 = 0xff;
pub(crate) const SMMU_CBAR_S1_TRANS_S2_BYPASS: u32 = 1 << 16;
pub(crate) const SMMU_CBAR_S1_MEMATTR_WB: u32 = 0xf << 12;
pub(crate) const SMMU_CBAR_S1_BPSHCFG_NSH: u32 = 3 << 8;
pub(crate) const SMMU_CB_SCTLR: usize = 0x00;
pub(crate) const SMMU_CB_TCR2: usize = 0x10;
pub(crate) const SMMU_CB_TTBR0: usize = 0x20;
pub(crate) const SMMU_CB_TTBR1: usize = 0x28;
pub(crate) const SMMU_CB_TCR: usize = 0x30;
pub(crate) const SMMU_CB_CONTEXTIDR: usize = 0x34;
pub(crate) const SMMU_CB_MAIR0: usize = 0x38;
pub(crate) const SMMU_CB_MAIR1: usize = 0x3c;
pub(crate) const SMMU_CB_RESUME: usize = 0x08;
pub(crate) const SMMU_CB_FSR: usize = 0x58;
pub(crate) const SMMU_CB_FAR: usize = 0x60;
pub(crate) const SMMU_CB_FSYNR0: usize = 0x68;
pub(crate) const SMMU_GR0_FSR: usize = 0x48;
pub(crate) const SMMU_GR0_FSYNR0: usize = 0x50;
pub(crate) const SMMU_RESUME_TERMINATE: u32 = 1;
pub(crate) const SMMU_GLOBAL_FSR_FAULT: u32 = 1 << 1;
pub(crate) const SMMU_FSR_SS: u32 = 1 << 30;
pub(crate) const SMMU_FSR_FAULT: u32 = (1 << 31)
    | (1 << 30)
    | (1 << 8)
    | (1 << 7)
    | (1 << 6)
    | (1 << 5)
    | (1 << 4)
    | (1 << 3)
    | (1 << 2)
    | (1 << 1);
pub(crate) const SMMU_SCTLR_S1_ASIDPNE: u32 = 1 << 12;
pub(crate) const SMMU_SCTLR_CFIE: u32 = 1 << 6;
pub(crate) const SMMU_SCTLR_CFRE: u32 = 1 << 5;
pub(crate) const SMMU_SCTLR_AFE: u32 = 1 << 2;
pub(crate) const SMMU_SCTLR_TRE: u32 = 1 << 1;
pub(crate) const SMMU_SCTLR_M: u32 = 1;
pub(crate) const SMMU_GR0_SCR0: usize = 0x00;
pub(crate) const SMMU_SCR0_GFRE: u32 = 1 << 1;
pub(crate) const SMMU_SCR0_GFIE: u32 = 1 << 2;
pub(crate) const SMMU_SCR0_GCFGFRE: u32 = 1 << 4;
pub(crate) const SMMU_SCR0_GCFGFIE: u32 = 1 << 5;
pub(crate) const SMMU_TCR_EPD1: u32 = 1 << 23;
pub(crate) const SMMU_TCR_SH0_INNER: u32 = 3 << 12;
pub(crate) const SMMU_TCR_ORGN0_WBWA: u32 = 1 << 10;
pub(crate) const SMMU_TCR_IRGN0_WBWA: u32 = 1 << 8;
pub(crate) const SMMU_TCR_T0SZ_32BIT: u32 = 32;
pub(crate) const SMMU_TCR_T0SZ_39BIT: u32 = 25;
pub(crate) const SMMU_TCR2_SEP_UPSTREAM: u32 = 0x7 << 15;
pub(crate) const SMMU_TCR2_AS: u32 = 1 << 4;
pub(crate) const SMMU_TCR2_PASIZE_40BIT: u32 = 2;

pub(crate) const SMMU_DESC_VALID: u64 = 1;
pub(crate) const SMMU_DESC_TABLE: u64 = 3;
pub(crate) const SMMU_DESC_BLOCK: u64 = 1;
pub(crate) const SMMU_DESC_TYPE_MASK: u64 = 3;
pub(crate) const SMMU_DESC_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
pub(crate) const SMMU_DESC_AF: u64 = 1 << 10;
pub(crate) const SMMU_DESC_SH_INNER: u64 = 3 << 8;
pub(crate) const SMMU_DESC_ATTR_NORMAL: u64 = 0;
pub(crate) const SMMU_DESC_XN: u64 = (1 << 53) | (1 << 54);
#[inline]
pub(super) fn apps_smmu_base() -> usize {
    super::super::platform::bramble::usb_resources().apps_smmu_base
}

#[inline]
pub(super) fn hsphy_base() -> usize {
    super::super::platform::bramble::usb_resources().hs_phy_base
}

#[inline]
pub(super) fn qmp_base() -> usize {
    super::super::platform::bramble::usb_resources().qmp_phy_base
}
// SM7250 exposes the Qualcomm glue/QSCRATCH block immediately above the
// DWC3 core.  The glue must report the cable's VBUS/session to the core when
// we take over directly from the bootloader.
#[inline]
pub(super) fn qscratch_base() -> usize {
    super::super::platform::bramble::usb_resources().qscratch_base
}
pub(crate) const QSCRATCH_HS_PHY_CTRL: usize = 0x10;
pub(crate) const QSCRATCH_CGCTL: usize = 0x28;
pub(crate) const QSCRATCH_SS_PHY_CTRL: usize = 0x30;
pub(crate) const QSCRATCH_GENERAL_CFG: usize = 0x08;
pub(crate) const QSCRATCH_GENERAL_CFG_XHCI_REV: u32 = 1 << 2;
// Qualcomm glue power-event status/mask registers. These are consumed by
// dwc3-msm's threaded power IRQ, not by the DWC3 event ring.
pub(crate) const QSCRATCH_PWR_EVENT_STATUS: usize = 0x58;
pub(crate) const QSCRATCH_PWR_EVENT_MASK: usize = 0x5c;
pub(crate) const PWR_EVENT_POWERDOWN_IN_P3: u32 = 1 << 2;
pub(crate) const PWR_EVENT_POWERDOWN_OUT_P3: u32 = 1 << 3;
pub(crate) const PWR_EVENT_LPM_IN_L2: u32 = 1 << 4;
pub(crate) const PWR_EVENT_LPM_OUT_L2: u32 = 1 << 5;
pub(crate) const PWR_EVENT_LPM_OUT_L1: u32 = 1 << 13;

pub(crate) const GCTL: usize = 0xc110;
pub(crate) const GUCTL: usize = 0xc12c;
pub(crate) const GUCTL2: usize = 0xc19c;
pub(crate) const GTXFIFOSIZ0: usize = 0xc300;
// DWC3_GUCTL1 is part of the global register block immediately after GCTL;
// 0xc360 is in the FIFO-register area and is not a user-control register.
pub(crate) const GUCTL1: usize = 0xc11c;
// msm-4.19 core.h: GUCTL3 sits in the 0xc600 block, not next to
// GUCTL1; the 4.19 offset is 0xc60c.
pub(crate) const GUCTL3: usize = 0xc60c;
pub(crate) const GSNPSID: usize = 0xc120;
pub(crate) const GRXTHRCFG: usize = 0xc10c;
pub(crate) const GSBUSCFG1: usize = 0xc104;
pub(crate) const GHWPARAMS0: usize = 0xc140;
pub(crate) const GHWPARAMS1: usize = 0xc144;
pub(crate) const GHWPARAMS3: usize = 0xc14c;
pub(crate) const GHWPARAMS7: usize = 0xc15c;
pub(crate) const GDBGLTSSM: usize = 0xc164;
// DWC_usb31 uses a separate link-debug block.  msm-4.19 core.h selects this
// offset whenever GSNPSID identifies the 0x3331/0x3332 IP.
pub(crate) const DWC31_LINK_GDBGLTSSM: usize = 0xd050;
// msm-4.19 dwc3_otg_start_peripheral() programs the USB3 LFPS exit-response
// timers on Bramble's DWC_usb31 before gadget VBUS connect.  This register is
// in the DWC3 core's USB31 link block, not in the QMP PHY window.
pub(crate) const DWC31_LINK_LU3LFPSRXTIM0: usize = 0xd010;
pub(crate) const DWC31_LINK_LU3LFPSRXTIM_GEN2_MASK: u32 = 0xff << 16;
pub(crate) const DWC31_LINK_LU3LFPSRXTIM_GEN1_MASK: u32 = 0xff;
pub(crate) const DWC31_LINK_LU3LFPSRXTIM_GEN2_BRAMBLE: u32 = 6 << 16;
pub(crate) const DWC31_LINK_LU3LFPSRXTIM_GEN1_BRAMBLE: u32 = 5;
pub(crate) const VER_NUMBER: usize = 0xc1a0;
pub(crate) const VER_TYPE: usize = 0xc1a4;
pub(crate) const GFLADJ: usize = 0xc630;
pub(crate) const GUSB2PHYCFG0: usize = 0xc200;
pub(crate) const GUSB3PIPECTL0: usize = 0xc2c0;
pub(crate) const GUSB2PHYCFG_ULPI_UTMI: u32 = 1 << 4;
pub(crate) const GUSB2PHYCFG_PHYIF_MASK: u32 = 1 << 3;
pub(crate) const GUSB2PHYCFG_USBTRDTIM_MASK: u32 = 0xf << 10;
pub(crate) const GUSB2PHYCFG_USBTRDTIM_UTMI_8_BIT: u32 = 9 << 10;
pub(crate) const GUSB2PHYCFG_USBTRDTIM_UTMI_16_BIT: u32 = 5 << 10;
pub(crate) const GUSB2PHYCFG_U2_FREECLK_EXISTS: u32 = 1 << 30;
pub(crate) const GEVNTADRLO0: usize = 0xc400;
pub(crate) const GEVNTADRHI0: usize = 0xc404;
pub(crate) const GEVNTSIZ0: usize = 0xc408;
pub(crate) const GEVNTCOUNT0: usize = 0xc40c;
pub(crate) const DEV_IMOD0: usize = 0xca00;
pub(crate) const GEVNT_BUFFER_STRIDE: usize = 0x10;
pub(crate) const DCFG: usize = 0xc700;
pub(crate) const DCTL: usize = 0xc704;
pub(crate) const DEVTEN: usize = 0xc708;
pub(crate) const DSTS: usize = 0xc70c;
pub(crate) const DALEPENA: usize = 0xc720;
pub(crate) const DEP_BASE: usize = 0xc800;

pub(crate) const GCTL_PRTCAPDIR_MASK: u32 = 3 << 12;
pub(crate) const GCTL_PRTCAP_DEVICE: u32 = 2 << 12;
pub(crate) const GCTL_PWRDNSCALE_MASK: u32 = 0xfff8_0000;
pub(crate) const GCTL_PWRDNSCALE_2: u32 = 2 << 19;
pub(crate) const GCTL_U2RSTECN: u32 = 1 << 16;
pub(crate) const GCTL_SOFITPSYNC: u32 = 1 << 10;
pub(crate) const GCTL_SCALEDOWN_MASK: u32 = 3 << 4;
pub(crate) const GCTL_DISSCRAMBLE: u32 = 1 << 3;
pub(crate) const GCTL_U2EXIT_LFPS: u32 = 1 << 2;
pub(crate) const GCTL_CORESOFTRESET: u32 = 1 << 11;
pub(crate) const GCTL_GBLHIBERNATIONEN: u32 = 1 << 1;
pub(crate) const GHWPARAMS1_EN_PWROPT_MASK: u32 = 3 << 24;
pub(crate) const GHWPARAMS1_EN_PWROPT_HIB: u32 = 2 << 24;
pub(crate) const GCTL_DSBLCLKGTNG: u32 = 1;
pub(crate) const GUCTL_REFCLKPER_MASK: u32 = 0xffc0_0000;
pub(crate) const GUCTL_REFCLKPER_19_2MHZ: u32 = 52 << 22;
pub(crate) const GFLADJ_REFCLK_FLADJ_MASK: u32 = 0x003f_ff00;
pub(crate) const GFLADJ_REFCLK_LPM_SEL: u32 = 1 << 23;
pub(crate) const GFLADJ_REFCLK_240MHZ_DECR: u32 = 12 << 24;
pub(crate) const GFLADJ_REFCLK_240MHZDECR_PLS1: u32 = 1 << 31;
pub(crate) const GFLADJ_REFCLK_FLADJ_19_2MHZ: u32 = 200 << 8;
pub(crate) const GUSB2PHYCFG_SUSPHY: u32 = 1 << 6;
pub(crate) const GUSB2PHYCFG_ENBLSLPM: u32 = 1 << 8;
pub(crate) const GUSB2PHYCFG_PHYSOFTRST: u32 = 1 << 31;
pub(crate) const GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST: u32 = 1 << 8;
pub(crate) const GUCTL1_DEV_L1_EXIT_BY_HW: u32 = 1 << 24;
pub(crate) const GUCTL1_IP_GAP_ADD_ON: u32 = 1 << 21;
pub(crate) const GUCTL3_USB20_RETRY_DISABLE: u32 = 1 << 16;
pub(crate) const GSBUSCFG1_PIPETRANSLIMIT_MASK: u32 = 0x0f << 8;
pub(crate) const GSBUSCFG1_PIPETRANSLIMIT_E: u32 = 0xe << 8;
pub(crate) const GUSB3PIPECTL_SUSPHY: u32 = 1 << 17;
pub(crate) const GUSB3PIPECTL_UX_EXIT_PX: u32 = 1 << 27;
pub(crate) const GUSB3PIPECTL_PHYSOFTRST: u32 = 1 << 31;

pub(crate) const DCTL_CSFTRST: u32 = 1 << 30;
pub(crate) const DCTL_SDIS: u32 = 1 << 0;
pub(crate) const DCTL_APPL1RES: u32 = 1 << 23;
pub(crate) const DCTL_HIRD_THRES_MASK: u32 = 0x1f << 24;
pub(crate) const DCTL_HIRD_THRES_LITO: u32 = 0x10 << 24;
pub(crate) const DCTL_HIRD_THRES_XBL: u32 = 0x07 << 24;
pub(crate) const DCTL_L1_HIBER_EN: u32 = 1 << 18;
pub(crate) const DCTL_KEEP_CONNECT: u32 = 1 << 19;
pub(crate) const DCTL_TSTCTRL_MASK: u32 = 0xf << 1;
pub(crate) const DCTL_TRGTULST_MASK: u32 = 0x0f << 17;
pub(crate) const DCTL_TRGTULST_RX_DET: u32 = 5 << 17;
pub(crate) const DCFG_NUMP_SHIFT: u32 = 17;
pub(crate) const DCFG_NUMP_MASK: u32 = 0x1f << DCFG_NUMP_SHIFT;
pub(crate) const DCFG_LPM_CAP: u32 = 1 << 22;
pub(crate) const DCFG_IGNSTRMPP: u32 = 1 << 23;
pub(crate) const DWC3_GRXTHRCFG_PKTCNTSEL: u32 = 1 << 29;
pub(crate) const DWC31_GRXTHRCFG_PKTCNTSEL: u32 = 1 << 26;
pub(crate) const GHWPARAMS0_MDWIDTH_SHIFT: u32 = 8;
pub(crate) const GHWPARAMS0_MDWIDTH_MASK: u32 = 0xff;
pub(crate) const GHWPARAMS7_RAM2_DEPTH_SHIFT: u32 = 16;
pub(crate) const GHWPARAMS7_RAM2_DEPTH_MASK: u32 = 0xffff;
pub(crate) const DWC3_IP: u32 = 0x5533;
pub(crate) const DWC31_IP: u32 = 0x3331;
pub(crate) const DWC32_IP: u32 = 0x3332;
pub(crate) const DWC31_REVISION_180A: u32 = 0x3138_302a;
pub(crate) const DWC31_REVISION_190A: u32 = 0x3139_302a;
// Linux applies the RxDetect reconnect workaround only through DWC3 1.87a.
// GSNPSID carries the same full revision value used by the upstream driver.
pub(crate) const DWC3_REVISION_187A: u32 = 0x5533_187a;
pub(crate) const DWC3_REVISION_190A: u32 = 0x5533_190a;
pub(crate) const DWC3_REVISION_194A: u32 = 0x5533_194a;
pub(crate) const DWC3_REVISION_220A: u32 = 0x5533_220a;
pub(crate) const DWC3_REVISION_250A: u32 = 0x5533_250a;
pub(crate) const DWC3_REVISION_310A: u32 = 0x5533_310a;
pub(crate) const DWC3_REVISION_270A: u32 = 0x5533_270a;
pub(crate) const DWC3_REVISION_290A: u32 = 0x5533_290a;
// msm-4.19 core.c dwc3_core_is_valid(): a DWC_usb31 core reports its
// revision as VER_NUMBER with the high bit set (1.70a = 0x3137302a).
pub(crate) const DWC3_REVISION_IS_DWC31: u32 = 0x8000_0000;
pub(crate) const DWC3_USB31_REVISION_170A: u32 = 0x3137_302a | DWC3_REVISION_IS_DWC31;
// VER_TYPE "ga**" marks the general-availability usb31 silicon.
pub(crate) const DWC3_USB31_VER_TYPE_GA: u32 = 0x6761_2a2a;
pub(crate) const GUCTL2_RST_ACTBITLATER: u32 = 1 << 14;

pub(crate) const HSPHY_UTMI_CTRL0: usize = 0x3c;
pub(crate) const HSPHY_UTMI_CTRL5: usize = 0x50;
pub(crate) const HSPHY_COMMON0: usize = 0x54;
pub(crate) const HSPHY_COMMON1: usize = 0x58;
pub(crate) const HSPHY_COMMON2: usize = 0x5c;
pub(crate) const HSPHY_CTRL1: usize = 0x60;
pub(crate) const HSPHY_CTRL2: usize = 0x64;
pub(crate) const HSPHY_CFG0: usize = 0x94;
pub(crate) const HSPHY_REFCLK_CTRL: usize = 0xa0;
pub(crate) const HSPHY_RTUNE_SEL: usize = 0xb4;
pub(crate) const HSPHY_TEST0: usize = 0x80;
pub(crate) const HSPHY_TEST1: usize = 0x84;

pub(crate) const HSPHY_UTMI_SLEEPM: u32 = 1 << 0;
pub(crate) const HSPHY_UTMI_ATE_RESET: u32 = 1 << 0;
pub(crate) const HSPHY_UTMI_POR: u32 = 1 << 1;
pub(crate) const HSPHY_COMMON0_FSEL_MASK: u32 = 0x7 << 4;
pub(crate) const HSPHY_COMMON0_VATESTENB_MASK: u32 = 0x3;
pub(crate) const HSPHY_COMMON1_VBUSVLDEXTSEL0: u32 = 1 << 4;
pub(crate) const HSPHY_COMMON1_PLLBTUNE: u32 = 1 << 5;
pub(crate) const HSPHY_COMMON2_VREGBYPASS: u32 = 1 << 0;
pub(crate) const HSPHY_CTRL1_VBUSVLDEXT0: u32 = 1 << 0;
pub(crate) const HSPHY_CTRL2_SUSPEND_N: u32 = 1 << 2;
pub(crate) const HSPHY_CTRL2_SUSPEND_N_SEL: u32 = 1 << 3;
pub(crate) const HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN: u32 = 1 << 1;
pub(crate) const HSPHY_TEST1_TESTDATAOUTSEL: u32 = 1 << 4;
pub(crate) const HSPHY_TEST1_TOGGLE_2WR: u32 = 1 << 6;
pub(crate) const HSPHY_TEST0_DATA_MASK: u32 = 0xff;

pub(crate) const PIPE_UTMI_CLK_SEL: u32 = 1 << 0;
pub(crate) const PIPE3_PHYSTATUS_SW: u32 = 1 << 3;
pub(crate) const PIPE_UTMI_CLK_DIS: u32 = 1 << 8;

// Qualcomm's Android wrapper reserves event buffers 1..N for GSI. These
// fields are part of the DWC3 event-buffer ABI, not ordinary endpoint
// registers, so keep the encoding next to the event-ring setup.
pub(crate) const GSI_TRB_ADDR_BIT_53: u32 = 1 << 21;
pub(crate) const GSI_TRB_ADDR_BIT_55: u32 = 1 << 23;
pub(crate) const GSI_CLK_EN: u32 = 1 << 12;
pub(crate) const GSI_RESTART_DBL_PNTR: u32 = 1 << 20;
pub(crate) const GSI_EN: u32 = 1 << 0;
pub(crate) const GSI_BLOCK_WR_GO: u32 = 1 << 1;
pub(crate) const GSI_EVENT_INTR_MASK: u32 = 1 << 31;
pub(crate) const GSI_EVENT_ADDR_EN_SHIFT: u32 = 22;
pub(crate) const GSI_EVENT_ADDR_INDEX_SHIFT: u32 = 16;
pub(crate) const GSI_WR_CTRL_STATE: u32 = 1 << 15;

pub(crate) const QMP_COM_PHY_MODE_CTRL: usize = 0x0000;
pub(crate) const QMP_COM_SW_RESET: usize = 0x0004;
pub(crate) const QMP_COM_POWER_DOWN_CTRL: usize = 0x0008;
pub(crate) const QMP_COM_TYPEC_CTRL: usize = 0x0010;
pub(crate) const QMP_COM_RESET_OVRD_CTRL: usize = 0x001c;
pub(crate) const QMP_PCS_STATUS1: usize = 0x1c14;
pub(crate) const QMP_PCS_STATUS2: usize = 0x1c18;
pub(crate) const QMP_PCS_AUTONOMOUS_MODE_CTRL: usize = 0x1f08;
pub(crate) const QMP_PCS_LFPS_RXTERM_IRQ_CLEAR: usize = 0x1f14;
pub(crate) const QMP_PCS_CLAMP_ENABLE: usize = 0x1c8c;
pub(crate) const QMP_PCS_POWER_DOWN_CONTROL: usize = 0x1c40;
pub(crate) const QMP_PCS_SW_RESET: usize = 0x1c00;
pub(crate) const QMP_PCS_START_CONTROL: usize = 0x1c44;
pub(crate) const QMP_PHYSTATUS: u32 = 1 << 6;
pub(crate) const QMP_ARCVR_DTCT_EN: u32 = 1 << 0;
pub(crate) const QMP_ALFPS_DTCT_EN: u32 = 1 << 1;
pub(crate) const QMP_ARCVR_DTCT_EVENT_SEL: u32 = 1 << 4;
pub(crate) const QMP_LFPS_IRQ_CLEAR: u32 = 1 << 0;
pub(crate) const QMP_CLAMP_EN: u32 = 1 << 0;

#[inline]
pub(super) fn reg(offset: usize) -> *mut u32 {
    (dwc3_base() + offset) as *mut u32
}

#[inline]
pub(super) fn qscratch_reg(offset: usize) -> *mut u32 {
    (qscratch_base() + offset) as *mut u32
}

#[inline]
pub(super) fn hsphy_reg(offset: usize) -> *mut u32 {
    (hsphy_base() + offset) as *mut u32
}

#[inline(always)]
pub(super) unsafe fn hsphy_write_barrier() {
    #[cfg(fullerene_aarch64_usb_hsphy_write_barrier)]
    core::arch::asm!("dsb st", options(nostack, preserves_flags));
}

#[inline]
pub(super) fn qmp_reg(offset: usize) -> *mut u32 {
    (qmp_base() + offset) as *mut u32
}

#[inline]
pub(super) fn qmp_contract_offset(slot: usize, fallback: usize) -> usize {
    let offset = super::super::platform::bramble::usb_resources().qmp_reg_offsets[slot];
    if offset == 0xffff { fallback } else { offset }
}

#[inline]
pub(super) unsafe fn smmu_reg(offset: usize) -> *mut u32 {
    (apps_smmu_base() + offset) as *mut u32
}

#[inline]
pub(super) unsafe fn smmu_page_reg(page_size: usize, page: usize, offset: usize) -> *mut u32 {
    (apps_smmu_base() + page * page_size + offset) as *mut u32
}

#[inline]
pub(super) unsafe fn smmu_page_write(page_size: usize, page: usize, offset: usize, value: u32) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset), value) };
}

#[inline]
pub(super) unsafe fn smmu_page_read(page_size: usize, page: usize, offset: usize) -> u32 {
    unsafe { read_volatile(smmu_page_reg(page_size, page, offset)) }
}

#[inline]
pub(super) unsafe fn smmu_page_write64(page_size: usize, page: usize, offset: usize, value: u64) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset).cast::<u64>(), value) };
}

#[inline]
pub(super) unsafe fn smmu_page_read64(page_size: usize, page: usize, offset: usize) -> u64 {
    unsafe { read_volatile(smmu_page_reg(page_size, page, offset).cast::<u64>()) }
}

#[inline]
pub(super) unsafe fn read_qscratch(offset: usize) -> u32 {
    unsafe { read_volatile(qscratch_reg(offset)) }
}

#[inline]
pub(super) unsafe fn write_qscratch(offset: usize, value: u32) {
    unsafe { write_volatile(qscratch_reg(offset), value) };
    unsafe { hsphy_write_barrier() };
    let _ = unsafe { read_volatile(qscratch_reg(offset)) };
}

#[inline]
pub(super) unsafe fn hsphy_update(offset: usize, mask: u32, value: u32) {
    let current = unsafe { read_volatile(hsphy_reg(offset)) };
    unsafe { write_volatile(hsphy_reg(offset), (current & !mask) | (value & mask)) };
    unsafe { hsphy_write_barrier() };
    let _ = unsafe { read_volatile(hsphy_reg(offset)) };
}

#[inline]
pub(super) unsafe fn dep_reg(endpoint: usize, offset: usize) -> usize {
    DEP_BASE + endpoint * 0x10 + offset
}

#[inline]
pub(super) unsafe fn read(offset: usize) -> u32 {
    unsafe { read_volatile(reg(offset)) }
}

#[inline]
pub(super) unsafe fn write(offset: usize, value: u32) {
    unsafe { write_volatile(reg(offset), value) }
    // Match Linux's writel() ordering barrier.  Without a DSB the CPU can
    // reorder a subsequent read (e.g. a DALEPENA readback) ahead of the
    // write's side-effect, making the register appear unchanged even though
    // the controller accepted the write.  This was the root cause of the
    // DALEPENA=0 readback observed across runs 1764675–1785948: the EP0
    // enable mask was written but never observed before the next MMIO load.
    core::arch::asm!("dsb st", options(nostack));
}
