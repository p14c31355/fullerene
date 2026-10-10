//! Kernel startup and the fixed ESP32 application set.

use crate::arch::xtensa::esp32::{board, memory, scheduler::Scheduler, time};
use nozzle::arch::xtensa::esp32::commands as nozzle_commands;

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use lattice::arch::xtensa::esp32::{
    EmbeddedApp, EmbeddedDesktop, Esp32Compositor, POINTER_MARKER, WINDOW_RECT,
};
use nitrogen::arch::xtensa::esp32::{
    board::BoardProfile,
    display::{FULL_FRAME, LcdRect, SpiLcd},
    touchscreen::{TouchChannel, Xpt2046Touch},
    uart,
};
use nitrogen::geometry::Rect;
use nitrogen::input::{PointerEvent, PointerPhase, PointerQueue};
use spin::Mutex;

/// Microseconds since boot, from the timer's 1 MHz count.
fn now_us() -> u32 {
    time::uptime_micros() as u32
}

/// Cross-check the clocks against a physical reference.
///
/// Two clocks disagreeing tells you only that they disagree. The UART is the
/// trustworthy one: its baud rate comes from the APB clock through a divider
/// with an error of a few parts per million, so a known number of bytes takes a
/// known time. Timing that same burst on both the timer and the cycle counter
/// yields each one's real rate.
fn report_uart_reference_probe() {
    /// 64 characters; repeated to make the burst long enough to time.
    const RULER: &str = "................................................................";
    const REPEATS: u32 = 16;
    // 115200 baud, 10 bit-times per byte (8N1).
    const MICROS_PER_BYTE: u32 = 10_000_000 / 115_200;
    let bytes = RULER.len() as u32 * REPEATS;
    let expected_us = bytes * MICROS_PER_BYTE;

    let uart = nitrogen::arch::xtensa::esp32::uart::write_str;
    let number = nitrogen::arch::xtensa::esp32::uart::write_u32;

    let start_us = time::uptime_micros();
    let start_cycles = time::cycles();
    uart("UART RULER START\n");
    for _ in 0..REPEATS {
        uart(RULER);
    }
    let cycles = time::cycles().wrapping_sub(start_cycles);
    let timer_us = (time::uptime_micros() - start_us) as u32;
    uart("\n");

    let timer_ratio_permille = if expected_us == 0 {
        0
    } else {
        timer_us / (expected_us / 1_000).max(1)
    };
    let cpu_mhz = if timer_us == 0 {
        0
    } else {
        cycles / timer_us
    };

    uart("UART RULER bytes=");
    number(bytes);
    uart(" expected_us=");
    number(expected_us);
    uart(" timer_us=");
    number(timer_us);
    uart(" cycles=");
    number(cycles);
    uart(" timer_ratio_permille=");
    number(timer_ratio_permille);
    uart(" cpu_mhz=");
    number(cpu_mhz);
    uart("\n");
    MEASURED_CPU_MHZ.store(cpu_mhz, Ordering::Relaxed);
}

static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);
static DISPLAY: Mutex<Option<SpiLcd>> = Mutex::new(None);

/// Events produced while a flush was in flight wait here for the shell.
const INPUT_QUEUE_EVENTS: usize = 16;

const POINTER_DOWN_COLOR: u16 = 0xf800;
const POINTER_MOVE_COLOR: u16 = 0xfd20;
const POINTER_UP_COLOR: u16 = 0x07e0;

/// Report what the timer peripheral actually answers.
///
/// Printed as plain decimal so the whole check works without the allocator:
/// 4660 and 22136 are the patterns written to the load and alarm registers, so
/// seeing them come back means the bus clock is open and the block is
/// reachable.
fn report_time_probe() {
    let probe = time::probe();
    let uart = nitrogen::arch::xtensa::esp32::uart::write_str;
    let number = nitrogen::arch::xtensa::esp32::uart::write_u32;
    uart("TIMER PROBE load_lo=");
    number(probe.load_lo);
    uart(" alarm_lo=");
    number(probe.alarm_lo);
    uart(" config=");
    number(probe.config);
    uart(" count_a=");
    number(probe.count_a);
    uart(" count_b=");
    number(probe.count_b);
    uart(" dport_clk_en=");
    number(probe.perip_clk_en);
    uart("\n");
    uart(if probe.registers_respond() {
        "TIMER PROBE registers=ok\n"
    } else {
        "TIMER PROBE registers=UNREACHABLE\n"
    });
    uart(if probe.counter_advances() {
        "TIMER PROBE counter=advancing\n"
    } else {
        "TIMER PROBE counter=STUCK\n"
    });
}

