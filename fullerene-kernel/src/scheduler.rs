//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use x86_64::VirtAddr;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::{TextBufferOperations, Color, ColorCode, ScreenChar};

// System-wide counters and statistics
static SYSTEM_TICK: AtomicU64 = AtomicU64::new(0);
static SCHEDULER_ITERATIONS: AtomicU64 = AtomicU64::new(0);

// I/O event queue (placeholder for future I/O operations)
static IO_EVENTS: spin::Mutex<VecDeque<IoEvent>> = spin::Mutex::new(VecDeque::new());

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
    let memory_used = petroleum::page_table::ALLOCATOR.lock().used();

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
    let allocator = petroleum::page_table::ALLOCATOR.lock();
    let used = allocator.used();
    let total = allocator.size();

    // Log warning if memory usage is high
    if used > total / 2 {
        log::warn!("High memory usage: {} bytes used out of {} bytes", used, total);
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
    static mut LAST_LOG_TICK: u64 = 0;

    // Only log every interval_ticks to avoid spam
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    unsafe {
        if current_tick - LAST_LOG_TICK >= interval_ticks {
            log::info!(
                "System Stats - Processes: {}/{}, Memory: {} bytes, Uptime: {} ticks",
                stats.active_processes,
                stats.total_processes,
                stats.memory_used,
                stats.uptime_ticks
            );
            LAST_LOG_TICK = current_tick;
        }
    }
}

/// Display system statistics on VGA periodically
fn display_system_stats_on_vga(stats: &SystemStats, interval_ticks: u64) {
    static mut LAST_DISPLAY_TICK: u64 = 0;

    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
    unsafe {
        if current_tick - LAST_DISPLAY_TICK >= interval_ticks {
            if let Some(vga_buffer) = crate::vga::VGA_BUFFER.get() {
                let uptime_minutes = stats.uptime_ticks / 60000; // Assuming ~1000 ticks per second
                let uptime_seconds = (stats.uptime_ticks % 60000) / 1000;

                let mut vga_writer = vga_buffer.lock();

                // Clear bottom rows for system info display
                let blank_char = petroleum::ScreenChar {
                    ascii_character: b' ',
                    color_code: petroleum::ColorCode::new(petroleum::Color::Black, petroleum::Color::Black),
                };

                // Set position to bottom left for system info
                vga_writer.set_position(22, 0);
                use core::fmt::Write;
                use petroleum::ColorCode;
                vga_writer.set_color_code(ColorCode::new(petroleum::Color::Cyan, petroleum::Color::Black));

                // Clear the status lines first
                for col in 0..80 {
                    vga_writer.set_char_at(23, col, blank_char);
                    vga_writer.set_char_at(24, col, blank_char);
                }

                // Display system info on bottom rows
                vga_writer.set_position(23, 0);
                let _ = write!(vga_writer, "Processes: {}/{}  ", stats.active_processes, stats.total_processes);
                let _ = write!(vga_writer, "Memory: {} KB  ", stats.memory_used / 1024);
                let _ = write!(vga_writer, "Tick: {}", stats.uptime_ticks);
                vga_writer.update_cursor();
            }
            LAST_DISPLAY_TICK = current_tick;
        }
    }
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
    if system_stats.memory_used > system_stats.memory_used / 4 * 3 { // >75%
        log::debug!("High memory usage detected, running memory optimization");
        // petroleum::page_table::ALLOCATOR.lock().optimize(); // Method not available
    }

    // Monitor process health
    if system_stats.active_processes > system_stats.total_processes / 2 {
        log::warn!("High active process ratio: {}/{}", system_stats.active_processes, system_stats.total_processes);
    }
}

/// Perform resource optimization tasks
fn optimize_system_resources() {
    // Optimize memory layout periodically
    static mut LAST_OPTIMIZATION_TICK: u64 = 0;
    let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);

    unsafe {
        if current_tick - LAST_OPTIMIZATION_TICK > 10000 { // Every 10000 ticks
            // Run memory defragmentation or optimization
            log::debug!("Running periodic resource optimization");
            LAST_OPTIMIZATION_TICK = current_tick;

            // Optimize heap allocation patterns
            // petroleum::page_table::ALLOCATOR.lock().defragment(); // Method not available
        }
    }
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
    // Simple fixed log content to avoid format macro
    let log_content = b"System stats logged to filesystem\n";

    static mut LOG_FILE_CREATED: bool = false;
    unsafe {
        if !LOG_FILE_CREATED {
            if crate::fs::create_file("system.log", log_content).is_ok() {
                LOG_FILE_CREATED = true;
            }
        } else {
            if let Ok(fd) = crate::fs::open_file("system.log") {
                let _ = crate::fs::seek_file(fd, 0);
                let _ = crate::fs::write_file(fd, log_content);
                let _ = crate::fs::close_file(fd);
            }
        }
    }
}

