//! Platform-neutral power and battery state primitives.
//!
//! Backends publish measurements through this value type.  It intentionally
//! contains no policy and no MMIO access, so firmware-owned PMICs can be
//! described before a safe transport driver exists.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryState {
    pub voltage_mv: Option<u32>,
    pub current_ma: Option<i32>,
    pub temperature_millic: Option<i32>,
    pub charge_permille: Option<u16>,
    pub design_capacity_mah: Option<u32>,
    pub charging: bool,
    pub present: bool,
}

impl BatteryState {
    pub const UNKNOWN: Self = Self {
        voltage_mv: None,
        current_ma: None,
        temperature_millic: None,
        charge_permille: None,
        design_capacity_mah: None,
        charging: false,
        present: false,
    };

    pub const fn usable(self) -> bool {
        self.present && (self.voltage_mv.is_some() || self.charge_permille.is_some())
    }
}

/// Open-circuit voltage → state of charge, from a vendor OCV table.
///
/// Phone vendors ship one table per battery part number in the device tree
/// (Huawei: `pc_temp_ocv_percent`, `pc_temp_ocv_ocv`, `pc_temp_ocv_cols`).
/// The table is a monotonic ladder: each row is a percent step, each column
/// a temperature.  This module keeps the interpolation maths free of MMIO so
/// every platform backend can share it and it can be tested on the host.
pub mod ocv {
    /// Rows in a Huawei `pc_temp_ocv` ladder (100 %, 95 %, … 1 %).
    pub const ROWS: usize = 29;
    /// Temperature columns shipped by the vendor (`temp_points[]`).
    pub const COLS: usize = 6;
    pub const TEMPS_C: [i32; COLS] = [-20, -10, 0, 25, 40, 60];

    /// A vendor `pc_temp_ocv` table: `percents[i]` is the state of charge
    /// whose resting voltage is `mv[i][t]` millivolts at `TEMPS_C[t]`.
    #[derive(Clone, Copy, Debug)]
    pub struct OcvTable {
        pub percents: [u8; ROWS],
        pub mv: [[u16; COLS]; ROWS],
    }

    /// Percent (1..=100) for a resting voltage of `ocv_mv` at `temp_c`.
    ///
    /// Temperatures between columns are interpolated linearly; outside the
    /// table the nearest column is used.  Voltages above the 100 % entry
    /// clamp to 100, below the lowest entry clamp to the last row.
    pub fn percent(table: &OcvTable, ocv_mv: u32, temp_c: i32) -> u8 {
        let (lo, hi, frac_e4) = bracket_temp(temp_c);
        let mut ladder = [0u32; ROWS];
        for i in 0..ROWS {
            ladder[i] = mv_at(table, i, lo, hi, frac_e4) as u32;
        }

        if ocv_mv >= ladder[0] {
            return 100;
        }
        for i in 0..ROWS - 1 {
            let top = ladder[i];
            let next = ladder[i + 1];
            if ocv_mv <= top && ocv_mv >= next {
                let p0 = table.percents[i] as i64;
                let p1 = table.percents[i + 1] as i64;
                let span = (top - next) as i64;
                if span == 0 {
                    return p1.max(1) as u8;
                }
                let into = (top - ocv_mv) as i64;
                let p = p0 + (p1 - p0) * into / span;
                return p.max(1).min(100) as u8;
            }
        }
        table.percents[ROWS - 1].max(1)
    }

    /// Interpolated millivolts for `row` at `temp_c`, using the bracketing
    /// columns `lo..=hi`.  `frac_e4` is the position from `lo` to `hi` in
    /// ten-thousandths.
    pub fn mv_at(table: &OcvTable, row: usize, lo: usize, hi: usize, frac_e4: u16) -> u32 {
        let a = table.mv[row][lo] as i64;
        let b = table.mv[row][hi] as i64;
        (a + (b - a) * frac_e4 as i64 / 10_000) as u32
    }

    /// Bracket `temp_c` inside `TEMPS_C`, returning `(lo, hi, frac_e4)`.
    pub fn bracket_temp(temp_c: i32) -> (usize, usize, u16) {
        if temp_c <= TEMPS_C[0] {
            return (0, 0, 0);
        }
        for t in 0..COLS - 1 {
            if temp_c <= TEMPS_C[t + 1] {
                let span = (TEMPS_C[t + 1] - TEMPS_C[t]) as i32;
                let off = temp_c - TEMPS_C[t];
                return (t, t + 1, (off * 10_000 / span) as u16);
            }
        }
        (COLS - 1, COLS - 1, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::BatteryState;
    use super::ocv::{self, COLS, OcvTable, ROWS, bracket_temp};

    /// A small ladder with the shape of the vendor table: 100 % down to 1 %,
    /// two temperature columns only (cold, warm) so the maths is checkable
    /// by hand.  Values taken from the ANE `SCUDATL` table at -20 °C and
    /// 25 °C respectively for the first and last row.
    fn ladder() -> OcvTable {
        let mut percents = [0u8; ROWS];
        let mut mv = [[0u16; COLS]; ROWS];
        for i in 0..ROWS {
            percents[i] = 100 - (i as u8) * 3;
            for t in 0..COLS {
                mv[i][t] = 4300 - (i as u16) * 40;
            }
        }
        OcvTable { percents, mv }
    }

    #[test]
    fn top_of_the_ladder_is_full() {
        let t = ladder();
        assert_eq!(ocv::percent(&t, 4300, 25), 100);
        assert_eq!(ocv::percent(&t, 4400, 25), 100);
    }

    #[test]
    fn middle_interpolates_between_rows() {
        let t = ladder();
        // 3 points per 40 mV; truncation keeps the lower integer percent.
        assert_eq!(ocv::percent(&t, 4280, 25), 99);
        assert_eq!(ocv::percent(&t, 4260, 25), 97);
    }

    #[test]
    fn floor_is_the_last_row() {
        let t = ladder();
        assert_eq!(ocv::percent(&t, 1000, 25), 16);
    }

    #[test]
    fn temperature_brackets() {
        assert_eq!(bracket_temp(-40), (0, 0, 0));
        assert_eq!(bracket_temp(-20), (0, 0, 0));
        assert_eq!(bracket_temp(25), (2, 3, 10_000));
        assert_eq!(bracket_temp(32), (3, 4, 4666));
        assert_eq!(bracket_temp(90), (5, 5, 0));
    }

    #[test]
    fn unknown_state_is_not_usable() {
        assert!(!BatteryState::UNKNOWN.usable());
    }

    #[test]
    fn measured_state_is_usable() {
        let state = BatteryState {
            voltage_mv: Some(3800),
            present: true,
            ..BatteryState::UNKNOWN
        };
        assert!(state.usable());
    }
}