pub fn boot() -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("FULLERENE BOOT\n");

    let _profile = BoardProfile::xh32s();
    nitrogen::arch::xtensa::esp32::uart::write_str("LCD INIT\n");
    let mut lcd = SpiLcd::new();
    if lcd.init().is_err() {
        crate::arch::xtensa::esp32::runtime::panic_message("LCD initialization failed");
    }
    // Deliberately allocation-free: the heap is not proven ready at this
    // point, and the boot markers must not depend on it.
    nitrogen::arch::xtensa::esp32::uart::write_str("LCD OK\n");
    // Paint a known pattern and leave it up briefly. A lit but blank panel and
    // a panel that simply has not been written yet look identical otherwise,
    // and this is the only way to tell them apart without a scope.
    match lcd.draw_test_pattern() {
        Ok(()) => nitrogen::arch::xtensa::esp32::uart::write_str("PANEL TEST BARS\n"),
        Err(_) => nitrogen::arch::xtensa::esp32::uart::write_str("PANEL TEST FAILED\n"),
    }
    for _ in 0..400_000 {
        core::hint::spin_loop();
    }
    *DISPLAY.lock() = Some(lcd);
    // Pin 21 is tied to the display backlight on the profile board.
    nitrogen::arch::xtensa::esp32::gpio::enable_output(board::LCD_BACKLIGHT);
    nitrogen::arch::xtensa::esp32::gpio::set_output_high(board::LCD_BACKLIGHT);
    nitrogen::arch::xtensa::esp32::uart::write_str("BACKLIGHT OK\n");

    time::init();
    report_time_probe();
    TIMER_COUNTS.store(time::probe().counter_advances(), Ordering::Relaxed);
    report_uart_reference_probe();
    nitrogen::arch::xtensa::esp32::uart::write_str("TIME OK\n");
    let mut scheduler = Scheduler::new();
    unsafe {
        nozzle_commands::MEMINFO = Some(|| (memory::used(), memory::capacity()));
        nozzle_commands::TASK_NAMES = Some(|| fixed_task_names());
        nozzle_commands::UPTIME = Some(time::uptime_millis);
        nozzle_commands::REBOOT = Some(reboot);
        nozzle_commands::LATENCY = Some(latency_report);
        nozzle_commands::TOUCH_SCAN = Some(request_touch_scan);
        nozzle_commands::TOUCH_CAL = Some(request_touch_cal);
    }
    if scheduler
        .spawn("lattice", lattice_task as *const () as usize, 8 * 1024)
        .is_none()
    {
        panic_message("lattice task spawn failed");
    }
    if scheduler
        .spawn("nozzle", nozzle_task as *const () as usize, 4 * 1024)
        .is_none()
    {
        panic_message("nozzle task spawn failed");
    }
    for (name, entry) in solvent::arch::xtensa::esp32::applications::application_tasks() {
        // Zero entries are explicit unlaunched registrations, not executable
        // tasks. Do not send them into the unfinished context switch.
        if entry != 0 {
            scheduler.spawn(name, entry, 3 * 1024);
        }
    }
    nitrogen::arch::xtensa::esp32::uart::write_str("SCHED RUN\n");
    scheduler.run();
}

fn reboot() -> bool {
    crate::arch::xtensa::esp32::platform::software_reset()
}

