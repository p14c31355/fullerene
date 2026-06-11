//! GUI subsystem — bridged to [`solvent`] runtime.
//!
//! This file serves as a thin bridge layer between kernel framebuffer
//! management and the Solvent runtime. All GUI/rendering logic lives
//! in `solvent`; this module only provides framebuffer access and
//! GPU present/flush, which are kernel-owned responsibilities.
//!
//! # Architecture
//!
//! ```text
//! Kernel (framebuffer memory, GPU present)
//!     ↓
//! gui.rs (framebuffer access, present/flush)
//!     ↓
//! Solvent (desktop state, compositor, events, timers)
//!     ↓
//! Lattice / Nozzle / Resonance / ChronoLine
//! ```

use petroleum::graphics::Renderer;
use solvent;

// Re-export solvent types used by other kernel modules
pub use solvent::{
    LatticeTerminal, MOUSE_STATE, MouseState, chrono_tick, is_initialized, poll_mouse_state,
    process_events, push_key_event, set_render_fn, write_terminal,
};

/// Initialise the GUI subsystem via Solvent runtime.
pub fn init() {
    // Register the kernel heap extension callback so that solvent can
    // request dynamic heap expansion when resizing terminal surfaces.
    solvent::set_heap_extend_fn(|additional| unsafe {
        crate::heap::extend_kernel_heap(additional)
    });

    // Register the wall-clock callback (CMOS RTC).
    solvent::set_wall_clock_fn(read_cmos_time);

    // Register VFS readdir callback — bridges the kernel VFS to solvent.
    solvent::set_vfs_readdir_fn(|path| {
        let entries = crate::vfs::readdir(path).map_err(|e| {
            // log::warn!("VFS readdir: {} → {}", path, e);
            e
        })?;
        let mut result = alloc::vec::Vec::new();
        for vn in entries {
            result.push(solvent::VfsEntry {
                name: vn.name,
                size: vn.size,
                is_dir: vn.is_dir,
            });
        }
        Ok(result)
    });

    // Register process list callback — bridges process manager to solvent.
    solvent::set_process_list_fn(|| {
        let mut result = alloc::vec::Vec::new();
        crate::process::PROCESS_MANAGER.with_list(|list| {
            for (pid, proc) in list.iter() {
                let state = match proc.state {
                    crate::process::ProcessState::Ready => solvent::ProcessStateKind::Ready,
                    crate::process::ProcessState::Running => solvent::ProcessStateKind::Running,
                    crate::process::ProcessState::Blocked => solvent::ProcessStateKind::Blocked,
                    crate::process::ProcessState::Terminated => {
                        solvent::ProcessStateKind::Terminated
                    }
                };
                result.push(solvent::ProcessEntry {
                    pid: pid.0,
                    name: alloc::string::String::from(proc.name),
                    state,
                });
            }
        });
        result
    });

    // Register device list callback — bridges device manager to solvent.
    solvent::set_device_list_fn(|| {
        let mut result = alloc::vec::Vec::new();
        if let Some(mgr) = crate::hardware::device_manager::get_device_manager()
            .lock()
            .as_ref()
        {
            for di in mgr.list_devices() {
                result.push(solvent::DeviceEntry {
                    name: alloc::string::String::from(di.name),
                    dev_type: alloc::string::String::from(di.device_type),
                    enabled: di.enabled,
                });
            }
        }
        result
    });

    // Calibrate TSC ticks per millisecond using the PIT (8254).
    // PIT channel 2 is free‑running and connected to the speaker
    // gate, so we can read its counter without disturbing audio.
    let tsc_per_ms = calibrate_tsc_with_pit();
    petroleum::serial::serial_log(format_args!(
        "TSC calibration: {} ticks/ms (~{:.1} GHz)\n",
        tsc_per_ms,
        tsc_per_ms as f64 / 1_000_000.0,
    ));
    solvent::set_tsc_per_ms(tsc_per_ms);

    solvent::init();
    petroleum::serial::serial_log(format_args!("solvent::init() completed\n"));
}

/// Render the desktop onto the primary framebuffer.
///
/// Bridged from solvent, providing kernel-owned framebuffer access.
pub fn render() {
    // Render via solvent with framebuffer access from kernel
    solvent::render(get_framebuffer_slice);

    // Signal present & flush GPU (kernel-owned resource management)
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        renderer.present();
    }
    drop(renderer_lock);
    crate::graphics::flush_gpu();
}

/// Perform one tick of the runtime loop with kernel framebuffer access.
///
/// This wraps `solvent::runtime_tick` with the kernel framebuffer callback.
pub fn runtime_tick(now: u64) {
    solvent::runtime_tick(now, get_framebuffer_slice);

    // Signal present & flush GPU
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        renderer.present();
    }
    drop(renderer_lock);
    crate::graphics::flush_gpu();
}

// ── Framebuffer access (kernel-internal) ─────────────────────

/// Get a mutable slice of the framebuffer pixels and its dimensions.
fn get_framebuffer_slice() -> Option<(&'static mut [u32], u32, u32)> {
    let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    let renderer = renderer_lock.as_ref()?;
    let info = renderer.get_info();

    let fb_ptr = info.address as *mut u32;
    let fb_len = (info.width as usize) * (info.height as usize);

    let fb_pixels = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    Some((fb_pixels, info.width, info.height))
}

