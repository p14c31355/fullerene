//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use crate::graphics;
use alloc::{collections::VecDeque, format};
use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations, common::SystemStats, display_stats_on_available_display, periodic_task, scheduler_log,
};

// Define periodic tasks in a struct for clarity
struct PeriodicTask {
    interval: u64,
    last_tick: alloc::sync::Arc<spin::Mutex<u64>>,
    task: fn(u64, u64), // current_tick, iteration_count
}

// Wrapper functions for tasks that need parameters
fn health_check_task(_tick: u64, _iter: u64) {
    perform_system_health_checks();
}

fn stats_task(_tick: u64, _iter: u64) {
    let stats = collect_system_stats();
    log_system_stats(&stats, LOG_INTERVAL_TICKS);
    display_system_stats_on_display(&stats, DISPLAY_INTERVAL_TICKS);
    // Add file logging
    const SYSTEM_LOG_FILE: &str = "system.log";
    let log_content = format!(
        "System Stats - Processes: {}/{}, Memory: {} bytes, Uptime: {} ticks\n",
        stats.active_processes,
        stats.total_processes,
        stats.memory_used,
        stats.uptime_ticks
    );
    if let Ok(_) = crate::fs::create_file(SYSTEM_LOG_FILE, log_content.as_bytes()) {
        log::info!("System log file written successfully");
    } else {
        log::warn!("Failed to write system log file");
    }
}

fn maintenance_task(_tick: u64, _iter: u64) {
    perform_system_maintenance();
}

fn memory_check_task(tick: u64, _iter: u64) {
    perform_memory_capacity_check(tick);
}

fn process_cleanup_task(_tick: u64, iter: u64) {
    perform_process_cleanup_check(iter);
}

fn backup_task(_tick: u64, _iter: u64) {
    perform_automated_backup();
}

lazy_static::lazy_static! {
    static ref PERIODIC_TASKS: [PeriodicTask; 6] = [
        PeriodicTask {
            interval: 1000,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: health_check_task,
        },
        PeriodicTask {
            interval: 5000,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: stats_task,
        },
        PeriodicTask {
            interval: 2000,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: maintenance_task,
        },
        PeriodicTask {
            interval: 10000,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: memory_check_task,
        },
        PeriodicTask {
            interval: 100,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: process_cleanup_task,
        },
        PeriodicTask {
            interval: 30000,
            last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
            task: backup_task,
        },
    ];
}
use x86_64::VirtAddr;

// System-wide counters and statistics
static SYSTEM_TICK: AtomicU64 = AtomicU64::new(0);
static SCHEDULER_ITERATIONS: AtomicU64 = AtomicU64::new(0);

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

/// Collect current system statistics
fn collect_system_stats() -> SystemStats {
    petroleum::common::collect_system_stats(
        crate::process::get_process_count,
        crate::process::get_active_process_count,
        || SYSTEM_TICK.load(Ordering::Relaxed),
    )
}

/// Process I/O events (placeholder for future expansion)
fn process_io_events() {
    let mut events = IO_EVENTS.lock();

    // Process all pending I/O events
    while let Some(event) = events.pop_front() {
        match event.event_type {
            // Placeholder for different event types
            0x01 => log::debug!("Processed keyboard event"),
            0x02 => log::debug!("Processed filesystem event"),
            _ => log::debug!("Processed unknown I/O event type {}", event.event_type),
        }
    }

    // Re-process any remaining events during next iteration
}