/// Convert every candidate touch channel and print the pairs.
///
/// This is the experiment that separates a wrong control byte from a plate with
/// no signal: with a finger held on the panel, the channel that works moves,
/// and every other one stays where it was.
fn report_touch_scan(touch: &mut Xpt2046Touch) {
    let uart = nitrogen::arch::xtensa::esp32::uart::write_str;
    uart("TOUCH SCAN BEGIN\n");
    let mut lines = String::new();
    touch.scan_channels(&mut |command, first, second| {
        let _ = core::fmt::Write::write_fmt(
            &mut lines,
            format_args!(
                "TOUCH SCAN cmd={} first={} second={}\n",
                command, first, second
            ),
        );
        if lines.len() > 256 {
            uart(&lines);
            lines.clear();
        }
    });
    uart(&lines);
    for (label, command) in [("X", nitrogen::arch::xtensa::esp32::touchscreen::X_COMMAND),
                             ("Y", nitrogen::arch::xtensa::esp32::touchscreen::Y_COMMAND)] {
        let mut repeated = String::new();
        let mut values = String::new();
        touch.scan_repeat(command, &mut |value| {
            let _ = core::fmt::Write::write_fmt(&mut values, format_args!(" {}", value));
        });
        let _ = core::fmt::Write::write_fmt(
            &mut repeated,
            format_args!("TOUCH SCAN repeat {} ({}){}", label, command, values),
        );
        uart(&repeated);
        uart("\n");
    }
    let axes = touch.last_axes();
    let mut tail = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut tail,
        format_args!(
            "TOUCH SCAN END axes={},{} p={} pen={}\n",
            axes.0,
            axes.1,
            touch.last_pressure(),
            u8::from(touch.pen_active())
        ),
    );
    uart(&tail);
}

/// Input that keeps running while the display transfers.
///
/// The panel driver calls [`InputPump::service`] between SPI chunks. With the
/// pen line idle that costs one pad read; with a finger down it takes a real
/// sample and queues the event. Either way the sampling rate is set by the
/// chunk size, not by how long a frame takes to shift out.
struct InputPump {
    touch: Xpt2046Touch,
    queue: PointerQueue<INPUT_QUEUE_EVENTS>,
    pen_seen: u32,
}

impl InputPump {
    fn new(profile: BoardProfile) -> Self {
        let mut touch = Xpt2046Touch::new(profile);
        // Ask the panel whether it is answering before trusting anything it
        // says. A silent channel otherwise produces a storm of phantom taps.
        let channel = touch.detect_channel();
        let uart = nitrogen::arch::xtensa::esp32::uart::write_str;
        let number = nitrogen::arch::xtensa::esp32::uart::write_u32;
        match channel {
            TouchChannel::Silent => {
                uart("TOUCH SELFTEST SILENT raw=");
                let (raw, pressure) = (touch.last_raw(), touch.last_pressure());
                number(u32::from(raw.0));
                uart(",");
                number(u32::from(raw.1));
                uart(" p=");
                number(u32::from(pressure));
                uart(" events=suppressed\n");
            }
            _ => uart("TOUCH SELFTEST REPORTING\n"),
        }
        Self {
            touch,
            queue: PointerQueue::new(),
            pen_seen: 0,
        }
    }

    /// Whether events from this panel may be believed.
    fn trusted(&self) -> bool {
        self.touch.channel() != TouchChannel::Silent
    }

    /// Called between display chunks. Returns whether the pen was down.
    ///
    /// PENIRQ is a hint, not a gate: this board needs an external pull-up for
    /// it, so the idle sampling floor in `poll_due` keeps the panel observable
    /// even when the line floats.
    fn service(&mut self, now_ms: u32) -> bool {
        let pen = self.touch.pen_active();
        if pen {
            self.pen_seen = self.pen_seen.saturating_add(1);
        }
        if let Some(event) = self.touch.poll_due(now_ms) {
            self.queue.push(event);
        }
        pen
    }

    /// Take the next event, sampling directly when the pacer queued nothing.
    fn poll(&mut self, now_ms: u32) -> Option<PointerEvent> {
        if let Some(event) = self.queue.pop() {
            return Some(event);
        }
        self.service(now_ms);
        self.queue.pop()
    }

    /// Raw panel readings from the last conversion, for calibration output.
    fn raw(&self) -> ((u16, u16), u16) {
        (self.touch.last_raw(), self.touch.last_pressure())
    }

    fn samples(&self) -> u32 {
        self.touch.samples_taken()
    }
}

