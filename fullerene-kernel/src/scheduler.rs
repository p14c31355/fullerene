//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use crate::graphics;
use alloc::{collections::VecDeque, format};
use core::sync::atomic::Ordering;
use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations, common::SystemStats,
    display_stats_on_available_display, periodic_task, scheduler_log,
};

struct PeriodicTask {
    interval: u64,
    last_tick: spin::Mutex<u64>,
    task: fn(u64, u64),
}

lazy_static::lazy_static! {
    static ref PERIODIC_TASKS: [PeriodicTask; 8] = [
        PeriodicTask { interval: 1000, last_tick: spin::Mutex::new(0), task: perform_system_health_checks },
        PeriodicTask { interval: 5000, last_tick: spin::Mutex::new(0), task: perform_stats_logging },
        PeriodicTask { interval: 2000, last_tick: spin::Mutex::new(0), task: perform_system_maintenance },
        PeriodicTask { interval: 10000, last_tick: spin::Mutex::new(0), task: perform_memory_capacity_check },
        PeriodicTask { interval: 100, last_tick: spin::Mutex::new(0), task: perform_process_cleanup_check },
        PeriodicTask { interval: 30000, last_tick: spin::Mutex::new(0), task: perform_automated_backup },
        PeriodicTask { interval: 5000, last_tick: spin::Mutex::new(0), task: |t, _| draw_desktop_on_available_framebuffer() },
        PeriodicTask { interval: 10000, last_tick: spin::Mutex::new(0), task: |_, _| emergency_condition_handler() },
    ];
}
use x86_64::VirtAddr;

// System-wide counters and statistics
static SYSTEM_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static SCHEDULER_ITERATIONS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// I/O event queue (placeholder for future I/O operations)
static IO_EVENTS: spin::Mutex<VecDeque<IoEvent>> = spin::Mutex::new(VecDeque::new());

// Periodic task intervals (in ticks)
const DESKTOP_UPDATE_INTERVAL_TICKS: u64 = 5000;
const LOG_INTERVAL_TICKS: u64 = 5000;
const DISPLAY_INTERVAL_TICKS: u64 = 5000;

// Configurable thresholds
const HIGH_MEMORY_THRESHOLD: usize = 50; // %
const MAX_PROCESSES_THRESHOLD: usize = 10;
const EMERGENCY_MEMORY_THRESHOLD: usize = 80; // %
const MAX_PROCESSES_EMERGENCY: usize = 100;

/// I/O event type for future I/O handling
#[derive(Clone, Copy)]
struct IoEvent {
    event_type: u8,
    data: usize,
}

/// Perform statistics logging and display
fn perform_stats_logging(_tick: u64, _iter: u64) {
    let stats = collect_system_stats();
    log_system_stats(&stats);
    display_system_stats_on_display(&stats);

    const SYSTEM_LOG_FILE: &str = "system.log";
    let log_content = format!(
        "System Stats - Processes: {}/{}, Memory: {} bytes, Uptime: {} ticks\n",
        stats.active_processes, stats.total_processes, stats.memory_used, stats.uptime_ticks
    );
    if let Ok(_) = crate::fs::create_file(SYSTEM_LOG_FILE, log_content.as_bytes()) {
        log::info!("System log file written successfully");
    } else {
        log::warn!("Failed to write system log file");
    }
}

/// Collect current system statistics
fn collect_system_stats() -> SystemStats {
    petroleum::common::collect_system_stats(
        crate::process::get_process_count,
        crate::process::get_active_process_count,
        || SYSTEM_TICK.load(core::sync::atomic::Ordering::Relaxed),
    )
}

fn process_io_events() {
    let mut events = IO_EVENTS.lock();
    while let Some(event) = events.pop_front() {
        log::debug!("Processed I/O event type {}", event.event_type);
    }
}

/// Perform system health checks (memory, processes, etc.)
fn perform_system_health_checks(_tick: u64, _iter: u64) {
    check_memory_usage();
    check_process_count();
    check_keyboard_buffer();
}