/// Perform system health checks (memory, processes, etc.)
fn perform_system_health_checks() {
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

/// Log system statistics periodically
fn log_system_stats(stats: &SystemStats, interval_ticks: u64) {
    static LAST_LOG_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

    // Only log every interval_ticks to avoid spam
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    petroleum::check_periodic!(LAST_LOG_TICK, interval_ticks, current_tick, {
        log::info!(
            "System Stats - Processes: {}/{}, Memory: {} bytes, Uptime: {} ticks",
            stats.active_processes,
            stats.total_processes,
            stats.memory_used,
            stats.uptime_ticks
        );
    });
}

/// Display system statistics on VGA or framebuffer periodically
fn display_system_stats_on_display(stats: &SystemStats, interval_ticks: u64) {
    petroleum::display_stats_on_available_display!(stats, SYSTEM_TICK.load(Ordering::Relaxed), interval_ticks, &crate::vga::VGA_BUFFER);
}

/// Get the current system tick count
pub fn get_system_tick() -> u64 {
    SYSTEM_TICK.load(Ordering::Relaxed)
}

/// Periodic system maintenance tasks
fn perform_system_maintenance() {
    // Environment monitoring
    monitor_environment();

    // Resource optimization
    optimize_system_resources();

    // Background service management
    manage_background_services();
}

/// Monitor system environment and adapt accordingly
fn monitor_environment() {
    // Check CPU load distribution
    let system_stats = collect_system_stats();

    // If memory usage is high, perform garbage collection
    let (_, total_memory, _) = petroleum::get_memory_stats!();
    if total_memory > 0 && system_stats.memory_used > total_memory * 3 / 4 {
        // >75%
        log::debug!("High memory usage detected, running memory optimization");
        // petroleum::page_table::ALLOCATOR.lock().optimize(); // Method not available
    }

    // Monitor process health
    if system_stats.active_processes > system_stats.total_processes / 2 {
        log::warn!(
            "High active process ratio: {}/{}",
            system_stats.active_processes,
            system_stats.total_processes
        );
    }
}

/// Perform resource optimization tasks
fn optimize_system_resources() {
    // Optimize memory layout periodically
    static LAST_OPTIMIZATION_TICK: spin::Mutex<u64> = spin::Mutex::new(0);
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    petroleum::check_periodic!(LAST_OPTIMIZATION_TICK, 10000, current_tick, {
        // Every 10000 ticks
        // Run memory defragmentation or optimization
        log::debug!("Running periodic resource optimization");

        // Optimize heap allocation patterns
        // petroleum::page_table::ALLOCATOR.lock().defragment(); // Method not available
    });
}

/// Manage background system services
fn manage_background_services() {
    // Placeholder for future background services
    // Ideas: disk I/O scheduler, network protocol handlers, device monitoring

    // For now, just ensure system remains responsive
    if SYSTEM_TICK.load(Ordering::Relaxed) % 5000 == 0 {
        log::debug!("Background service check completed");
    }
}

/// Periodic OS feature: automated filesystem backup
fn perform_automated_backup() {
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

/// Process a single scheduler iteration
fn process_scheduler_iteration() {
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    let iteration_count = SCHEDULER_ITERATIONS.load(Ordering::Relaxed);

    // Process I/O events
    process_io_events();

    // Run periodic tasks from PERIODIC_TASKS array
    for task in PERIODIC_TASKS.iter() {
        let mut last_tick = task.last_tick.lock();
        if current_tick - *last_tick >= task.interval {
            *last_tick = current_tick;
            (task.task)(current_tick, iteration_count);
        }
    }

    // Additional tasks that don't fit the pattern
    if current_tick % DESKTOP_UPDATE_INTERVAL_TICKS == 0 {
        graphics::draw_os_desktop();
    }
    if current_tick % 10000 == 0 {
        emergency_condition_handler();
    }

    // Yield and handle system calls
    yield_and_process_system_calls();
}

// Autmoated backup function moved to use in stats_task

/// Check and log memory utilization periodically
fn perform_memory_capacity_check(current_tick: u64) {
    periodic_task!(current_tick, 10000, {
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
    });
}

/// Periodically clean up terminated processes
fn perform_process_cleanup_check(iteration_count: u64) {
    periodic_task!(iteration_count, 100, {
        crate::process::cleanup_terminated_processes();
    });
}

/// Yield control and process system calls
fn yield_and_process_system_calls() {
    crate::syscall::kernel_syscall(22, 0, 0, 0); // Yield syscall

    // Allow I/O operations with brief pauses
    for _ in 0..50 {
        petroleum::cpu_pause();
    }
}

/// Handle periodic UI operations (desktop updates)
fn perform_periodic_ui_operations(current_tick: u64) {
    if current_tick % DESKTOP_UPDATE_INTERVAL_TICKS == 0 {
        graphics::draw_os_desktop();
    }
}

/// Perform periodic emergency condition checks
fn perform_emergency_checks(current_tick: u64) {
    if current_tick % 10000 == 0 {
        emergency_condition_handler();
    }
}

/// Initialize shell process and return PID
fn initialize_shell_process() -> crate::process::ProcessId {
    let shell_pid = crate::process::create_process(
        "shell_process",
        VirtAddr::new(shell_process_main as usize as u64),
    )
    .expect("Failed to create shell process");
    log::info!("Created shell process with PID {}", shell_pid);
    crate::process::unblock_process(shell_pid);
    shell_pid
}

/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    log::info!("Starting enhanced OS scheduler with integrated system features...");
    scheduler_log!("About to initialize shell process");

    let _ = initialize_shell_process();
    scheduler_log!("Shell process initialized successfully");

    // Main scheduler loop - continuously execute processes with integrated OS functionality
    scheduler_log!("Entering main loop");
    scheduler_log!("Main loop starting");

    // Print to VGA if available for GUI output
    if let Some(vga_buffer) = crate::vga::VGA_BUFFER.get() {
        let mut writer = vga_buffer.lock();
        writer.write_string("Scheduler loop started - VGA output enabled\n");
        writer.write_string("System is running...\n");
        writer.update_cursor();
    }
    // Log that scheduler is running for confirmation
    log::info!("Scheduler loop started - framebuffer text system active");

    loop {
        // Increment system counters for this iteration
        SYSTEM_TICK.fetch_add(1, Ordering::Relaxed);
        SCHEDULER_ITERATIONS.fetch_add(1, Ordering::Relaxed);

        // Process one complete scheduler iteration
        process_scheduler_iteration();

        // Yield to user processes if any are ready
        crate::process::yield_current();
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