/// Rolling measurements from the drawing-latency probe.
#[derive(Clone, Copy, Debug)]
struct Latency {
    probes: u32,
    fill_us: u32,
    full_flush_us: u32,
    window_flush_us: u32,
    marker_flush_us: u32,
    window_bytes: u32,
    full_bytes: u32,
    marker_bytes: u32,
    pace_calls: u32,
    samples_during_flush: u32,
    tap_to_flush_us: u32,
    worst_tap_to_flush_us: u32,
}

impl Latency {
    const fn empty() -> Self {
        Self {
            probes: 0,
            fill_us: 0,
            full_flush_us: 0,
            window_flush_us: 0,
            marker_flush_us: 0,
            window_bytes: 0,
            full_bytes: 0,
            marker_bytes: 0,
            pace_calls: 0,
            samples_during_flush: 0,
            tap_to_flush_us: 0,
            worst_tap_to_flush_us: 0,
        }
    }
}

static LATENCY: Mutex<Latency> = Mutex::new(Latency::empty());

/// The CPU clock the UART-ruler probe measured, in MHz.
static MEASURED_CPU_MHZ: AtomicU32 = AtomicU32::new(0);

/// Whether the timer peripheral's count advances, recorded once at boot.
static TIMER_COUNTS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn note_latency(update: impl FnOnce(&mut Latency)) {
    update(&mut LATENCY.lock());
}

/// The most recent drawing-latency measurements, one metric per line.
pub fn latency_report() -> String {
    let latency = *LATENCY.lock();
    let mut report = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "drawing latency ({} probe{}):\n",
            latency.probes,
            if latency.probes == 1 { "" } else { "s" }
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  spi_hz={} cpu_mhz_measured={} timer_counter={} clock=1us_timer\n",
            current_spi_hz(),
            MEASURED_CPU_MHZ.load(Ordering::Relaxed),
            if TIMER_COUNTS.load(Ordering::Relaxed) {
                "advancing"
            } else {
                "STUCK"
            },
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  surface_fill_us={} ({}x{} px)\n",
            latency.fill_us, WINDOW_RECT.width, WINDOW_RECT.height
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  full_flush_us={} bytes={}\n",
            latency.full_flush_us, latency.full_bytes
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  window_flush_us={} bytes={}\n",
            latency.window_flush_us, latency.window_bytes
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  marker_flush_us={} bytes={}\n",
            latency.marker_flush_us, latency.marker_bytes
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  pace_calls_per_full_frame={} input_samples_during_flush={}\n",
            latency.pace_calls, latency.samples_during_flush
        ),
    );
    let _ = core::fmt::Write::write_fmt(
        &mut report,
        format_args!(
            "  tap_to_flush_us={} worst={}\n",
            latency.tap_to_flush_us, latency.worst_tap_to_flush_us
        ),
    );
    report
}

fn current_spi_hz() -> u32 {
    DISPLAY
        .lock()
        .as_ref()
        .map_or(0, |display| display.clock_hz())
}

/// Flush one rectangle, keeping input alive between SPI chunks.
///
/// Returns the microseconds the transfer took and how many times the input
/// pacer ran, so the caller can report both without a second pass.
fn flush_region(
    pixels: &[u16],
    rect: Rect,
    pump: &mut InputPump,
    count_pace: bool,
) -> (u32, u32) {
    let mut pace_calls = 0u32;
    let mut display = DISPLAY.lock();
    let Some(lcd) = display.as_mut() else {
        return (0, 0);
    };
    let started = now_us();
    let mut pace = || {
        pace_calls = pace_calls.saturating_add(1);
        pump.service(now_us() / 1_000);
    };
    let result = lcd.flush_paced(
        pixels,
        LcdRect::new(rect.x, rect.y, rect.width, rect.height),
        &mut pace,
    );
    let elapsed = now_us().wrapping_sub(started);
    drop(display);
    if result.is_err() {
        panic_message("LCD flush failed");
    }
    if !count_pace {
        pace_calls = 0;
    }
    (elapsed, pace_calls)
}

