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

use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::Renderer;
use solvent;

// Re-export solvent types used by other kernel modules
pub use solvent::{
    LatticeTerminal, MOUSE_STATE, MouseState, chrono_tick, consume_frame_due, cursor_update_due,
    is_initialized, poll_mouse_state, process_events, push_key_event, set_render_fn, tick_core,
    write_terminal,
};

/// Initialise the GUI subsystem via Solvent runtime.
pub fn init() {
    // Install all kernel→solvent callbacks at once.
    solvent::SolventCallbacks {
        heap_extend: Some(|additional| unsafe { crate::heap::extend_kernel_heap(additional) }),
        wall_clock: Some(read_cmos_time),
        vfs_readdir: Some(|path| {
            let entries = crate::contexts::vfs::readdir(path)?;
            let mut result = alloc::vec::Vec::new();
            for vn in entries {
                result.push(solvent::VfsEntry {
                    name: vn.name,
                    size: vn.size,
                    is_dir: vn.is_dir,
                });
            }
            Ok(result)
        }),
        vfs_read: Some(|path| {
            let fd = crate::contexts::vfs::open(path, 0)?;
            let mut buf = alloc::vec::Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                match crate::contexts::vfs::read(fd.fd, &mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(e) => {
                        let _ = crate::contexts::vfs::close(fd.fd);
                        return Err(e);
                    }
                }
            }
            let _ = crate::contexts::vfs::close(fd.fd);
            Ok(buf)
        }),
        vfs_write: Some(|path, data| crate::contexts::vfs::replace_file(path, data)),
        vfs_copy: Some(|source, destination, is_dir| {
            crate::contexts::vfs::copy_path(source, destination, is_dir)
        }),
        vfs_move: Some(|source, destination, is_dir| {
            crate::contexts::vfs::move_path(source, destination, is_dir)
        }),
        vfs_remove: Some(|path, is_dir| crate::contexts::vfs::remove_path(path, is_dir)),
        process_list: Some(|| {
            let mut result = alloc::vec::Vec::new();
            crate::process::SCHEDULER.with_list(|list| {
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
        }),
        device_list: Some(|| {
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
        }),
        mounted_drive_list: Some(|| {
            crate::contexts::vfs::with_vfs(|vfs| vfs.mounted_block_devices()).unwrap_or_default()
        }),
        usb_poll: Some(|| crate::drivers::registry::poll_usb()),
        shell_cmd: None,
        launch_shell: Some(|| {
            crate::scheduler::request_shell_launch();
        }),
        settings_save: None,
        kernel_log: Some(|| {
            alloc::string::String::from_utf8_lossy(&crate::klog::snapshot()).into_owned()
        }),
        metrics: Some(crate::metrics::format_snapshot),
    }
    .install();

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

    solvent::set_render_progress_fn(crate::boot_stage::draw_boot_label);
    solvent::init();
    petroleum::serial::serial_log(format_args!("solvent::init() completed\n"));

    crate::interrupts::apic::register_mmio_watchdog();
    petroleum::serial::serial_log(format_args!("MMIO NMI watchdog registered\n"));

    #[cfg(not(nitrogen_no_iwlwifi))]
    {
        solvent::register_wifi_service();
        petroleum::serial::serial_log(format_args!("wifi service registered\n"));
    }
}

/// Once the first frame renders successfully, disable the boot-screen
/// progress callback so `solvent::render` doesn't keep drawing labels
/// over the desktop on every frame.
static BOOT_PROGRESS_DONE: AtomicBool = AtomicBool::new(false);

/// Signal present and flush GPU after rendering.
fn finish_frame() {
    crate::contexts::kernel::with_kernel_mut(|k| {
        if let Some(ref mut renderer) = k.framebuffer.renderer {
            renderer.present();
        }
    });
    crate::graphics::flush_gpu();
}

pub fn render() {
    let frame_start = unsafe { core::arch::x86_64::_rdtsc() };
    // Draw progress labels before acquiring the FramebufferGuard,
    // to avoid mutable aliasing of the framebuffer slice.
    if !BOOT_PROGRESS_DONE.load(Ordering::Relaxed) {
        crate::boot_stage::draw_boot_label(b"RENDERING...");
        crate::boot_stage::draw_boot_label(b"RENDER via guard");
        // Disable the progress callback: subsequent labels drawn from within
        // solvent::render() would race with the &mut [u32] in FramebufferGuard.
        solvent::set_render_progress_fn(|_| {});
    }

    if crate::contexts::framebuffer::with_framebuffer(|fb| solvent::render(fb)).is_none() {
        crate::boot_stage::draw_boot_label(b"RENDER: framebuffer unavailable");
    }

    BOOT_PROGRESS_DONE.store(true, Ordering::Release);
    finish_frame();
    crate::metrics::record_frame_ticks(
        unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(frame_start),
    );
}

/// Perform one tick of the runtime loop with kernel framebuffer access.
///
/// The tick is split into two phases to avoid a deadlock with
/// `spin::Mutex` (which is non-recursive):
///
/// 1. **tick_core** — input polling, event processing, timer updates.
///    This runs **without** the `KERNEL` lock so that event handlers
///    (e.g. file manager opening, shell commands) can call back into
///    VFS → `KERNEL.lock()` without self-deadlocking.
///
/// 2. **render** — framebuffer rendering under the `KERNEL` lock.
///    Full-scene and cursor-only requests both borrow a `FramebufferGuard`.
pub fn runtime_tick(now: u64) {
    solvent::tick_core(now);
    let full_frame = solvent::consume_frame_due();
    let cursor_only = !full_frame && solvent::cursor_update_due();
    if full_frame || cursor_only {
        let frame_start = unsafe { core::arch::x86_64::_rdtsc() };
        let rendered = crate::contexts::framebuffer::with_framebuffer(|framebuffer| {
            if full_frame {
                solvent::render(framebuffer);
            } else {
                solvent::render_cursor_fast(framebuffer);
            }
        });
        if rendered.is_none() {
            crate::boot_stage::draw_boot_label(b"RENDER: framebuffer unavailable");
        }
        finish_frame();
        crate::metrics::record_frame_ticks(
            unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(frame_start),
        );
    }
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
    // Ensure PIT channel 2 gate is enabled (bit 0 of System Control Port B
    // at 0x61).  The BIOS may leave it disabled, causing the counter to
    // stall and the calibration to fall back to 3 GHz.
    let original_61 = unsafe { x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read() };
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(original_61 | 0x01);
    }

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
        None => {
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(original_61);
            }
            return 3_000_000;
        }
    };

    // Early PIT stutter test: if the counter doesn't budge after ~500 µs,
    // the 8254 is not running (no emulation / chipset quirk).  Bail fast
    // rather than spin for 1 full second waiting for 20 ms of ticks.
    let c1 = match pit_read_count() {
        Some(c) => c,
        None => return 3_000_000,
    };
    let mut stutter_ok = c0 != c1;
    if !stutter_ok {
        // Wait ~500 µs and retry once
        let t_stutter = unsafe { core::arch::x86_64::_rdtsc() };
        while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t_stutter) < 500_000 {
            core::hint::spin_loop();
        }
        let c2 = match pit_read_count() {
            Some(c) => c,
            None => return 3_000_000,
        };
        stutter_ok = c0 != c2;
    }
    if !stutter_ok {
        // PIT counter is frozen — no PIT emulation on this platform.
        unsafe {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(original_61);
        }
        petroleum::serial::serial_log(format_args!(
            "TSC PIT calib: PIT stutter test failed (no 8254?), using 3 GHz fallback\n"
        ));
        return 3_000_000;
    }

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
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(original_61);
            }
            return 3_000_000; // stalled
        }
        core::hint::spin_loop();
    }

    // Restore original PIT gate state
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(original_61);
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
