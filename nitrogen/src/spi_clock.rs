//! SPI master clock-divider arithmetic, shared by SPI backends.
//!
//! The search below is a port of ESP-IDF's `spi_ll_master_cal_clock`: the
//! divider `n` is brute forced and the prescaler `pre` derived for each, the
//! best frequency match wins, and on ties the larger `n` is kept because it
//! gives the finest duty-cycle resolution.
//!
//! It lives apart from the ESP32 register block so the arithmetic can be
//! tested on the host - divider bugs are silent on hardware until the panel
//! shows garbage, which is an expensive place to find them.

/// The `SPI_CLOCK_...` fields a backend has to write, already positioned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDivider {
    /// Raw register value with the fields in their hardware positions.
    pub register: u32,
    /// Prescaler value (1-based), for reporting.
    pub prescale: u32,
    /// Divider value (1-based), for reporting.
    pub divider: u32,
    /// Whether the peripheral should run straight off the bus clock.
    pub equals_system_clock: bool,
}

impl ClockDivider {
    pub fn frequency_hz(&self, bus_hz: u32) -> u32 {
        if self.equals_system_clock {
            return bus_hz;
        }
        bus_hz / (self.prescale * self.divider)
    }
}

/// Compute the divider fields for a target frequency.
///
/// `duty_cycle` is in 256ths: 128 is an even split.
///
/// ESP-IDF keeps the *closest* match, which may sit slightly above the request.
/// For a panel that is the wrong contract: the requested frequency is the
/// ceiling its timing spec allows, so an above-ceiling result is out of spec
/// even when it is numerically nearer. The search therefore tracks the best
/// at-or-below candidate and only falls back to a higher clock when no divider
/// can reach the target at all.
pub fn solve(
    bus_hz: u32,
    target_hz: u32,
    duty_cycle: u32,
    prescale_shift: u32,
    divider_shift: u32,
    high_shift: u32,
    equals_system_clock_bit: u32,
) -> ClockDivider {
    if target_hz == 0 || target_hz >= bus_hz {
        return ClockDivider {
            register: equals_system_clock_bit,
            prescale: 1,
            divider: 1,
            equals_system_clock: true,
        };
    }
    let bus = i64::from(bus_hz);
    let target = i64::from(target_hz);
    let mut below: Option<(i64, i64, i64)> = None;
    let mut nearest: Option<(i64, i64, i64)> = None;
    let mut candidate = 2i64;
    while candidate <= 64 {
        // This is what "pick the prescaler, then keep the divider with the
        // least error" looks like in integer arithmetic.
        let mut prescale = ((bus / candidate) + (target / 2)) / target;
        if prescale <= 0 {
            prescale = 1;
        }
        if prescale > 8192 {
            prescale = 8192;
        }
        let frequency = bus / (prescale * candidate);
        let error = (frequency - target).abs();
        // Ties go to the larger divider, as in the reference search.
        if nearest.is_none_or(|(_, _, best)| error <= best) {
            nearest = Some((candidate, prescale, error));
        }
        if frequency <= target && below.is_none_or(|(_, _, best)| error <= best) {
            below = Some((candidate, prescale, error));
        }
        candidate += 1;
    }

    let (divider, prescale, _) = below.or(nearest).unwrap_or((2, 8192, 0));
    let divider = divider as u32;
    let prescale = prescale as u32;
    let mut high = (duty_cycle * divider + 127) / 256;
    if high == 0 {
        high = 1;
    }
    ClockDivider {
        // The low-count field is always the divider minus one in ESP-IDF's
        // layout, so `divider - 1` also lands in the low bits.
        register: ((prescale - 1) << prescale_shift)
            | ((divider - 1) << divider_shift)
            | ((high - 1) << high_shift)
            | (divider - 1),
        prescale,
        divider,
        equals_system_clock: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ESP32 SPI_CLOCK field positions, so the tests exercise the real
    // layout rather than an invented one.
    const PRESCALE_SHIFT: u32 = 18;
    const DIVIDER_SHIFT: u32 = 12;
    const HIGH_SHIFT: u32 = 6;
    const EQUALS_SYSCLK: u32 = 1 << 31;

    fn solve_esp32(bus: u32, target: u32) -> ClockDivider {
        solve(
            bus,
            target,
            128,
            PRESCALE_SHIFT,
            DIVIDER_SHIFT,
            HIGH_SHIFT,
            EQUALS_SYSCLK,
        )
    }

    #[test]
    fn twenty_megahertz_from_eighty_matches_the_documented_fields() {
        let divider = solve_esp32(80_000_000, 20_000_000);
        assert_eq!(divider.prescale, 1);
        assert_eq!(divider.divider, 4);
        assert_eq!(divider.register >> PRESCALE_SHIFT & 0x1fff, 0);
        assert_eq!(divider.register >> DIVIDER_SHIFT & 0x3f, 3);
        assert_eq!(divider.register >> HIGH_SHIFT & 0x3f, 1);
        assert_eq!(divider.register & 0x3f, 3);
        assert_eq!(divider.frequency_hz(80_000_000), 20_000_000);
    }

    #[test]
    fn one_megahertz_from_eighty_uses_prescale_eighty() {
        let divider = solve_esp32(80_000_000, 1_000_000);
        assert_eq!(divider.frequency_hz(80_000_000), 1_000_000);
        assert_eq!(divider.prescale * divider.divider, 80);
    }

    #[test]
    fn the_result_never_exceeds_the_requested_clock_and_stays_close() {
        for target in [1_000_000u32, 2_500_000, 10_000_000, 26_000_000, 40_000_000] {
            let divider = solve_esp32(80_000_000, target);
            let actual = divider.frequency_hz(80_000_000);
            assert!(
                actual <= target,
                "{actual} Hz exceeded the {target} Hz request"
            );
            let step = actual / 2 + 1;
            assert!(
                target - actual <= step,
                "{actual} Hz is more than one divider step below {target} Hz"
            );
        }
    }

    #[test]
    fn a_target_above_the_bus_clock_selects_the_bus_clock() {
        let divider = solve_esp32(80_000_000, 120_000_000);
        assert!(divider.equals_system_clock);
        assert_eq!(divider.register, EQUALS_SYSCLK);
        assert_eq!(divider.frequency_hz(80_000_000), 80_000_000);
    }

    #[test]
    fn the_prescaler_stays_inside_the_field_width() {
        // A very slow request must clamp the prescaler instead of overflowing
        // into neighbouring register fields.
        let divider = solve_esp32(80_000_000, 1_000);
        assert!(divider.prescale <= 8192);
        assert!((divider.register >> PRESCALE_SHIFT) & 0x1fff == divider.prescale - 1);
    }
}