/// Measure the drawing path end to end: surface fill, full frame, window-only
/// redraw, and the touch marker, plus how much input ran during the transfer.
///
/// Every number is measured on the device with the corrected microsecond
/// timebase; nothing here is estimated from the clock rate.
fn drawing_latency_probe(
    lcd_present: bool,
    surface: &mut Esp32Compositor,
    desktop: &mut EmbeddedDesktop,
    pump: &mut InputPump,
) {
    if !lcd_present || surface.pixels().is_empty() {
        return;
    }

    // 1. Surface fill only: no transfer, so this is the CPU cost of painting.
    let fill_started = now_us();
    let _ = surface.fill(
        WINDOW_RECT.x,
        WINDOW_RECT.y,
        WINDOW_RECT.width,
        WINDOW_RECT.height,
        0x001f,
    );
    let fill_us = now_us().wrapping_sub(fill_started);

    // 2. A whole frame, the worst case the old path paid on every redraw.
    let _ = surface.take_dirty();
    surface.mark_dirty(FULL_FRAME);
    let full_bytes = FULL_FRAME.rgb565_bytes();
    let samples_before = pump.samples();
    let (full_flush_us, pace_calls) = flush_region(surface.pixels(), FULL_FRAME, pump, true);
    let samples_during_flush = pump.samples().saturating_sub(samples_before);

    // 3. The dirty-region path: switch application and transfer only the window.
    let _ = surface.take_dirty();
    let _ = desktop.set_active(EmbeddedApp::Files);
    desktop.render_window(surface);
    let window_bytes = WINDOW_RECT.rgb565_bytes();
    let (window_flush_us, _) = match surface.take_dirty() {
        Some(dirty) => flush_region(surface.pixels(), dirty, pump, false),
        None => (0, 0),
    };

    // 4. A pointer marker: the smallest transfer the shell produces.
    let _ = surface.take_dirty();
    let marker = desktop
        .draw_pointer(surface, 160, 120, 0xf800)
        .unwrap_or(Rect::new(0, 0, POINTER_MARKER, POINTER_MARKER));
    let marker_bytes = marker.rgb565_bytes();
    let (marker_flush_us, _) = flush_region(surface.pixels(), marker, pump, false);

    // 5. Redraw the desktop so the panel is left in a coherent state.
    let _ = surface.take_dirty();
    desktop.render(surface);
    if let Some(dirty) = surface.take_dirty() {
        let _ = flush_region(surface.pixels(), dirty, pump, false);
    }

    note_latency(|latency| {
        latency.probes = latency.probes.saturating_add(1);
        latency.fill_us = fill_us;
        latency.full_flush_us = full_flush_us;
        latency.window_flush_us = window_flush_us;
        latency.marker_flush_us = marker_flush_us;
        latency.full_bytes = full_bytes;
        latency.window_bytes = window_bytes;
        latency.marker_bytes = marker_bytes;
        latency.pace_calls = pace_calls;
        latency.samples_during_flush = samples_during_flush;
    });
    uart::write_str(&latency_report());
}

