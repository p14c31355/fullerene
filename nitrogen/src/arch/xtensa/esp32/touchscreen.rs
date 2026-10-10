//! XPT2046 resistive-touch backend for the ESP32 carrier board.
//!
//! The controller uses a separate SPI transport from the LCD, and it stays
//! bit-banged: it is specified for a ~2.5 MHz clock, its four signal pins are
//! not the LCD's, and the LCD is what needs the peripheral. What changed here
//! is the interface above the sampler - `press`/`release`/`move` are turned
//! into Down/Move/Up events by the shared tracker in `nitrogen::input`, so the
//! shell no longer does edge detection of its own.
//!
//! Coordinates are normalized by calibration, but the absolute calibration is
//! still provisional until the user performs a four-corner touch calibration.

use super::{
    board::{BoardProfile, DISPLAY_HEIGHT, DISPLAY_WIDTH},
    gpio,
};
use crate::input::{PointerConfig, PointerEvent, PointerSample, PointerTracker};

/// XPT2046 control bytes: start bit, channel, 12-bit differential conversion.
///
/// A2A1A0 selects the channel: 101 is X, 001 is Y, 011 is Z1 and 100 is Z2.
///
/// These are the values this unit was measured to answer. The sweep in
/// `SCAN_COMMANDS` was run with a finger held on the panel: channel 101 read a
/// stable 1024/1025 and channel 001 read 791, while the same channels with the
/// low bits set to 01 returned a constant near 50 and never moved. The
/// datasheet lists both combinations, so the command byte is not something to
/// assume - on this part only the power-down-00 form converts.
pub const X_COMMAND: u8 = 0xd0;
pub const Y_COMMAND: u8 = 0x90;
pub const Z1_COMMAND: u8 = 0xb1;
pub const Z2_COMMAND: u8 = 0xc1;

/// Control bytes worth trying when an axis does not respond.
///
/// The channel map above is the datasheet one, but clones and re-marked parts
/// exist, and the low bits select single-ended versus differential drive and
/// the power-down mode. Sweeping them while a finger is held separates a wrong
/// command from no signal on that plate: a command that responds to the finger
/// is the right one, and one that never moves has nothing behind it.
pub const SCAN_COMMANDS: [u8; 16] = [
    0xd0, 0xd1, 0xd2, 0xd3, // channel 101 (X), the four mode/PD combinations
    0xd8, 0xd9, 0xda, 0xdb, // channel 101, single-ended drive
    0x90, 0x91, 0x98, 0x99, // channel 001 (Y)
    0xb1, 0xc1, 0xa1, 0xe1, // channels 011, 100, 010, 111
];

/// The two commands the sweep showed answering on this unit.
pub const WORKING_COMMANDS: (u8, u8) = (X_COMMAND, Y_COMMAND);

/// How far apart the two conversions of the same axis may be.
///
/// Each axis is converted twice on purpose: a genuine contact produces two
/// readings that agree closely, while an unconnected or floating panel does
/// not. On this unit the readings were pinned at a constant value in both
/// axes while the pressure estimate still claimed a press, which produced a
/// storm of phantom Down/Up pairs; this gate is what separates the two cases.
pub const AXIS_SPREAD_LIMIT: u16 = 120;

/// Contact threshold on the pressure channel.
///
/// The discriminator is `z1 - z2`, measured rather than assumed. On this unit
/// an untouched panel reads the two about equal (z1 - z2 within a few counts of
/// zero), while a finger on the glass pulls them roughly 475 counts apart. The
/// earlier estimate `z1 + 4095 - z2` was useless for the same measurement: it
/// sat near 4092 idle and 4570 pressed, so any floor low enough to catch a
/// touch also fired on an untouched panel, and the shell saw taps nobody made.
pub const PRESSURE_FLOOR: u16 = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub min_x: u16,
    pub max_x: u16,
    pub min_y: u16,
    pub max_y: u16,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub swap_xy: bool,
}

