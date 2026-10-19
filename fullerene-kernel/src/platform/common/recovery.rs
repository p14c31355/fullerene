//! The cross-terminal boot/USB recovery lanes (extracted from the
//! ane_usb_dev.rs monolith in the v331 refactor).
//!
//! The recovery lanes are terminal-generic: any SoC port with a timer
//! pump and an EP0-driven responder can adopt them by feeding
//! [`boot_recovery_check`] from its timer IRQ and [`boot_heartbeat`]
//! from its wait loop. The SoC-specific parts (what "reset" means, what
//! the EP0-success counter is fed by) stay on the platform side.

use core::sync::atomic::Ordering;

/// v330 recovery: the boot thread's heartbeat. The wait loop bumps this
/// every pass; the timer pump checks it - a stalled heartbeat with the
/// pump alive means the boot thread is wedged, and the pump schedules the
/// v63 loader reset (the no-combo recovery window) instead of leaving the
/// device unreachable until a power hold.
pub static BOOT_HEARTBEAT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// v331b: incremented by ep0_send on every SUCCESSFUL EP0 completion
/// (the endpoint armed and the transfer retired). Frozen success +
/// rising stuck = the USB zombie.
pub static EP0_OK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// v331b: how many consecutive frozen-boot resets were scheduled this
/// boot (the diagnostic counter for the ring).
pub static RECOVERY_RESETS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// v330 recovery: called by the wait loop every pass.
pub fn boot_heartbeat() {
    BOOT_HEARTBEAT.fetch_add(1, Ordering::Relaxed);
}

/// v331b: the USB zombie lane. Runs only while the boot thread is alive
/// (the frozen-boot lane owns the wedge case). If no EP0 transfer
/// completed for 54000 beats (~15 min) while the pump keeps seeing EPENA
/// stalls, the link cannot recover by itself - schedule the loader reset.
fn usb_zombie_check(beats: u32) {
    static LAST_OK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    static ZOMBIE_AT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    // A hold is not a zombie: EP0 is silent because the boot is parked on
    // purpose. Resetting here would throw away the evidence the hold exists
    // to show - exactly what happened at 22:29 tonight (blackbox.rs:1184).
    if crate::blackbox::panel_hold() {
        ZOMBIE_AT.store(0, Ordering::Relaxed);
        return;
    }
    let ok = EP0_OK.load(Ordering::Relaxed);
    let last = LAST_OK.load(Ordering::Relaxed);
    if ok != last {
        LAST_OK.store(ok, Ordering::Relaxed);
        ZOMBIE_AT.store(0, Ordering::Relaxed);
        return;
    }
    let at = ZOMBIE_AT.load(Ordering::Relaxed);
    if at == 0 {
        ZOMBIE_AT.store(beats, Ordering::Relaxed);
        return;
    }
    if beats.wrapping_sub(at) > 54000 {
        schedule_loader_reset("WD: USB ZOMBIE RESET\n");
        ZOMBIE_AT.store(beats, Ordering::Relaxed); // once per window
    }
}

/// The one place that schedules the v63 loader reset. The callers pass
/// the ring marker; the reset itself is the platform's panic path.
fn schedule_loader_reset(marker: &str) {
    crate::blackbox::text_puts(marker);
    RECOVERY_RESETS.fetch_add(1, Ordering::Relaxed);
    crate::blackbox::schedule_reset_at(crate::timer::uptime_us());
}

/// v330 recovery: pump-side check. Runs from the timer IRQ every tick:
/// if the heartbeat has not moved for 7200 beats (~10 min at the observed
/// 12 beats/s), schedule the loader reset. The 10 min window is far above
/// the storage probe and the desktop render's legitimate stall times.
pub fn boot_recovery_check(beats: u32) {
    static LAST_SEEN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    static FROZEN_AT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    let hb = BOOT_HEARTBEAT.load(Ordering::Relaxed);
    let last = LAST_SEEN.load(Ordering::Relaxed);
    if hb != last {
        // The boot thread is alive.
        LAST_SEEN.store(hb, Ordering::Relaxed);
        FROZEN_AT.store(0, Ordering::Relaxed);
        // v331b: also check the USB liveness lane - a live boot thread with
        // a permanently wedged EP0 (the host sees a zombie: descriptor
        // reads hang, no flash window) is unrecoverable without a reset.
        usb_zombie_check(beats);
        return;
    }
    // The heartbeat is frozen: the boot thread has not run the wait loop.
    let frozen_at = FROZEN_AT.load(Ordering::Relaxed);
    if frozen_at == 0 {
        FROZEN_AT.store(beats, Ordering::Relaxed);
        crate::blackbox::text_puts("WD: BOOT FROZEN\n");
        return;
    }
    if beats.wrapping_sub(frozen_at) > 7200 {
        // ~10 min of a frozen boot thread with the pump alive. Schedule the
        // v63 loader reset: the unmanned engine reflashes, the device
        // recovers without a power hold.
        schedule_loader_reset("WD: BOOT FROZEN RESET\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_is_relaxed_counter() {
        // The heartbeat must be a plain counter (no cross-state coupling).
        let before = BOOT_HEARTBEAT.load(Ordering::Relaxed);
        boot_heartbeat();
        assert_eq!(BOOT_HEARTBEAT.load(Ordering::Relaxed), before + 1);
    }
}