extern "C" fn lattice_task() -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("LATTICE START\n");
    let profile = BoardProfile::xh32s();
    let mut desktop = EmbeddedDesktop::new();
    let mut surface = Esp32Compositor::new(320, 240);
    if !surface.allocate() {
        crate::arch::xtensa::esp32::runtime::panic_message("display surface allocation failed");
    }
    let mut pump = InputPump::new(profile);
    let mut redraw_window = false;

    // Present the first frame, then measure the drawing path once so the
    // numbers are visible over UART without anyone typing a command.
    desktop.render(&mut surface);
    if let Some(dirty) = surface.take_dirty() {
        let _ = flush_region(surface.pixels(), dirty, &mut pump, false);
    }
    drawing_latency_probe(true, &mut surface, &mut desktop, &mut pump);

    loop {
        let now_ms = now_us() / 1_000;

        // A silent channel still prints raw readings (that is the evidence),
        // but it must not drive the shell.
        if !pump.trusted() {
            pump.poll(now_ms);
        } else if let Some(event) = pump.poll(now_ms) {
            let event_started = now_us();
            handle_pointer(
                event,
                &mut desktop,
                &mut surface,
                &mut redraw_window,
                event_started,
            );
        }

        if TOUCH_SCAN_REQUESTED.swap(false, Ordering::Relaxed) {
            report_touch_scan(&mut pump.touch);
        }
        if TOUCH_CAL_REQUESTED.swap(false, Ordering::Relaxed) {
            run_calibration_burst(&mut pump.touch);
        }

        // Calibration evidence, printed whether or not a press is believed:
        // the numbers a calibration has to be derived from are these, and
        // seeing the idle baseline next to a finger's readings is what says
        // whether the panel is answering and where it thinks the finger is.
        let now = now_us();
        if now.wrapping_sub(RAW_PRINT_AT.load(Ordering::Relaxed)) > 100_000 {
            RAW_PRINT_AT.store(now, Ordering::Relaxed);
            let axes = pump.touch.last_axes();
            let pressure = pump.touch.last_pressure();
            let mut line = String::new();
            let _ = core::fmt::Write::write_fmt(
                &mut line,
                format_args!(
                    "TOUCH AXES x={} y={} p={} pen={} chan={} rejects={} quad={},{},{},{}\n",
                    axes.0,
                    axes.1,
                    pressure,
                    u8::from(pump.touch.pen_active()),
                    match pump.touch.channel() {
                        TouchChannel::Reporting => "reporting",
                        TouchChannel::Silent => "silent",
                        TouchChannel::Unknown => "unknown",
                    },
                    pump.touch.spread_rejects(),
                    pump.touch.last_quad().0,
                    pump.touch.last_quad().1,
                    pump.touch.last_quad().2,
                    pump.touch.last_quad().3
                ),
            );
            uart::write_str(&line);
        }

        if redraw_window {
            let _ = surface.take_dirty();
            desktop.render_window(&mut surface);
            redraw_window = false;
        }

        if let Some(dirty) = surface.take_dirty() {
            let (flush_us, _) = flush_region(surface.pixels(), dirty, &mut pump, false);
            // A zero deadline means no press is waiting to be shown.
            let tap_started = PENDING_TAP_US.swap(0, Ordering::Relaxed);
            if tap_started != 0 {
                let delta = now_us().wrapping_sub(tap_started);
                note_latency(|latency| {
                    latency.tap_to_flush_us = delta;
                    latency.worst_tap_to_flush_us = latency.worst_tap_to_flush_us.max(delta);
                });
            }
            let _ = flush_us;
        }

        crate::arch::xtensa::esp32::scheduler::scheduler_yield();
    }
}

/// Microsecond timestamp of the last Down event still waiting to be painted.
static PENDING_TAP_US: AtomicU32 = AtomicU32::new(0);

/// When the raw touch values were last printed.
static RAW_PRINT_AT: AtomicU32 = AtomicU32::new(0);

/// Set by the `touchscan` shell command; served by the display task.
static TOUCH_SCAN_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Raise a touch channel scan request from the shell.
fn request_touch_scan() {
    TOUCH_SCAN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Set by the `touchcal` shell command; served by the display task.
static TOUCH_CAL_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Raise a calibration burst request from the shell.
fn request_touch_cal() {
    TOUCH_CAL_REQUESTED.store(true, Ordering::Relaxed);
}

/// Stream pressed readings as fast as the panel allows.
///
/// The window and the reporting gap are both fixed: run it with a finger held
/// still on the spot being calibrated and the readings for that spot arrive as
/// a dense cluster instead of the four or five the render loop manages.
fn run_calibration_burst(touch: &mut Xpt2046Touch) {
    let uart = nitrogen::arch::xtensa::esp32::uart::write_str;
    uart("TOUCH CAL BEGIN\n");
    let mut lines = String::new();
    let mut now = now_us;
    let samples = touch.calibration_burst(
        4_000_000,
        20_000,
        &mut now,
        &mut |x, y, pressure| {
            let _ = core::fmt::Write::write_fmt(
                &mut lines,
                format_args!("TOUCH CAL x={} y={} p={}\n", x, y, pressure),
            );
            if lines.len() > 512 {
                uart(&lines);
                lines.clear();
            }
        },
    );
    uart(&lines);
    let mut tail = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut tail,
        format_args!("TOUCH CAL END samples={}\n", samples),
    );
    uart(&tail);
}