impl Calibration {
    /// Convert a calibrated raw point to screen coordinates.
    ///
    /// The mapping clamps before division, avoiding 16-bit overflow and making
    /// noisy edge samples stay within the panel.
    pub fn map_to_screen(&self, sample: TouchSample, width: u16, height: u16) -> (u16, u16) {
        let min_x = self.min_x.min(self.max_x);
        let max_x = self.min_x.max(self.max_x);
        let min_y = self.min_y.min(self.max_y);
        let max_y = self.min_y.max(self.max_y);
        let (raw_x, raw_x_min, raw_x_max) = if self.swap_xy {
            (sample.y, min_y, max_y)
        } else {
            (sample.x, min_x, max_x)
        };
        let (raw_y, raw_y_min, raw_y_max) = if self.swap_xy {
            (sample.x, min_x, max_x)
        } else {
            (sample.y, min_y, max_y)
        };
        let raw_x = raw_x.clamp(raw_x_min, raw_x_max);
        let raw_y = raw_y.clamp(raw_y_min, raw_y_max);
        let span_x = u32::from(raw_x_max - raw_x_min).max(1);
        let span_y = u32::from(raw_y_max - raw_y_min).max(1);
        let mapped_x = (u32::from(raw_x - raw_x_min) * u32::from(width.saturating_sub(1)) / span_x)
            .min(u32::from(width.saturating_sub(1)));
        let mapped_y = (u32::from(raw_y - raw_y_min) * u32::from(height.saturating_sub(1))
            / span_y)
            .min(u32::from(height.saturating_sub(1)));
        let screen_x = if self.mirror_x {
            u32::from(width.saturating_sub(1)).saturating_sub(mapped_x)
        } else {
            mapped_x
        };
        let screen_y = if self.mirror_y {
            u32::from(height.saturating_sub(1)).saturating_sub(mapped_y)
        } else {
            mapped_y
        };
        (screen_x as u16, screen_y as u16)
    }
}

pub struct Xpt2046Touch {
    clk: u8,
    mosi: u8,
    miso: u8,
    cs: u8,
    irq: u8,
    calibration: Calibration,
    tracker: PointerTracker,
    /// Count of device reads this object has performed, including the ones
    /// taken between display chunks. The latency report uses it to prove that
    /// input kept running while a flush was in flight.
    samples_taken: u32,
    /// Timestamp of the last device read, for the idle sampling floor.
    last_sample_ms: u32,
    /// Diagnostics from the last conversion: the raw panel coordinates and the
    /// pressure estimate they came with. Printed over UART so a calibration
    /// can be derived from what the panel actually reports instead of from a
    /// default that was never checked against this unit.
    last_raw: (u16, u16),
    /// Axis conversions from the most recent read, pressure gate or not.
    last_axes: (u16, u16),
    /// The four conversions behind the last sample: X twice, then Y twice.
    /// Kept so a rejected reading can be explained instead of just counted.
    last_quad: (u16, u16, u16, u16),
    last_pressure: u16,
    /// Readings discarded because the duplicated axis conversions disagreed.
    spread_rejects: u32,
    channel: TouchChannel,
}

/// What the boot self-test concluded about the touch channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchChannel {
    /// Not tested yet.
    Unknown,
    /// Idle readings vary, so the controller is converting something real.
    Reporting,
    /// Idle readings never change: the panel is not answering, and events
    /// derived from it would be phantoms.
    Silent,
}

impl Xpt2046Touch {
    pub fn new(profile: BoardProfile) -> Self {
        let pins = profile.touch;
        gpio::enable_output(pins.clk);
        gpio::enable_output(pins.mosi);
        gpio::enable_output(pins.cs);
        gpio::enable_input(pins.miso);
        gpio::enable_input_pullup(pins.irq);
        // XPT2046 uses SPI mode 0, so the first command bit must begin with
        // a real low-to-high clock edge.
        gpio::set_output_low(pins.clk);
        gpio::set_output_high(pins.cs);
        gpio::set_output_high(pins.mosi);
        Self {
            clk: pins.clk,
            mosi: pins.mosi,
            miso: pins.miso,
            cs: pins.cs,
            irq: pins.irq,
            // Measured, not guessed. Four corners pressed in turn, with the
            // dense burst reading each one, gave: top-left (0, 0),
            // bottom-right (3305, 3411), and the two others on the same rails.
            // The direct mapping - raw increases right and down, no mirror, no
            // swap - puts top-left at screen (0, 0) and bottom-right at the
            // opposite corner, which is the check that matters.
            calibration: Calibration {
                min_x: 0,
                max_x: 3_305,
                min_y: 0,
                max_y: 3_411,
                mirror_x: false,
                mirror_y: false,
                swap_xy: false,
            },
            tracker: PointerTracker::new(PointerConfig::resistive_default()),
            samples_taken: 0,
            last_sample_ms: 0,
            last_raw: (0, 0),
            last_axes: (0, 0),
            last_quad: (0, 0, 0, 0),
            last_pressure: 0,
            spread_rejects: 0,
            channel: TouchChannel::Unknown,
        }
    }

