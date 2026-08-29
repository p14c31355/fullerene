//! Kernel startup and the fixed ESP32 application set.

use crate::arch::xtensa::esp32::{board, memory, scheduler::Scheduler, time};
use nozzle::arch::xtensa::esp32::commands as nozzle_commands;

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use lattice::arch::xtensa::esp32::{EmbeddedDesktop, Esp32Compositor};
use nitrogen::arch::xtensa::esp32::{
    board::BoardProfile, display::SpiLcd, touchscreen::Xpt2046Touch, uart,
};
use spin::Mutex;

static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);
static DISPLAY: Mutex<Option<SpiLcd>> = Mutex::new(None);

pub fn boot() -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("FULLERENE BOOT\n");

    let _profile = BoardProfile::xh32s();
    nitrogen::arch::xtensa::esp32::uart::write_str("LCD INIT\n");
    let mut lcd = SpiLcd::new();
    if lcd.init().is_err() {
        crate::arch::xtensa::esp32::runtime::panic_message("LCD initialization failed");
    }
    *DISPLAY.lock() = Some(lcd);
    // Pin 21 is tied to the display backlight on the profile board.
    nitrogen::arch::xtensa::esp32::gpio::enable_output(board::LCD_BACKLIGHT);
    nitrogen::arch::xtensa::esp32::gpio::set_output_high(board::LCD_BACKLIGHT);

    nitrogen::arch::xtensa::esp32::uart::write_str("LCD OK\n");
    time::init();
    nitrogen::arch::xtensa::esp32::uart::write_str("TIME OK\n");
    let mut scheduler = Scheduler::new();
    unsafe {
        nozzle_commands::MEMINFO = Some(|| (memory::used(), memory::capacity()));
        nozzle_commands::TASK_NAMES = Some(|| fixed_task_names());
        nozzle_commands::UPTIME = Some(time::uptime_millis);
        nozzle_commands::REBOOT = Some(|| true);
    }
    scheduler.spawn("lattice", lattice_task as *const () as usize, 8 * 1024);
    scheduler.spawn("nozzle", nozzle_task as *const () as usize, 4 * 1024);
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

extern "C" fn lattice_task() -> ! {
    nitrogen::arch::xtensa::esp32::uart::write_str("LATTICE START\n");
    let profile = BoardProfile::xh32s();
    let mut desktop = EmbeddedDesktop::new();
    let mut surface = Esp32Compositor::new(320, 240);
    if !surface.allocate() {
        crate::arch::xtensa::esp32::runtime::panic_message("display surface allocation failed");
    }
    let mut touch = Xpt2046Touch::new(profile);
    let mut previous_pressed = false;
    let mut redraw = true;

    loop {
        if redraw {
            let mut display = DISPLAY.lock();
            if let Some(lcd) = display.as_mut() {
                desktop.render(&mut surface);
                lcd.mark_full_dirty();
                if lcd.flush(surface.pixels()).is_err() {
                    drop(display);
                    crate::arch::xtensa::esp32::runtime::panic_message("LCD flush failed");
                }
            }
            redraw = false;
        }

        if let Some(sample) = touch.read() {
            if sample.pressed && !previous_pressed {
                let (x, y) = touch.map_to_screen(sample, 320, 240);
                uart::write_str("TOUCH DOWN raw=");
                uart::write_u32(u32::from(sample.x));
                uart::write_str(",");
                uart::write_u32(u32::from(sample.y));
                uart::write_str(" screen=");
                uart::write_u32(u32::from(x));
                uart::write_str(",");
                uart::write_u32(u32::from(y));
                uart::write_str("\n");
                if let Some(app) = desktop.hit_taskbar(x, y) {
                    if desktop.set_active(app) {
                        nitrogen::arch::xtensa::esp32::uart::write_str("LATTICE APP SWITCH\n");
                        redraw = true;
                    }
                }
            } else if !sample.pressed && previous_pressed {
                nitrogen::arch::xtensa::esp32::uart::write_str("TOUCH UP\n");
            }
            previous_pressed = sample.pressed;
        }

        crate::arch::xtensa::esp32::scheduler::scheduler_yield();
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

pub fn alloc_error_report(layout: core::alloc::Layout) -> ! {
    let mut text = alloc::string::String::new();
    core::fmt::Write::write_fmt(
        &mut text,
        format_args!("allocation failed: {} bytes\n", layout.size()),
    )
    .ok();
    panic_message(&text)
}
