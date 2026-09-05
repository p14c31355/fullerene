//! Stable QMP initialization tables and bootloader-DT PHY sequence installation.

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

/// Active PHY tables. The compiled values are the Bramble production
/// fallback, while the DT path may replace them after validating the complete
/// vendor property. Keeping the delay array separate preserves the compact
/// static table and still executes the DT's third cell rather than silently
/// dropping it.
pub(super) static mut ACTIVE_QMP_INIT: [(usize, u32); 146] = QMP_INIT;
pub(super) static mut ACTIVE_QMP_INIT_DELAY_US: [u32; 146] = [0; 146];
// The compiled fallback matches the exact-build Bramble stock DTB extracted
// from Google's UP1A.231105.001.B2 factory package:
// qcom,param-override-seq = <0x63 0x6c 0x85 0x70 0x17 0x74>. The first cell
// is the value and the second is the register offset; the writer below stores
// the pair as (offset, value). This fallback is used when `fastboot boot`
// supplies no usable property, so the exact stock package is the strongest
// available board-specific source. The trailing sentinel keeps the fixed
// table shape and is skipped by the writer.
pub(super) static mut ACTIVE_HSPHY_PARAM_OVERRIDE: [(usize, u32); 3] =
    [(0x6c, 0x63), (0x70, 0x85), (0x74, 0x17)];

/// Which table source the current boot is using. Recorded by
/// `install_dt_phy_sequences()` and published through the retained-trace
/// readout channel: 0 = compiled fallback (or no DT at all), 1 = a two-entry
/// HS override from the DT, 2 = a three-entry HS override from the DT, 3 =
/// the QMP table also installed from the DT. The fallback initial value is
/// 0, which is the correct classification before any install attempt.
pub(crate) static mut HSPHY_TABLE_SOURCE: u32 = 0;

/// Observation of the DT `qcom,param-override-seq` property exactly as the
/// FDT reader returned it, captured before any validation. The round-3
/// packing collapsed (value, offset) pairs into one slot and conflated
/// "property absent" with "property present but the first pair incomplete",
/// so the fields below are kept separately:
/// - `present` is set iff the property name was found on a matched,
///   enabled hsphy node (regardless of its length).
/// - `length_bytes` is the raw property byte length (0 when absent).
/// - `cells` holds the six raw big-endian cells in property order; `None`
///   means the cell was not present in the property.
pub(crate) static mut HS_DT_PARAM_OVERRIDE: (
    bool,
    u32,
    [Option<u32>; 6],
) = (false, 0, [None; 6]);

/// Identity of the compatible HS-PHY node used for the observation. The
/// ordinal is among enabled matching nodes; `reg_base` is the first `reg`
/// tuple address.
pub(crate) static mut HS_DT_NODE_IDENTITY: (bool, u32, u64) = (false, 0, 0);

pub fn hsphy_table_source() -> u32 {
    unsafe { HSPHY_TABLE_SOURCE }
}

/// Small identity readout codes. `ordinal` is capped for the four-bit timing
/// channel; `reg-match` is 1 only for the qpr1 primary base 0x088e3000.
pub fn hsphy_node_code(aspect: &str) -> u32 {
    let (node_present, ordinal, reg_base) = unsafe { HS_DT_NODE_IDENTITY };
    let (property_present, property_length, _) = unsafe { HS_DT_PARAM_OVERRIDE };
    match aspect {
        "ordinal" => node_present.then_some(ordinal.min(15)).unwrap_or(0),
        "reg-match" => {
            if !node_present {
                0
            } else if reg_base == 0x088e3000 {
                1
            } else {
                2
            }
        }
        // 1 = ordinal 0 + 0x088e3000 + property absent; 2/3/4 are
        // the same identity with 8/16/24-byte properties; 5 is another
        // length, 6 is another ordinal, and 7 is another base.
        "proof" => {
            if !node_present {
                0
            } else if ordinal != 0 {
                6
            } else if reg_base != 0x088e3000 {
                7
            } else if !property_present {
                1
            } else {
                match property_length {
                    8 => 2,
                    16 => 3,
                    24 => 4,
                    _ => 5,
                }
            }
        }
        _ => 0,
    }
}