    pub fn set_calibration(&mut self, calibration: Calibration) {
        self.calibration = calibration;
    }

    pub fn tracker(&mut self) -> &mut PointerTracker {
        &mut self.tracker
    }

    pub fn samples_taken(&self) -> u32 {
        self.samples_taken
    }

    /// Raw panel coordinates and pressure from the most recent conversion.
    pub fn last_raw(&self) -> (u16, u16) {
        self.last_raw
    }

    /// The X/Y conversions on their own, without the pressure gate.
    ///
    /// This is what the self-test and the calibration output need: the pressure
    /// estimate decides whether a finger is down, but the axes are what say
    /// whether the controller is converting anything at all.
    pub fn read_axes(&mut self) -> (u16, u16) {
        self.samples_taken = self.samples_taken.saturating_add(1);
        self.select();
        let x_first = self.read_axis(X_COMMAND);
        let x_second = self.read_axis(X_COMMAND);
        let y_first = self.read_axis(Y_COMMAND);
        let y_second = self.read_axis(Y_COMMAND);
        let _ = self.read_adc(0xd0);
        let _ = self.read_adc(0x00);
        self.deselect();
        let x = ((u32::from(x_first) + u32::from(x_second)) / 2) as u16;
        let y = ((u32::from(y_first) + u32::from(y_second)) / 2) as u16;
        self.last_axes = (x, y);
        self.last_raw = (x, y);
        (x, y)
    }

    pub fn last_pressure(&self) -> u16 {
        self.last_pressure
    }

    /// Decide whether the panel is answering at all.
    ///
    /// With nothing touching the panel every real controller still returns
    /// slightly different conversions. A board whose touch channel is not
    /// speaking returns the same numbers forever while the pressure estimate
    /// still claims a press, and every event built from that is a phantom: the
    /// shell sees a storm of taps nobody made. This samples the idle panel a
    /// few times at boot and records whether the readings ever move.
    pub fn detect_channel(&mut self) -> TouchChannel {
        let mut readings: [(u16, u16); 8] = [(0, 0); 8];
        let mut seen = 0usize;
        for slot in readings.iter_mut() {
            let _ = self.read_axes();
            *slot = self.last_axes;
            seen = seen.saturating_add(1);
        }
        let mut distinct = 0u32;
        for index in 1..seen {
            if readings[index] != readings[0] {
                distinct = distinct.saturating_add(1);
            }
        }
        // A controller that is converting a real input produces different
        // numbers from one conversion to the next. One that is not answering
        // produces the same number forever.
        self.channel = if distinct == 0 {
            TouchChannel::Silent
        } else {
            TouchChannel::Reporting
        };
        self.channel
    }

    pub fn channel(&self) -> TouchChannel {
        self.channel
    }

    /// Axis conversions from the most recent read, pressure gate or not.
    pub fn last_axes(&self) -> (u16, u16) {
        self.last_axes
    }

    /// The four conversions behind the last sample.
    pub fn last_quad(&self) -> (u16, u16, u16, u16) {
        self.last_quad
    }

    /// Read one channel eight times in a row and report each value.
    ///
    /// A single pair cannot show how many conversions a channel needs before
    /// its reading is the channel's own. Eight in a row does: the first value
    /// after a switch belongs to the previous channel, and the rest reveal
    /// whether the readings have settled or are still moving.
    pub fn scan_repeat(&mut self, command: u8, report: &mut dyn FnMut(u16)) {
        self.select();
        for _ in 0..8 {
            report(self.read_adc(command));
        }
        self.deselect();
    }

    /// Read every candidate control byte twice and report the pairs.
    ///
    /// Two conversions per channel, because the first one after a channel
    /// change belongs to the previous channel.
    pub fn scan_channels(&mut self, report: &mut dyn FnMut(u8, u16, u16)) {
        self.select();
        for command in SCAN_COMMANDS {
            let first = self.read_adc(command);
            let second = self.read_adc(command);
            report(command, first, second);
        }
        self.deselect();
    }

