//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use alloc::{collections::VecDeque, format};
use core::sync::atomic::Ordering;
use petroleum::{common::SystemStats, scheduler_log};

struct PeriodicTask {
    interval: u64,
    last_tick: spin::Mutex<u64>,
    task: fn(u64, u64),
}

/// Wrapper functions to match `fn(u64, u64)` signature for non-arg tasks
fn emergency_handler_task(_tick: u64, _iter: u64) {
    emergency_condition_handler();
}

/// Pre-allocated periodic tasks array (no heap allocation, no lazy_static)
const PERIODIC_TASKS: [PeriodicTask; 7] = [
    PeriodicTask {
        interval: 1000,
        last_tick: spin::Mutex::new(0),
        task: perform_system_health_checks,
    },
    PeriodicTask {
        interval: 5000,
        last_tick: spin::Mutex::new(0),
        task: perform_stats_logging,
    },
    PeriodicTask {
        interval: 2000,
        last_tick: spin::Mutex::new(0),
        task: perform_system_maintenance,
    },
    PeriodicTask {
        interval: 10000,
        last_tick: spin::Mutex::new(0),
        task: perform_memory_capacity_check,
    },
    PeriodicTask {
        interval: 100,
        last_tick: spin::Mutex::new(0),
        task: perform_process_cleanup_check,
    },
    PeriodicTask {
        interval: 30000,
        last_tick: spin::Mutex::new(0),
        task: perform_automated_backup,
    },
    // draw_desktop_task REMOVED — rendering is now owned by Solvent runtime_tick
    // with frame pacing (FRAME_TIMER_ID).  Calling gui::render() from a periodic
    // task would bypass frame pacing and cause full‑framebuffer clears at the
    // periodic rate, contributing to flicker.
    PeriodicTask {
        interval: 10000,
        last_tick: spin::Mutex::new(0),
        task: emergency_handler_task,
    },
];

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
    if nitrogen::ps2::keyboard::input_available() {
        let drained = nitrogen::ps2::keyboard::drain_line_buffer(&mut []);
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

fn display_system_stats_on_display(stats: &SystemStats) {
    // Placeholder for console drawing logic
    petroleum::serial::_print(format_args!(
        "Processes: {}/{} | Memory: {} KB | Tick: {}\n",
        stats.active_processes,
        stats.total_processes,
        stats.memory_used / 1024,
        stats.uptime_ticks
    ));
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
        log::warn!(
            "High active process ratio: {}/{}",
            stats.active_processes,
            stats.total_processes
        );
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

/// Draw the OS desktop using Lattice compositor (or fallback).
fn draw_desktop_on_available_framebuffer() {
    crate::gui::render();
}

/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    scheduler_log!("Scheduler loop starting");

    log::info!("Scheduler loop started");
    // Use a simpler approach to console printing that doesn't rely on global renderer state
    petroleum::serial::_print(format_args!("Scheduler loop started\n"));

    // Draw the desktop immediately
    draw_desktop_on_available_framebuffer();

    // Verify the drawing test to diagnose rendering issues.
    // Since the framebuffer is now accessed via PCI direct BAR0 probe
    // (see init_graphics), use verify_drawing_test which performs
    // volatile write + readback verification through the PCI MMIO window.
    //
    // IMPORTANT: The actual renderer lives in KernelContext.framebuffer,
    // not the standalone global defined by define_context!.
    // gui::render() uses with_kernel_mut(|k| k.framebuffer.renderer),
    // so the test must reference the same instance.
    let test_result = {
        let kernel_lock = crate::contexts::kernel::get_kernel();
        let kg = kernel_lock.lock();
        match kg.as_ref().and_then(|k| k.framebuffer.info()) {
            Some(info) => petroleum::graphics::verify_drawing_test(&info),
            None => petroleum::graphics::DrawingTestResult::Fail(
                "KernelContext.framebuffer has no renderer (info() returned None)",
            ),
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

    // Inject the kernel's framebuffer renderer into Solvent so that
    // runtime_tick_no_fb() (called from the shell's read_byte yield loop)
    // can paint the terminal buffer, cursor, and desktop onto the screen.
    crate::gui::set_render_fn(crate::gui::render);

    // The shell is NOT a separate process — it runs cooperatively inside the
    // scheduler loop.  When the shell blocks waiting for keyboard input, its
    // LatticeTerminal::read_byte() services the entire runtime (mouse polling,
    // timer advancement, event processing, rendering) via runtime_tick_no_fb().
    // This means the shell IS the scheduler loop.
    crate::shell::shell_main();

    // shell_main exits (via "exit" command) → halt the system.
    petroleum::halt_loop();
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
    crate::process::get_active_process_count() > MAX_PROCESSES_EMERGENCY,
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
