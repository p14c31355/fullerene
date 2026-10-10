//! Read-only clock contracts shared by platform backends.
//!
//! A descriptor records the clock relationship a driver requires without
//! implying ownership of, or writes to, the clock provider.  This is the
//! boundary used while firmware/secure-world clock ownership is unresolved.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockKind {
    Fixed,
    FixedFactor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockDescriptor {
    pub kind: ClockKind,
    pub phandle: u32,
    pub parent_phandle: Option<u32>,
    pub rate_hz: u32,
    pub divider: u32,
    pub multiplier: u32,
}

impl ClockDescriptor {
    pub const fn fixed(phandle: u32, rate_hz: u32) -> Self {
        Self {
            kind: ClockKind::Fixed,
            phandle,
            parent_phandle: None,
            rate_hz,
            divider: 1,
            multiplier: 1,
        }
    }

    pub const fn fixed_factor(
        phandle: u32,
        parent_phandle: u32,
        rate_hz: u32,
        divider: u32,
        multiplier: u32,
    ) -> Self {
        Self {
            kind: ClockKind::FixedFactor,
            phandle,
            parent_phandle: Some(parent_phandle),
            rate_hz,
            divider,
            multiplier,
        }
    }

    pub const fn is_well_formed(self) -> bool {
        self.rate_hz != 0
            && self.divider != 0
            && self.multiplier != 0
            && match self.kind {
                ClockKind::Fixed => self.parent_phandle.is_none(),
                ClockKind::FixedFactor => self.parent_phandle.is_some(),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockDescriptor, ClockKind};

    #[test]
    fn fixed_clock_has_no_parent() {
        let clock = ClockDescriptor::fixed(0x28, 20_000_000);
        assert_eq!(clock.kind, ClockKind::Fixed);
        assert!(clock.is_well_formed());
    }

    #[test]
    fn fixed_factor_requires_parent_and_divider() {
        let clock = ClockDescriptor::fixed_factor(0x53, 0x12f, 19_200_000, 1, 1);
        assert_eq!(clock.kind, ClockKind::FixedFactor);
        assert!(clock.is_well_formed());
        assert!(!ClockDescriptor::fixed_factor(0x53, 0x12f, 19_200_000, 0, 1).is_well_formed());
    }
}