    /// Sample the panel back to back for a fixed window, reporting pressed
    /// readings.
    ///
    /// Calibration needs density: the render loop's cadence is set by the
    /// display, and a handful of readings cannot fix four corners. This runs
    /// the conversions flat out instead, which is also the honest way to see
    /// how fast the panel can be read at all.
    pub fn calibration_burst(
        &mut self,
        window_us: u32,
        min_gap_us: u32,
        now: &mut dyn FnMut() -> u32,
        report: &mut dyn FnMut(u16, u16, u16),
    ) -> u32 {
        let started = now();
        let mut next_report = started;
        let mut samples = 0u32;
        while now().wrapping_sub(started) < window_us {
            // Report the read that was just taken, not a stored snapshot: the
            // snapshot is one refactor away from reporting the wrong thing.
            let reading = self.read();
            let stamp = now();
            if self.last_pressure >= PRESSURE_FLOOR
                && stamp.wrapping_sub(next_report) >= min_gap_us
            {
                if let Some(sample) = reading {
                    next_report = stamp;
                    report(sample.x, sample.y, self.last_pressure);
                }
            }
            samples = samples.saturating_add(1);
        }
        samples
    }

    /// Whether the tracker currently reports contact.
    pub fn is_contact(&self) -> bool {
        self.tracker.is_pressed()
    }

    /// How many readings were thrown away for inconsistent duplicate
    /// conversions. A number that climbs while nobody is touching the panel is
    /// the panel telling you it is not answering.
    pub fn spread_rejects(&self) -> u32 {
        self.spread_rejects
    }

    /// Cheap liveness test on the pen line.
    ///
    /// PENIRQ is active low and open-drain on this controller, so a single
    /// pad read answers "is a finger down right now" without starting a
    /// conversion. The display flush uses this to decide whether a full
    /// sample is worth taking between chunks: idle frames pay one register
    /// read per chunk instead of a full conversion sequence.
    pub fn pen_active(&self) -> bool {
        gpio::input(self.irq) == Some(false)
    }

    /// One device sample, mapped into surface coordinates.
    ///
    /// A released reading keeps `pressed = false` and its coordinates are
    /// meaningless, so they are reported as zero.
    pub fn sample(&mut self) -> PointerSample {
        self.samples_taken = self.samples_taken.saturating_add(1);
        let Some(raw) = self.read() else {
            return PointerSample::released();
        };
        if !raw.pressed {
            return PointerSample::released();
        }
        let (x, y) = self
            .calibration
            .map_to_screen(raw, DISPLAY_WIDTH, DISPLAY_HEIGHT);
        PointerSample {
            x,
            y,
            pressed: true,
        }
    }

    /// Read the panel once and return the event, if any, that it completed.
    pub fn poll(&mut self, now_ms: u32) -> Option<PointerEvent> {
        let sample = self.sample();
        self.tracker.update(sample, now_ms)
    }

    /// Sample only when the pen line asks for it, or when the idle interval
    /// has elapsed.
    ///
    /// `pen_active` is a cheap pad read and stays the fast path; the interval
    /// is the fallback for a board without the external pull-up.
    pub fn poll_due(&mut self, now_ms: u32) -> Option<PointerEvent> {
        if !self.pen_active() && now_ms.saturating_sub(self.last_sample_ms) < IDLE_SAMPLE_INTERVAL_MS
        {
            return None;
        }
        self.last_sample_ms = now_ms;
        self.poll(now_ms)
    }

    /// Read the touch controller once.
    ///
    /// The conversion always runs. An earlier version returned early when
    /// PENIRQ read high, which made the shell's reaction depend on a pad that
    /// this board needs an external pull-up for: with the line floating the
    /// axes were never converted at all, and the panel looked silent when it
    /// was merely unasked. `pen_active` remains available as a cheap hint for
    /// how often to sample.
    pub fn read(&mut self) -> Option<TouchSample> {
        self.select();
        let z1 = self.read_adc(Z1_COMMAND);
        let z2 = self.read_adc(Z2_COMMAND);
        // z1 grows relative to z2 as the plates are pressed together. This is
        // the discriminating quantity; see PRESSURE_FLOOR.
        let pressure = z1.saturating_sub(z2);
        self.last_pressure = pressure;
        if pressure < PRESSURE_FLOOR {
            self.deselect();
            return Some(TouchSample {
                x: 0,
                y: 0,
                pressed: false,
            });
        }

        // Axis conversions: each is preceded by its own settling conversion.
        // An earlier version averaged an X reading with a Y reading here, which
        // is why the axes never moved while the pressure did.
        let x_first = self.read_axis(X_COMMAND);
        let x_second = self.read_axis(X_COMMAND);
        let y_first = self.read_axis(Y_COMMAND);
        let y_second = self.read_axis(Y_COMMAND);
        self.last_quad = (x_first, x_second, y_first, y_second);
        let _ = self.read_adc(0xd0); // power down
        let _ = self.read_adc(0x00);
        self.deselect();

        // Reject a reading whose duplicate conversions disagree: that is the
        // signature of a panel that is not answering, not of a finger.
        if x_first.abs_diff(x_second) > AXIS_SPREAD_LIMIT
            || y_first.abs_diff(y_second) > AXIS_SPREAD_LIMIT
        {
            self.spread_rejects = self.spread_rejects.saturating_add(1);
            return Some(TouchSample {
                x: 0,
                y: 0,
                pressed: false,
            });
        }
        let x = ((u32::from(x_first) + u32::from(x_second)) / 2) as u16;
        let y = ((u32::from(y_first) + u32::from(y_second)) / 2) as u16;
        self.last_raw = (x, y);
        // Keep the axis snapshot in step as well: the calibration burst reads
        // this field, and leaving it to `read_axes` alone meant the burst
        // reported whatever the boot self-test had left there.
        self.last_axes = (x, y);
        Some(TouchSample {
            x,
            y,
            pressed: true,
        })
    }

