//! Turning four measured panel corners into an axis mapping.
//!
//! A resistive panel reports raw ADC values that have no fixed relationship to
//! the screen: either axis may be reversed, and which raw axis answers a
//! horizontal finger movement depends on how the panel is bonded. Four known
//! screen points are enough to resolve all of it, and this is the arithmetic
//! that does it - kept apart from the driver so it can be tested on the host,
//! because getting it wrong is invisible until someone taps a button and
//! watches the wrong thing happen.

/// One measured point: raw panel coordinates for a known screen position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

impl Point {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// The raw readings taken at the four corners of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CornerSamples {
    pub top_left: Point,
    pub top_right: Point,
    pub bottom_left: Point,
    pub bottom_right: Point,
}

/// How one screen axis maps onto a raw axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisMapping {
    pub min: u16,
    pub max: u16,
    /// Whether the raw value decreases as the screen coordinate increases.
    pub mirror: bool,
}

impl AxisMapping {
    /// The fraction of the axis, in 0..=span, for a raw reading.
    pub const fn span(&self) -> u32 {
        self.max.saturating_sub(self.min) as u32
    }
}

/// The resolved mapping for the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelCalibration {
    pub x: AxisMapping,
    pub y: AxisMapping,
    /// Whether the raw Y axis is the one that answers screen X.
    pub swap_xy: bool,
}

/// Work out the mapping from the four corner readings.
///
/// The swap decision comes from which raw axis moves when the finger moves
/// horizontally: if the top-left and top-right readings differ more in raw Y
/// than in raw X, the panel is bonded with its axes crossed.
pub fn resolve(corners: CornerSamples) -> PanelCalibration {
    let horizontal_raw_x = corners.top_left.x.abs_diff(corners.top_right.x);
    let horizontal_raw_y = corners.top_left.y.abs_diff(corners.top_right.y);
    let swap_xy = horizontal_raw_y > horizontal_raw_x;

    // Screen X is answered by raw X, or by raw Y when the panel is crossed.
    let (left, right, top, bottom) = if swap_xy {
        (
            corners.top_left.y,
            corners.top_right.y,
            corners.top_left.x,
            corners.bottom_left.x,
        )
    } else {
        (
            corners.top_left.x,
            corners.top_right.x,
            corners.top_left.y,
            corners.bottom_left.y,
        )
    };

    let x_min = left.min(right);
    let x_max = left.max(right);
    let y_min = top.min(bottom);
    let y_max = top.max(bottom);

    PanelCalibration {
        x: AxisMapping {
            min: x_min,
            max: x_max,
            // The screen's left edge gives the larger raw value.
            mirror: left > right,
        },
        y: AxisMapping {
            min: y_min,
            max: y_max,
            mirror: top > bottom,
        },
        swap_xy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corners(tl: (u16, u16), tr: (u16, u16), bl: (u16, u16), br: (u16, u16)) -> CornerSamples {
        CornerSamples {
            top_left: Point::new(tl.0, tl.1),
            top_right: Point::new(tr.0, tr.1),
            bottom_left: Point::new(bl.0, bl.1),
            bottom_right: Point::new(br.0, br.1),
        }
    }

    #[test]
    fn a_direct_panel_maps_without_mirrors() {
        // Raw grows right and down, exactly like the screen.
        let resolved = resolve(corners((0, 0), (3300, 0), (0, 3400), (3300, 3400)));
        assert_eq!(resolved.swap_xy, false);
        assert_eq!(
            resolved.x,
            AxisMapping {
                min: 0,
                max: 3300,
                mirror: false
            }
        );
        assert_eq!(
            resolved.y,
            AxisMapping {
                min: 0,
                max: 3400,
                mirror: false
            }
        );
    }

    #[test]
    fn a_horizontally_reversed_panel_is_detected() {
        let resolved = resolve(corners((3300, 0), (0, 0), (3300, 3400), (0, 3400)));
        assert!(!resolved.swap_xy);
        assert!(resolved.x.mirror, "left edge reading high means mirror x");
        assert!(!resolved.y.mirror);
    }

    #[test]
    fn a_vertically_reversed_panel_is_detected() {
        let resolved = resolve(corners((0, 3400), (3300, 3400), (0, 0), (3300, 0)));
        assert!(!resolved.swap_xy);
        assert!(!resolved.x.mirror);
        assert!(resolved.y.mirror, "top edge reading high means mirror y");
    }

    #[test]
    fn a_crossed_panel_is_detected_even_when_both_axes_are_reversed() {
        // Screen X moves with raw Y and screen Y moves with raw X, and both run
        // backwards. This is the case a corner-by-corner guess gets wrong.
        let resolved = resolve(corners((3400, 3300), (3400, 0), (0, 3300), (0, 0)));
        assert!(resolved.swap_xy, "horizontal movement changed raw Y");
        assert_eq!(resolved.x.min, 0);
        assert_eq!(resolved.x.max, 3300);
        assert!(
            resolved.x.mirror,
            "the left edge reads the high end of raw Y"
        );
        assert_eq!(resolved.y.min, 0);
        assert_eq!(resolved.y.max, 3400);
        assert!(
            resolved.y.mirror,
            "the top edge reads the high end of raw X"
        );
    }

    #[test]
    fn the_span_reflects_the_measured_extremes() {
        let resolved = resolve(corners((400, 300), (3600, 300), (400, 3700), (3600, 3700)));
        assert_eq!(resolved.x.span(), 3200);
        assert_eq!(resolved.y.span(), 3400);
    }
}