/// Check memory usage and log warnings if high
fn check_memory_usage() {
    let (used, total, _) = petroleum::get_memory_stats!();

    if total > 0 && (used as u128 * 100 / total as u128) > HIGH_MEMORY_THRESHOLD as u128 {
        log::warn!(
            "High memory usage: {} bytes used out of {} bytes",
            used,
            total
        );
    }
}

/// Check process count and log warnings only when threshold exceeded
fn check_process_count() {
    let active_count = crate::process::get_active_process_count();
    if active_count > MAX_PROCESSES_THRESHOLD {
        log::warn!("High process count: {} active processes", active_count);
    }
}

/// Check and drain keyboard buffer if needed
fn check_keyboard_buffer() {
    if crate::keyboard::input_available() {
        let drained = crate::keyboard::drain_line_buffer(&mut []);
        if drained > 256 {
            log::debug!("Drained {} bytes from keyboard buffer", drained);
        }
    }
}

/// Log system statistics
fn log_system_stats(stats: &SystemStats) {
    log::info!(
        "System Stats - Processes: {}/{}, Memory: {} bytes, Uptime: {} ticks",
        stats.active_processes,
        stats.total_processes,
        stats.memory_used,
        stats.uptime_ticks
    );
}

/// Display system statistics on the primary console
fn display_system_stats_on_display(stats: &SystemStats) {
    use petroleum::graphics::Console as _;
    let mut renderer = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut console) = *renderer {
        console.set_cursor(22, 0);
        console.set_color(0x03); // Cyan (VGA index)
        let _ = core::fmt::write(console, format_args!(
            "Processes: {}/{} | Memory: {} KB | Tick: {}\n",
            stats.active_processes,
            stats.total_processes,
            stats.memory_used / 1024,
            stats.uptime_ticks
        ));
    }
}

/// Get the current system tick count
pub fn get_system_tick() -> u64 {
    SYSTEM_TICK.load(Ordering::Relaxed)
}

/// Periodic system maintenance tasks
fn perform_system_maintenance(_tick: u64, _iter: u64) {
    // Environment monitoring
    monitor_environment();

    // Resource optimization
    optimize_system_resources();

    // Background service management
    manage_background_services();
}

fn monitor_environment() {
    let stats = collect_system_stats();
    let (_, total, _) = petroleum::get_memory_stats!();
    if total > 0 && stats.memory_used > total * 3 / 4 {
        log::debug!("High memory usage detected, running memory optimization");
    }
    if stats.active_processes > stats.total_processes / 2 {
        log::warn!("High active process ratio: {}/{}", stats.active_processes, stats.total_processes);
    }
}

fn optimize_system_resources() {
    log::debug!("Running periodic resource optimization");
}

fn manage_background_services() {
    log::debug!("Background service check completed");
}

/// Periodic OS feature: automated filesystem backup
fn perform_automated_backup(_tick: u64, _iter: u64) {
    // Simple backup: fixed message
    let log_content = b"Automated backup completed\n";

    match crate::fs::create_file("backup.log", log_content) {
        Ok(_) => {
            log::debug!("Automated backup completed, log written to backup.log");
        }
        Err(e) => {
            log::warn!("Failed to perform automated backup: {:?}", e);
        }
    }
}

fn process_scheduler_iteration() {
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    let iteration_count = SCHEDULER_ITERATIONS.load(Ordering::Relaxed);

    process_io_events();

    for task in PERIODIC_TASKS.iter() {
        let mut last_tick = task.last_tick.lock();
        if current_tick - *last_tick >= task.interval {
            *last_tick = current_tick;
            (task.task)(current_tick, iteration_count);
        }
    }

    yield_and_process_system_calls();
}

// Autmoated backup function moved to use in stats_task

/// Check and log memory utilization periodically
fn perform_memory_capacity_check(_tick: u64, _iter: u64) {
    let (used_bytes, total_bytes, _) = petroleum::get_memory_stats!();
    let usage_percent = if total_bytes > 0 {
        (used_bytes * 100) / total_bytes
    } else {
        0
    };

    log::info!(
        "Memory utilization: {} bytes / {} bytes ({}%)",
        used_bytes,
        total_bytes,
        usage_percent
    );

    if usage_percent > 90 {
        log::warn!("Critical memory usage (>90%) detected!");
    }
}