    pub fn map_to_screen(&self, sample: TouchSample, width: u16, height: u16) -> (u16, u16) {
        self.calibration.map_to_screen(sample, width, height)
    }

    /// One axis, read correctly.
    ///
    /// The conversion that follows a channel change belongs to the *previous*
    /// channel: the sample-and-hold settles during it. Reading X and Y
    /// alternately therefore mixes the two, and the first X conversion after
    /// the pressure sequence is likewise not X at all. Two consecutive
    /// conversions of the same channel, with the first discarded, is what the
    /// controller's own timing expects.
    fn read_axis(&mut self, command: u8) -> u16 {
        let _ = self.read_adc(command);
        self.read_adc(command)
    }

    fn read_adc(&mut self, command: u8) -> u16 {
        self.shift_byte(command);
        // The conversion frame after the command byte is sixteen clocks: one
        // leading zero, the twelve result bits, then three trailing zeros. It
        // is read as a word and shifted down by three, which is what the
        // ADS7846-family drivers do and what the measurement agrees with: with
        // only twelve clocks taken straight after the command the panel centre
        // read 1024 where mid-scale is 2048, exactly one bit short.
        let mut word = 0u16;
        for _ in 0..16 {
            gpio::set_output_high(self.clk);
            clock_delay();
            word = (word << 1) | u16::from(gpio::input(self.miso) == Some(true));
            gpio::set_output_low(self.clk);
            clock_delay();
        }
        word >> 3
    }

    fn select(&mut self) {
        gpio::set_output_low(self.cs);
    }

    fn deselect(&mut self) {
        gpio::set_output_high(self.cs);
    }

    fn shift_byte(&mut self, byte: u8) -> u8 {
        let mut value = 0;
        for bit in (0..8).rev() {
            if byte & (1 << bit) != 0 {
                gpio::set_output_high(self.mosi);
            } else {
                gpio::set_output_low(self.mosi);
            }
            clock_delay();
            gpio::set_output_high(self.clk);
            clock_delay();
            value <<= 1;
            if gpio::input(self.miso) == Some(true) {
                value |= 1;
            }
            gpio::set_output_low(self.clk);
            clock_delay();
        }
        value
    }
}

/// Keep the software SPI clock inside the XPT2046's timing budget.
///
/// The controller is specified for a maximum clock around 2 MHz. Twenty-four
/// spin iterations put the half-period at roughly 0.3-0.5 us on this core,
/// which is about 1-1.6 MHz: inside the limit with margin, and fast enough
/// that a sample costs a fraction of a millisecond. Eighty iterations, tried
/// while chasing reliability, made one sample cost over a millisecond and the
/// panel visibly lagged the finger - the reliability was already there once
/// the command bytes and the sixteen-clock frame were right.
#[inline(never)]
fn clock_delay() {
    for _ in 0..24 {
        core::hint::spin_loop();
    }
}

/// Longest gap between samples when the pen line says nothing is down.
///
/// PENIRQ needs an external pull-up on this board and floats when it is
/// missing, so it is treated as a hint and never as the only reason to look at
/// the panel. Without this floor a missing pull-up means touch never works at
/// all.
pub const IDLE_SAMPLE_INTERVAL_MS: u32 = 4;

/// How often the shell prints raw readings while calibrating.
pub const RAW_REPORT_INTERVAL_US: u32 = 100_000;
