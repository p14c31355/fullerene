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
    touchscreen::Xpt2046Touch,
    uart,
};
use nitrogen::geometry::Rect;
use nitrogen::input::{PointerEvent, PointerPhase, PointerQueue};
use spin::Mutex;

static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);
static DISPLAY: Mutex<Option<SpiLcd>> = Mutex::new(None);

/// Events produced while a flush was in flight wait here for the shell.
const INPUT_QUEUE_EVENTS: usize = 16;

/// The on-panel pointer indicator, inside the top panel.
///
/// Down/Move/Up each repaint exactly this square, so the event path costs a
/// 12x12 transfer (288 bytes) and never leaves a trail to erase - the reason it
/// is a fixed indicator rather than a marker at the contact point.
const POINTER_INDICATOR: Rect = Rect::new(292, 5, POINTER_MARKER, POINTER_MARKER);
const POINTER_DOWN_COLOR: u16 = 0xf800;
const POINTER_MOVE_COLOR: u16 = 0xfd20;
const POINTER_UP_COLOR: u16 = 0x07e0;

pub fn boot() -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("FULLERENE BOOT\n");

    let _profile = BoardProfile::xh32s();
    nitrogen::arch::xtensa::esp32::uart::write_str("LCD INIT\n");
    let mut lcd = SpiLcd::new();
    if lcd.init().is_err() {
        crate::arch::xtensa::esp32::runtime::panic_message("LCD initialization failed");
    }
    let mut banner = String::new();
    core::fmt::Write::write_fmt(
        &mut banner,
        format_args!("LCD OK hspi_hz={}\n", lcd.clock_hz()),
    )
    .ok();
    nitrogen::arch::xtensa::esp32::uart::write_str(&banner);
    *DISPLAY.lock() = Some(lcd);
    // Pin 21 is tied to the display backlight on the profile board.
    nitrogen::arch::xtensa::esp32::gpio::enable_output(board::LCD_BACKLIGHT);
    nitrogen::arch::xtensa::esp32::gpio::set_output_high(board::LCD_BACKLIGHT);

    time::init();
    nitrogen::arch::xtensa::esp32::uart::write_str("TIME OK\n");
    let mut scheduler = Scheduler::new();
    unsafe {
        nozzle_commands::MEMINFO = Some(|| (memory::used(), memory::capacity()));
        nozzle_commands::TASK_NAMES = Some(|| fixed_task_names());
        nozzle_commands::UPTIME = Some(time::uptime_millis);
        nozzle_commands::REBOOT = Some(reboot);
        nozzle_commands::LATENCY = Some(latency_report);
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
        Self {
            touch: Xpt2046Touch::new(profile),
            queue: PointerQueue::new(),
            pen_seen: 0,
        }
    }

    /// Called between display chunks. Returns whether the pen was down.
    fn service(&mut self, now_ms: u32) -> bool {
        if !self.touch.pen_active() {
            return false;
        }
        self.pen_seen = self.pen_seen.saturating_add(1);
        if let Some(event) = self.touch.poll(now_ms) {
            self.queue.push(event);
        }
        true
    }

    /// Take the next event, sampling directly when the pacer queued nothing.
    fn poll(&mut self, now_ms: u32) -> Option<PointerEvent> {
        if let Some(event) = self.queue.pop() {
            return Some(event);
        }
        self.service(now_ms);
        self.queue.pop()
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
            "  spi_hz={} timer_prescale={} tick_us={}\n",
            current_spi_hz(),
            time::prescale(),
            1_000 / time::prescale().max(1),
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
    let started = time::uptime_micros();
    let mut pace = || {
        pace_calls = pace_calls.saturating_add(1);
        pump.service(time::uptime_millis() as u32);
    };
    let result = lcd.flush_paced(
        pixels,
        LcdRect::new(rect.x, rect.y, rect.width, rect.height),
        &mut pace,
    );
    let elapsed = time::uptime_micros().saturating_sub(started) as u32;
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
    let fill_started = time::uptime_micros();
    let _ = surface.fill(
        WINDOW_RECT.x,
        WINDOW_RECT.y,
        WINDOW_RECT.width,
        WINDOW_RECT.height,
        0x001f,
    );
    let fill_us = time::uptime_micros().saturating_sub(fill_started) as u32;

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
        let now_ms = time::uptime_millis() as u32;

        if let Some(event) = pump.poll(now_ms) {
            let event_started = time::uptime_micros();
            handle_pointer(
                event,
                &mut desktop,
                &mut surface,
                &mut redraw_window,
                event_started,
            );
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
                let delta = time::uptime_micros().saturating_sub(u64::from(tap_started)) as u32;
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

fn handle_pointer(
    event: PointerEvent,
    desktop: &mut EmbeddedDesktop,
    surface: &mut Esp32Compositor,
    redraw_window: &mut bool,
    event_started_us: u64,
) {
    let mut line = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!(
            "TOUCH {} us={} at={},{}\n",
            event.phase.name(),
            event_started_us as u32,
            event.x,
            event.y
        ),
    );
    uart::write_str(&line);

    match event.phase {
        PointerPhase::Down => {
            // The tap-to-flush measurement starts here and is closed out when
            // the resulting transfer completes.
            PENDING_TAP_US.store(event_started_us as u32, Ordering::Relaxed);
            if let Some(app) = desktop.hit_taskbar(event.x, event.y) {
                if desktop.set_active(app) {
                    uart::write_str("LATTICE APP SWITCH\n");
                    *redraw_window = true;
                }
            }
            let _ = surface.fill(
                POINTER_INDICATOR.x,
                POINTER_INDICATOR.y,
                POINTER_INDICATOR.width,
                POINTER_INDICATOR.height,
                POINTER_DOWN_COLOR,
            );
        }
        PointerPhase::Move => {
            let _ = surface.fill(
                POINTER_INDICATOR.x,
                POINTER_INDICATOR.y,
                POINTER_INDICATOR.width,
                POINTER_INDICATOR.height,
                POINTER_MOVE_COLOR,
            );
        }
        PointerPhase::Up => {
            let _ = surface.fill(
                POINTER_INDICATOR.x,
                POINTER_INDICATOR.y,
                POINTER_INDICATOR.width,
                POINTER_INDICATOR.height,
                POINTER_UP_COLOR,
            );
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
