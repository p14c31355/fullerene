//! Resistive touchscreen input backend with an explicit calibration policy.

use super::{board::BoardProfile, i2c::I2cBus};

#[derive(Clone, Copy, Debug)]
pub struct TouchSample {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    pub min_x: u16,
    pub max_x: u16,
    pub min_y: u16,
    pub max_y: u16,
    pub swap_xy: bool,
}

impl Calibration {
    pub fn transform(&self, sample: TouchSample) -> TouchSample {
        let x = sample.x.clamp(self.min_x, self.max_x);
        let y = sample.y.clamp(self.min_y, self.max_y);
        TouchSample {
            x: if self.swap_xy { y } else { x },
            y: if self.swap_xy { x } else { y },
            pressed: sample.pressed,
        }
    }
}

pub struct ResistiveTouch {
    bus: I2cBus,
    calibration: Calibration,
}

impl ResistiveTouch {
    pub fn new(profile: BoardProfile) -> Self {
        Self {
            bus: I2cBus::new(profile.touch.i2c_sda, profile.touch.i2c_scl),
            calibration: Calibration {
                min_x: 200,
                max_x: 3_900,
                min_y: 240,
                max_y: 3_800,
                swap_xy: false,
            },
        }
    }

    pub fn read(&mut self) -> Option<TouchSample> {
        Some(self.calibration.transform(TouchSample {
            x: 0,
            y: 0,
            pressed: false,
        }))
    }

    pub fn set_calibration(&mut self, calibration: Calibration) {
        self.calibration = calibration;
    }
}