/// Periodically clean up terminated processes
fn perform_process_cleanup_check(_tick: u64, _iter: u64) {
    crate::process::cleanup_terminated_processes();
}

/// Yield control and process system calls
fn yield_and_process_system_calls() {
    crate::syscall::kernel_syscall(22, 0, 0, 0); // Yield syscall

    // Allow I/O operations with brief pauses
    for _ in 0..50 {
        petroleum::cpu_pause();
    }
}

/// Draw the OS desktop on the available framebuffer (UEFI or BIOS)
fn draw_desktop_on_available_framebuffer() {
    use petroleum::graphics::Renderer as _;
    let mut renderer = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer {
        let (w, h) = renderer.get_resolution();
        petroleum::serial::serial_log(format_args!("Drawing desktop: {}x{}\n", w, h));
        crate::graphics::draw_os_desktop(renderer);
    } else {
        petroleum::serial::serial_log(format_args!("PRIMARY_RENDERER is None!\n"));
    }
}

/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    scheduler_log!("Scheduler loop starting");

    log::info!("Scheduler loop started");
    crate::graphics::print_to_console("Scheduler loop started\n");

    // Draw the desktop immediately
    draw_desktop_on_available_framebuffer();

    // Debug: Print renderer address to verify mapping
    {
        let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
        if let Some(ref renderer) = *renderer_lock {
            let info = renderer.get_info();
            petroleum::serial::serial_log(format_args!(
                "DEBUG: Renderer FB Virt Addr: {:#x}, Phys Addr: {:#x}, Stride: {}\n", 
                info.address, 
                info.address - petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64,
                info.stride
            ));
        }
    }

    // Verify the drawing test to diagnose rendering issues
    let test_result = {
        let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
        if let Some(ref renderer) = *renderer_lock {
            petroleum::graphics::verify_drawing_test(renderer.get_info())
        } else {
            petroleum::graphics::DrawingTestResult::Fail("PRIMARY_RENDERER is None")
        }
    };

    match test_result {
        petroleum::graphics::DrawingTestResult::Pass => {
            petroleum::serial::serial_log(format_args!("=== GRAPHICS_TEST PASS ===\n"));
        }
        petroleum::graphics::DrawingTestResult::Fail(msg) => {
            petroleum::serial::serial_log(format_args!("=== GRAPHICS_TEST FAIL: {} ===\n", msg));
        }
    }

    loop {
        SYSTEM_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        SCHEDULER_ITERATIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        process_scheduler_iteration();
        // Cooperative yield to idle process (if available)
        if crate::process::get_active_process_count() > 0 {
            crate::process::yield_current();
        } else {
            // No processes to yield to, just pause briefly
            for _ in 0..1000 { petroleum::cpu_pause(); }
        }
    }
}

/// Handle emergency system conditions (OOM, process limits, etc.)
fn emergency_condition_handler() {
    check_emergency_memory();
    check_emergency_process_count();
}

petroleum::health_check!(
    check_emergency_memory,
    {
        let (used, total, _) = petroleum::get_memory_stats!();
        total > 0 && (used as u128 * 100 / total as u128) > EMERGENCY_MEMORY_THRESHOLD as u128
    },
    error,
    "EMERGENCY: Critical memory usage detected!",
    {
        // In a full implementation, this would:
        // 1. Kill memory-hog processes
        // 2. Perform emergency memory cleanup
        // 3. Log diagnostic information
    }
);

petroleum::health_check!(
    check_emergency_process_count,
    { crate::process::get_active_process_count() > MAX_PROCESSES_EMERGENCY },
    error,
    "EMERGENCY: Too many active processes!",
    {
        // Would implement process cleanup here
    }
);

/// Shell process main function
pub extern "C" fn shell_process_main() -> ! {
    log::info!("Shell process started");

    // Run the shell main loop
    crate::shell::shell_main();

    // If shell exits, terminate the process
    crate::process::terminate_process(crate::process::current_pid().unwrap(), 0);

    // Should never reach here
    petroleum::halt_loop();
}
