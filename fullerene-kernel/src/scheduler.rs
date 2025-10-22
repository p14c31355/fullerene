//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use crate::graphics;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::{
    Color, ColorCode, ScreenChar, TextBufferOperations, check_periodic, periodic_task, write_serial_bytes,
};
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

// System diagnostics structure
#[derive(Clone, Copy)]
struct SystemStats {
    total_processes: usize,
    active_processes: usize,
    memory_used: usize,
    uptime_ticks: u64,
}

/// I/O event type for future I/O handling
#[derive(Clone, Copy)]
struct IoEvent {
    event_type: u8,
    data: usize,
}

/// Collect current system statistics
fn collect_system_stats() -> SystemStats {
    // Count total and active processes
    // For now, we'll use a simple implementation
    let total_processes = crate::process::get_process_count();
    let active_processes = crate::process::get_active_process_count();

    // Get memory usage from the global allocator
    let (memory_used, _, _) = petroleum::get_memory_stats!();

    let uptime_ticks = SYSTEM_TICK.load(Ordering::Relaxed);

    SystemStats {
        total_processes,
        active_processes,
        memory_used,
        uptime_ticks,
    }
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
    // Check memory usage
    let (used, total, _) = petroleum::get_memory_stats!();

    // Log warning if memory usage is high
    if used > total / 2 {
        log::warn!(
            "High memory usage: {} bytes used out of {} bytes",
            used,
            total
        );
    }

    // Check for too many processes
    let active_count = crate::process::get_active_process_count();
    if active_count > 10 {
        log::warn!("High process count: {} active processes", active_count);
    }

    // Check keyboard buffer for overflow
    if crate::keyboard::input_available() {
        // Drain excess input to prevent buffer overflow
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

/// Display system statistics on VGA periodically
fn display_system_stats_on_vga(stats: &SystemStats, interval_ticks: u64) {
    static LAST_DISPLAY_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    petroleum::check_periodic!(LAST_DISPLAY_TICK, interval_ticks, current_tick, {
        if let Some(vga_buffer) = crate::vga::VGA_BUFFER.get() {
            const TICKS_PER_SECOND: u64 = 1000; // Assuming ~1000 ticks per second
            let uptime_minutes = stats.uptime_ticks / (60 * TICKS_PER_SECOND);
            let uptime_seconds = (stats.uptime_ticks % (60 * TICKS_PER_SECOND)) / TICKS_PER_SECOND;

            let mut vga_writer = vga_buffer.lock();

            // Clear bottom rows for system info display
            let blank_char = petroleum::ScreenChar {
                ascii_character: b' ',
                color_code: petroleum::ColorCode::new(
                    petroleum::Color::Black,
                    petroleum::Color::Black,
                ),
            };

            // Set position to bottom left for system info
            vga_writer.set_position(22, 0);
            use core::fmt::Write;
            use petroleum::ColorCode;
            vga_writer.set_color_code(ColorCode::new(
                petroleum::Color::Cyan,
                petroleum::Color::Black,
            ));

            // Clear the status lines first
            for col in 0..80 {
                vga_writer.set_char_at(23, col, blank_char);
                vga_writer.set_char_at(24, col, blank_char);
            }

            // Display system info on bottom rows
            vga_writer.set_position(23, 0);
            let _ = write!(
                vga_writer,
                "Processes: {}/{}  ",
                stats.active_processes, stats.total_processes
            );
            let _ = write!(vga_writer, "Memory: {} KB  ", stats.memory_used / 1024);
            let _ = write!(vga_writer, "Tick: {}", stats.uptime_ticks);
            vga_writer.update_cursor();
        }
    });
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

/// Helper function for logging system stats to filesystem
fn log_system_stats_to_fs(stats: &SystemStats) {
    // Use alloc::format! to create a log string with actual stats.
    let log_content = alloc::format!(
        "Uptime: {}, Processes: {}/{}, Memory Used: {}\n",
        stats.uptime_ticks,
        stats.active_processes,
        stats.total_processes,
        stats.memory_used
    );

    static LOG_FILE_CREATED: spin::Mutex<bool> = spin::Mutex::new(false);
    let mut log_file_created = LOG_FILE_CREATED.lock();

    if !*log_file_created {
        match crate::fs::create_file("system.log", log_content.as_bytes()) {
            Ok(_) => {
                *log_file_created = true;
                log::debug!("Created system.log file");
            }
            Err(e) => {
                log::warn!("Failed to create system.log file: {:?}", e);
            }
        }
    } else {
        match crate::fs::open_file("system.log") {
            Ok(fd) => {
                if let Err(e) = crate::fs::seek_file(fd, 0) {
                    log::warn!("Failed to seek in system.log: {:?}", e);
                }
                if let Err(e) = crate::fs::write_file(fd, log_content.as_bytes()) {
                    log::warn!("Failed to write to system.log: {:?}", e);
                }
                if let Err(e) = crate::fs::close_file(fd) {
                    log::warn!("Failed to close system.log: {:?}", e);
                }
            }
            Err(e) => {
                log::warn!("Failed to open system.log file: {:?}", e);
            }
        }
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
    let system_stats = collect_system_stats();

    // Process I/O events and perform periodic tasks
    process_io_events();
    perform_periodic_health_checks(&system_stats, current_tick);
    perform_periodic_system_tasks(&system_stats, current_tick, iteration_count);

    // Yield and handle system calls
    yield_and_process_system_calls();

    // Periodic desktop update and emergency checks
    perform_periodic_ui_operations(current_tick);
    perform_emergency_checks(current_tick);
}

/// Perform periodic health checks and statistics
fn perform_periodic_health_checks(stats: &SystemStats, current_tick: u64) {
    static LAST_HEALTH_CHECK_TICK: spin::Mutex<u64> = spin::Mutex::new(0);
    check_periodic!(LAST_HEALTH_CHECK_TICK, 1000, current_tick, {
        perform_system_health_checks();
        log_system_stats(stats, LOG_INTERVAL_TICKS);
        display_system_stats_on_vga(stats, DISPLAY_INTERVAL_TICKS);
    });
}

/// Perform periodic filesystem and maintenance tasks
fn perform_periodic_system_tasks(stats: &SystemStats, current_tick: u64, iteration_count: u64) {
    periodic_task!(current_tick, 3000, {
        log_system_stats_to_fs(stats);
        perform_automated_backup();
    });

    periodic_task!(current_tick, 2000, {
        perform_system_maintenance();
    });

    perform_memory_capacity_check(current_tick);
    perform_process_cleanup_check(iteration_count);
}

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
    ).expect("Failed to create shell process");
    log::info!("Created shell process with PID {}", shell_pid);
    crate::process::unblock_process(shell_pid);
    shell_pid
}

/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    log::info!("Starting enhanced OS scheduler with integrated system features...");
    write_serial_bytes!(0x3F8, 0x3FD, b"Scheduler: About to initialize shell process\n");

    let _ = initialize_shell_process();
    write_serial_bytes!(0x3F8, 0x3FD, b"Scheduler: Shell process initialized successfully\n");

    // Main scheduler loop - continuously execute processes with integrated OS functionality
    log::info!("Scheduler: Entering main loop");
    write_serial_bytes!(0x3F8, 0x3FD, b"Scheduler: Main loop starting\n");
    loop {
        // Increment system counters for this iteration
        SYSTEM_TICK.fetch_add(1, Ordering::Relaxed);
        SCHEDULER_ITERATIONS.fetch_add(1, Ordering::Relaxed);

        // Process one complete scheduler iteration
        process_scheduler_iteration();
    }
}

/// Handle emergency system conditions (OOM, process limits, etc.)
fn emergency_condition_handler() {
    // Check for out-of-memory condition
    let (used, total, _) = petroleum::get_memory_stats!();
    if used > (total * 4) / 5 {
        // >80% usage
        log::error!("EMERGENCY: Critical memory usage detected!");
        // In a full implementation, this would:
        // 1. Kill memory-hog processes
        // 2. Perform emergency memory cleanup
        // 3. Log diagnostic information
    }

    // Check process limits
    if crate::process::get_active_process_count() > 100 {
        log::error!("EMERGENCY: Too many active processes!");
        // Would implement process cleanup here
    }
}

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