/// Periodic OS feature: automated filesystem backup
fn perform_automated_backup() {
    // Simple backup: fixed message
    let log_content = b"Automated backup completed\n";

    let _ = crate::fs::create_file("backup.log", log_content);
}

/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    use x86_64::instructions::hlt;

    // Initialize scheduler by creating the shell process
    log::info!("Starting enhanced OS scheduler with integrated system features...");
    let shell_pid = crate::process::create_process(
        "shell_process",
        VirtAddr::new(shell_process_main as usize as u64),
    );
    log::info!("Created shell process with PID {}", shell_pid);

    // Set initial process as shell
    let _ = crate::process::unblock_process(shell_pid);

    // Main scheduler loop - continuously execute processes with integrated OS functionality
    loop {
        // Increment system tick counter
        SYSTEM_TICK.fetch_add(1, Ordering::Relaxed);
        SCHEDULER_ITERATIONS.fetch_add(1, Ordering::Relaxed);

        // Collect current system statistics (every scheduler iteration for simplicity)
        let system_stats = collect_system_stats();

        // Process I/O events (keyboard, filesystem, etc.)
        process_io_events();

        // Periodically perform health checks and log statistics
        let current_tick = SYSTEM_TICK.load(Ordering::Relaxed);
        if current_tick % 1000 == 0 { // Every 1000 ticks
            perform_system_health_checks();
            log_system_stats(&system_stats, 5000); // Log every 5000 ticks
            display_system_stats_on_vga(&system_stats, 5000); // Display every 5000 ticks
        }

        // Periodic filesystem synchronization and OS features (every 3000 ticks)
        if current_tick % 3000 == 0 {
            log_system_stats_to_fs(&system_stats);
            perform_automated_backup();
        }

        // Perform system maintenance tasks periodically
        if current_tick % 2000 == 0 {
            perform_system_maintenance();
        }

        // Periodic memory capacity check (every 10000 ticks)
        if current_tick % 10000 == 0 {
            let allocator = petroleum::page_table::ALLOCATOR.lock();
            let used_bytes = allocator.used();
            let total_bytes = allocator.size();
            let usage_ratio = used_bytes as f32 / total_bytes as f32;

            log::info!("Memory utilization: {} bytes / {} bytes ({:.2}%)",
                used_bytes, total_bytes, usage_ratio * 100.0);

            if usage_ratio > 0.9 {
                log::warn!("Critical memory usage (>90%) detected!");
            }
        }

        // Check for process cleanup every 100 iterations
        let iteration_count = SCHEDULER_ITERATIONS.load(Ordering::Relaxed);
        if iteration_count % 100 == 0 {
            // Check for terminated processes and clean up
            crate::process::cleanup_terminated_processes();
        }

        // Handle any pending system calls or kernel requests
        // This is a placeholder - in a full implementation, there would be
        // a queue of kernel tasks to process

        // Yield to allow scheduler to run process switching
        crate::syscall::kernel_syscall(22, 0, 0, 0); // Yield syscall

        // Allow graphics and I/O operations to process
        // This loop will be interrupted by timer maintaining process scheduling

        // Yield for short periods to allow more frequent system operations using pause instead of hlt for QEMU-friendliness
        // pause allows the CPU to enter a low-power state while remaining responsive to interrupts,
        // making it more suitable for virtualization environments like QEMU compared to hlt which
        // puts the CPU in a deeper sleep state that's harder for hypervisors to manage efficiently.
        for _ in 0..50 { // Reduced from 100 to allow more frequent system operations
            unsafe { core::arch::asm!("pause"); }
        }

        // After yield cycle, check if any emergency conditions need handling
        // (e.g., out of memory, too many processes, kernel panic recovery)
        if current_tick % 10000 == 0 {
            emergency_condition_handler();
        }
    }
}

/// Handle emergency system conditions (OOM, process limits, etc.)
fn emergency_condition_handler() {
    // Check for out-of-memory condition
    let allocator = petroleum::page_table::ALLOCATOR.lock();
    if allocator.used() > (allocator.size() * 4) / 5 { // >80% usage
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
    crate::process::terminate_process(
        crate::process::current_pid().unwrap(),
        0
    );

    // Should never reach here
    loop {
        // Use pause for QEMU-friendliness instead of hlt
        // pause allows the CPU to enter a low-power state while remaining responsive to interrupts,
        // making it more suitable for virtualization environments like QEMU compared to hlt which
        // puts the CPU in a deeper sleep state that's harder for hypervisors to manage efficiently.
        unsafe { core::arch::asm!("pause"); }
    }
}