/// Categorical readout of the DT observation, one small code per aspect:
/// `0` = property absent, `1` = present with the given byte length
/// (8/16/24 map to themselves, anything else reads as 4), so the timing
/// channel never carries a huge raw value. `pair0/1/2` classify each
/// (value, offset) entry: 0 = absent/incomplete, 1 = exactly the qpr1
/// base value, 2 = the known alternate, 3 = other. Each code fits in the
/// 4-bit attach-delay ladder without clipping.
pub fn hsphy_prop_code(aspect: &str) -> u32 {
    let (present, length, cells) = unsafe { HS_DT_PARAM_OVERRIDE };
    match aspect {
        "present" => u32::from(present),
        "len" => match length {
            0 => 0,
            8 | 16 | 24 => length / 8,
            _ => 4,
        },
        "pair0" | "pair1" | "pair2" => {
            let index = aspect["pair".len()..].parse::<usize>().unwrap_or(3);
            if index >= 3 {
                return 0;
            }
            let (value, offset) = match (cells[index * 2], cells[index * 2 + 1]) {
                (Some(value), Some(offset)) => (value, offset),
                _ => return 0,
            };
            match (aspect, value, offset) {
                ("pair0", 0x67, 0x6c) => 1,
                ("pair1", 0xc8, 0x70) => 1,
                ("pair0", 0x63, 0x6c) | ("pair1", 0x85, 0x70) => 2,
                ("pair2", 0x17, 0x74) => 1,
                _ => 3,
            }
        }
        _ => 0,
    }
}

/// Record the DT observation from the install path. Called from main.rs
/// while the DTB is live; later code reads it through `hsphy_prop_code()`.
pub fn record_hs_dt_param_override_observation(
    observation: Option<(bool, u32)>,
    cells: [Option<u32>; 6],
) {
    let (present, length) = observation.unwrap_or((false, 0));
    unsafe {
        HS_DT_PARAM_OVERRIDE = (present, length, cells);
    }
}

pub fn record_hs_dt_node_identity(observation: Option<(usize, Option<u64>)>) {
    unsafe {
        HS_DT_NODE_IDENTITY = observation
            .map(|(ordinal, reg_base)| {
                (true, ordinal.min(u32::MAX as usize) as u32, reg_base.unwrap_or(0))
            })
            .unwrap_or((false, 0, 0));
    }
}

/// Install the complete PHY programming properties from the bootloader DTB.
/// A partial or malformed property is rejected as a unit, leaving the known
/// Bramble fallback in place. The QMP binding terminates its 146 triples with
/// `<0xffffffff 0xffffffff 0>`, which is a sentinel and is not written.
pub fn install_dt_phy_sequences(hs_raw: [Option<u32>; 6], qmp_raw: [Option<u32>; 441]) -> bool {
    let mut installed = false;

    // The base Lito node has three QUSB2 override entries, and the Google
    // qpr1 sources (lito-usb.dtsi and lito-qrd.dtsi) both keep three
    // entries: TUNE1 (0x6c), TUNE2 (0x70), TUNE3 (0x74). Accept both the
    // three-entry source form and the two-entry historical fallback so a
    // shorter production DT is not silently discarded in favour of the
    // broader SoC fallback.
    let hs_three = hs_raw.iter().all(Option::is_some);
    let hs_two = hs_raw[..4].iter().all(Option::is_some) && hs_raw[4..].iter().all(Option::is_none);
    if hs_three || hs_two {
        let count = if hs_two { 2 } else { 3 };
        let mut entries = [(0usize, 0u32), (0usize, 0u32), (usize::MAX, 0u32)];
        let mut valid = true;
        for index in 0..count {
            let value = hs_raw[index * 2].unwrap();
            let offset = hs_raw[index * 2 + 1].unwrap();
            valid &= value <= 0xff
                && matches!(offset, 0x6c | 0x70 | 0x74)
                && entries[..index]
                    .iter()
                    .all(|entry| entry.0 != offset as usize);
            entries[index] = (offset as usize, value);
        }
        if valid {
            unsafe {
                ACTIVE_HSPHY_PARAM_OVERRIDE = entries;
                // 1 = two-entry production override, 2 = three-entry SoC
                // override. Distinguish them so the retained-trace readout
                // can prove which analog tuning the boot actually used.
                HSPHY_TABLE_SOURCE = if hs_two { 1 } else { 2 };
            }
            installed = true;
        }
    }

    if qmp_raw.iter().all(Option::is_some)
        && qmp_raw[438] == Some(u32::MAX)
        && qmp_raw[439] == Some(u32::MAX)
        && qmp_raw[440] == Some(0)
    {
        let mut entries = [(0usize, 0u32); 146];
        let mut delays = [0u32; 146];
        let mut valid = true;
        for index in 0..146 {
            let raw = index * 3;
            let offset = qmp_raw[raw].unwrap();
            let value = qmp_raw[raw + 1].unwrap();
            let delay_us = qmp_raw[raw + 2].unwrap();
            valid &= offset <= 0x2fff && value <= 0xff && delay_us <= 1_000_000;
            entries[index] = (offset as usize, value);
            delays[index] = delay_us;
        }
        if valid {
            unsafe {
                ACTIVE_QMP_INIT = entries;
                ACTIVE_QMP_INIT_DELAY_US = delays;
                // Bit 8 marks a fully DT-installed QMP table on top of the
                // HS classification, so a fallback QMP table cannot be
                // confused with a DT-supplied one in the readout.
                HSPHY_TABLE_SOURCE |= 0x100;
            }
            installed = true;
        }
    }

    installed
}