/// Draw the contact marker where the finger actually is.
///
/// The marker sits at the touch point rather than in a fixed corner because
/// that is the only way to see whether the mapping is right: press a spot, and
/// the square appears there or it does not. Moving leaves a trail, which is
/// acceptable for a bring-up view and is the next thing to replace with a
/// marker that repaints underneath itself.
fn draw_touch_marker(surface: &mut Esp32Compositor, x: u16, y: u16, color: u16) {
    let half = POINTER_MARKER / 2;
    let _ = surface.fill(
        x.saturating_sub(half),
        y.saturating_sub(half),
        POINTER_MARKER,
        POINTER_MARKER,
        color,
    );
}

fn handle_pointer(
    event: PointerEvent,
    desktop: &mut EmbeddedDesktop,
    surface: &mut Esp32Compositor,
    redraw_window: &mut bool,
    event_started_us: u32,
) {
    let mut line = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!(
            "TOUCH {} us={} at={},{}\n",
            event.phase.name(),
            event_started_us,
            event.x,
            event.y
        ),
    );
    uart::write_str(&line);

    match event.phase {
        PointerPhase::Down => {
            // The tap-to-flush measurement starts here and is closed out when
            // the resulting transfer completes.
            PENDING_TAP_US.store(event_started_us, Ordering::Relaxed);
            if let Some(app) = desktop.hit_taskbar(event.x, event.y) {
                if desktop.set_active(app) {
                    uart::write_str("LATTICE APP SWITCH\n");
                    *redraw_window = true;
                }
            }
            draw_touch_marker(surface, event.x, event.y, POINTER_DOWN_COLOR);
        }
        PointerPhase::Move => {
            draw_touch_marker(surface, event.x, event.y, POINTER_MOVE_COLOR);
        }
        PointerPhase::Up => {
            draw_touch_marker(surface, event.x, event.y, POINTER_UP_COLOR);
        }
    }
}

extern "C" fn nozzle_task() -> ! {
    use nozzle::Terminal;

    struct UartTerminal;

    impl Terminal for UartTerminal {
        fn write_str(&mut self, value: &str) {
            nitrogen::arch::xtensa::esp32::uart::write_str(value);
        }

        fn read_byte(&mut self) -> Option<u8> {
            loop {
                if let Some(byte) = nitrogen::arch::xtensa::esp32::uart::receive_byte() {
                    return Some(byte);
                }
                // A blocking shell must remain cooperative with Lattice.
                crate::arch::xtensa::esp32::scheduler::scheduler_yield();
            }
        }
    }

    nitrogen::arch::xtensa::esp32::uart::write_str("NOZZLE START\n");
    let mut terminal = UartTerminal;
    let mut shell = nozzle::Shell::new(
        &mut terminal,
        nozzle_commands::ESP32_COMMANDS,
        nozzle::ShellServices::none(),
    );
    shell.run();
    loop {
        crate::arch::xtensa::esp32::scheduler::scheduler_yield();
    }
}

pub fn next_task_id() -> u32 {
    NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn fixed_task_names() -> alloc::vec::Vec<String> {
    ["lattice", "nozzle", "system-info", "files", "settings"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub fn task_summary() -> String {
    alloc::format!(
        "one-address-space tasks; heap {}/{} bytes",
        memory::used(),
        memory::capacity()
    )
}

/// Print a bounded panic marker over UART0 and halt. Full backtrace support
/// is a separate bring-up milestone; this must never be silent.
pub fn panic_report(_info: &core::panic::PanicInfo<'_>) -> ! {
    panic_message("kernel panic");
}

pub fn panic_message(message: &str) -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("ESP32 PANIC: ");
    nitrogen::arch::xtensa::esp32::uart::write_str(message);
    nitrogen::arch::xtensa::esp32::uart::write_str("\n");
    loop {
        core::hint::spin_loop();
    }
}

pub fn alloc_error_report(_layout: core::alloc::Layout) -> ! {
    // Formatting here would allocate from the allocator that just failed.
    panic_message("allocation failed")
}