// ── Wall clock (CMOS RTC) ────────────────────────────────────

/// Read a CMOS register.
fn cmos_read(reg: u8) -> u8 {
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x70).write(reg);
        x86_64::instructions::port::PortReadOnly::<u8>::new(0x71).read()
    }
}

/// Convert a BCD value to binary if the RTC is in BCD mode.
fn bcd_to_bin(bcd: u8) -> u8 {
    (bcd & 0x0F) + ((bcd >> 4) * 10)
}

/// Read wall-clock time from the CMOS RTC.
///
/// Returns `Some((year, month, day, hour, minute, second))` on success.
fn read_cmos_time() -> Option<(u16, u8, u8, u8, u8, u8)> {
    // Wait for update-in-progress flag to clear before reading
    let mut timeout = 0;
    while (cmos_read(0x0A) & 0x80 != 0) && timeout < 10000 {
        timeout += 1;
    }

    let status_b = cmos_read(0x0B);
    let use_bcd = status_b & 0x04 == 0;

    let mut second = cmos_read(0x00);
    let mut minute = cmos_read(0x02);
    let mut hour = cmos_read(0x04);
    let mut day = cmos_read(0x07);
    let mut month = cmos_read(0x08);
    let mut year_raw = cmos_read(0x09);

    // Handle hour format: status B bit 1 SET means 24-hour mode, CLEAR means 12-hour
    // In 12-hour mode, bit 7 of hour indicates PM
    let is_12hour = status_b & 0x02 == 0;
    let pm = is_12hour && (hour & 0x80 != 0);

    // Clear PM bit before BCD decode
    hour &= 0x7F;

    // Decode BCD if needed
    if use_bcd {
        hour = bcd_to_bin(hour);
    }

    // Convert 12-hour to 24-hour if needed
    if is_12hour {
        if pm && hour != 12 {
            hour += 12;
        }
        if !pm && hour == 12 {
            hour = 0;
        }
    }

    if use_bcd {
        second = bcd_to_bin(second);
        minute = bcd_to_bin(minute);
        day = bcd_to_bin(day);
        month = bcd_to_bin(month);
        year_raw = bcd_to_bin(year_raw);
    }

    // Century from register 0x32 (if available).
    // Register 0x32 typically holds the century as 2-digit BCD (e.g. 0x20 for 20xx).
    // The century value IS the full century representation, so e.g. 20 × 100 + 26 = 2026.
    // Do NOT add an additional 2000 offset when the century register is present.
    let century = cmos_read(0x32);
    let full_year = if century != 0 {
        let c = if use_bcd {
            bcd_to_bin(century) as u16
        } else {
            century as u16
        };
        c * 100 + year_raw as u16
    } else {
        // No century register — assume 2000+ as fallback
        2000u16 + year_raw as u16
    };

    // Return raw UTC.  Timezone offset is applied in solvent::update_clock()
    // so the user can change it at runtime via the AppGrid.
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    Some((full_year, month, day, hour, minute, second))
}

// ── TSC calibration via PIT channel 2 ────────────────────────

/// Measure TSC ticks per millisecond using the PIT channel 2
/// (which is left free‑running by the PC speaker code).
///
/// Channel 2 is configured in rate‑generator mode by the BIOS
/// with divisor 0 (effectively 65536), giving ~18.2 Hz.
/// We read the LATCH command → current count twice to measure
/// elapsed time.
fn calibrate_tsc_with_pit() -> u64 {
    // Read current count from PIT channel 2 via latch command.
    fn pit_read_count() -> Option<u16> {
        unsafe {
            // Latch counter for channel 2 using standard Counter Latch Command (0x80)
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0x80);
            // Read low then high byte
            let lo = x86_64::instructions::port::PortReadOnly::<u8>::new(0x42).read();
            let hi = x86_64::instructions::port::PortReadOnly::<u8>::new(0x42).read();
            let count = u16::from_le_bytes([lo, hi]);
            // 0 means the counter wrapped — valid for our decay count.
            Some(count)
        }
    }

    let t0 = unsafe { core::arch::x86_64::_rdtsc() };
    let c0 = match pit_read_count() {
        Some(c) => c,
        None => return 3_000_000, // PIT unavailable — fall back to 3 GHz
    };

    // Measure 20 ms of PIT ticks (23864 counts at 1.193182 MHz).
    // This is more robust than waiting for a full wrap, which can be
    // missed if the VM or CPU is preempted for >55 ms.
    let target_ticks: u16 = 23864;
    loop {
        let cur = match pit_read_count() {
            Some(c) => c,
            None => return 3_000_000,
        };
        let elapsed = c0.wrapping_sub(cur);
        if elapsed >= target_ticks {
            break;
        }
        // TSC watchdog: 1 second timeout at 3 GHz
        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0) > 3_000_000_000 {
            return 3_000_000; // stalled
        }
        core::hint::spin_loop();
    }

    let ticks = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0);
    let ms_elapsed = 20u64;
    let result = ticks / ms_elapsed;
    // Sanity check: reject values outside 100 MHz … 10 GHz.
    if result < 100_000 || result > 10_000_000 {
        petroleum::serial::serial_log(format_args!(
            "TSC PIT calib rejected ({:.1} GHz), using fallback\n",
            result as f64 / 1_000_000.0,
        ));
        return 3_000_000;
    }
    result
}
