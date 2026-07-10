//! APIC (Advanced Programmable Interrupt Controller) handling
//!
//! This module provides APIC initialization and management functions.
//! All unsafe volatile/port I/O is encapsulated in `nitrogen::apic_controller::ApicController`.

use nitrogen::apic::{ApicFlags, ApicOffsets, IO_APIC_BASE};
use nitrogen::apic_controller::ApicController;
use nitrogen::mmio;
use petroleum::common::utils::reset_mutex_lock;
use spin::Mutex;
use x86_64::instructions;
use x86_64::registers::model_specific::Msr;

/// Hardware interrupt vectors
pub const TIMER_INTERRUPT_INDEX: u32 = 32;
pub const KEYBOARD_INTERRUPT_INDEX: u32 = 33;
pub const MOUSE_INTERRUPT_INDEX: u32 = 44;

/// Global APIC controller instance.
///
/// Set during early boot (UEFI MMIO mapping phase) and then used by
/// `init_apic_hw_only`, `init_apic`, and `send_eoi`.
pub static APIC_CONTROLLER: Mutex<Option<ApicController>> = Mutex::new(None);

/// Get the physical APIC base address from the IA32_APIC_BASE MSR.
fn get_apic_base_phys() -> Option<u64> {
    let value = unsafe { Msr::new(ApicOffsets::BASE_MSR).read() };
    if value & (1 << 11) != 0 {
        Some(value & ApicOffsets::BASE_ADDR_MASK)
    } else {
        None
    }
}

/// Compute the higher-half virtual address from a physical address.
fn phys_to_virt(phys: u64) -> u64 {
    phys + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64
}

/// Pre-initialise the APIC controller with the LAPIC virtual base address.
///
/// Called from `UefiInitContext::map_mmio()` during early boot, before the
/// IDT or interrupt handlers are set up.  This stores the controller in the
/// global static so that `init_apic_hw_only()` can mask LVTs before any
/// PCI device (e.g. VirtIO-GPU) can send MSI/MSI-X interrupts.
pub fn preinit_apic_controller(lapic_virt: u64) {
    unsafe {
        reset_mutex_lock(&APIC_CONTROLLER);
    }

    let ioapic_virt = phys_to_virt(IO_APIC_BASE);

    // SAFETY: The caller guarantees that lapic_virt and ioapic_virt point
    // to valid, mapped MMIO regions in the higher half.
    let controller = unsafe { ApicController::new(lapic_virt, ioapic_virt) };
    *APIC_CONTROLLER.lock() = Some(controller);
}

/// Send End-Of-Interrupt to the Local APIC.
///
/// # Safety
/// Interrupt handlers run with IF=0 (interrupt gate), so the APIC controller
/// lock can never be contended (non-interrupt code that holds the lock runs
/// with IF=1 but cannot be preempted by an interrupt handler on UP).
/// A blocking `lock()` is safe here — `try_lock()` would silently lose EOIs.
pub fn send_eoi() {
    if let Some(ref ctrl) = *APIC_CONTROLLER.lock() {
        ctrl.send_eoi();
    }
}

/// Hardware-only APIC initialisation (called BEFORE IDT/ISRs are ready).
///
/// Masks all Local APIC LVT entries, disables the legacy PIC, and enables
/// the Local APIC in software so that MSI/MSI-X interrupts from PCI devices
/// (e.g. VirtIO-GPU after SET_SCANOUT) are safely suppressed.  This function
/// does NOT configure the timer or I/O APIC; those are set up later by
/// [`init_apic`].
pub fn init_apic_hw_only() {
    petroleum::serial::serial_log(format_args!(
        "[init_apic_hw_only] Masking APIC LVTs early\n"
    ));

    // If the controller hasn't been pre-initialised via map_mmio(), try to
    // create one now using the MSR-discovered physical address.
    let mut guard = APIC_CONTROLLER.lock();
    if guard.is_none() {
        let phys = get_apic_base_phys().unwrap_or(0xFEE00000);
        let lapic_virt = phys_to_virt(phys);
        let ioapic_virt = phys_to_virt(IO_APIC_BASE);

        if lapic_virt >= 0xFFFF_8000_0000_0000 && (lapic_virt & 0xFFF) == 0 {
            // SAFETY: Addresses validated above; MMIO regions are identity-mapped
            // in the higher half by the bootloader.
            let ctrl = unsafe { ApicController::new(lapic_virt, ioapic_virt) };
            *guard = Some(ctrl);
        } else {
            petroleum::serial::serial_log(format_args!(
                "[init_apic_hw_only] Invalid APIC base {:#x}, skipping\n",
                lapic_virt
            ));
            return;
        }
    }

    if let Some(ref ctrl) = *guard {
        ApicController::disable_legacy_pic();
        petroleum::serial::serial_log(format_args!("[init_apic_hw_only] Legacy PIC disabled\n"));

        ctrl.enable();
        ctrl.mask_all_lvts();
        ctrl.lapic_write(ApicOffsets::TMRDIV, 0x3);
        ctrl.lapic_write(ApicOffsets::TMRINITCNT, 0); // Stop the timer entirely

        petroleum::serial::serial_log(format_args!(
            "[init_apic_hw_only] All LVTs masked, APIC enabled (timer stopped)\n"
        ));
    }
}

/// Initialize APIC (called AFTER the IDT and interrupt handlers are set up).
///
/// Configures the timer, unmasks LVTs as appropriate, and sets up I/O APIC
/// routing for legacy IRQs.
pub fn init_apic() {
    petroleum::serial::serial_log(format_args!("Initializing APIC...\n"));

    // Ensure the controller exists (may have been created by preinit or hw_only).
    let mut guard = APIC_CONTROLLER.lock();
    if guard.is_none() {
        let phys = get_apic_base_phys().unwrap_or(0xFEE00000);
        let lapic_virt = phys_to_virt(phys);
        let ioapic_virt = phys_to_virt(IO_APIC_BASE);

        if lapic_virt >= 0xFFFF_8000_0000_0000 && (lapic_virt & 0xFFF) == 0 {
            let ctrl = unsafe { ApicController::new(lapic_virt, ioapic_virt) };
            *guard = Some(ctrl);
        } else {
            petroleum::serial::serial_log(format_args!(
                "ERROR: [init_apic] Invalid APIC base address {:#x} — MMIO mapping may be missing\n",
                lapic_virt
            ));
            return;
        }
    }

    if let Some(ref ctrl) = *guard {
        ApicController::disable_legacy_pic();
        petroleum::serial::serial_log(format_args!("Legacy PIC disabled.\n"));

        ctrl.enable();
        ctrl.mask_all_lvts();

        petroleum::serial::serial_log(format_args!("APIC LVT entries masked.\n"));

        // Configure timer: periodic, unmasked, divide-by-16, ~1ms initial count.
        // Note: on real hardware the actual frequency depends on the bus clock;
        // the scheduler hlt() loop is interrupt-driven so precise timing isn't
        // critical — any periodic tick prevents the permanent hang.
        ctrl.configure_timer(
            TIMER_INTERRUPT_INDEX,
            ApicFlags::TIMER_PERIODIC,
            1_000_000,
            0x3,
        );

        petroleum::serial::serial_log(format_args!(
            "APIC timer configured (periodic, div=16, initial_count=1000000).\n"
        ));

        // Configure I/O APIC for legacy IRQs.
        ctrl.configure_legacy_irqs(KEYBOARD_INTERRUPT_INDEX as u8, MOUSE_INTERRUPT_INDEX as u8);

        petroleum::serial::serial_log(format_args!(
            "I/O APIC legacy IRQs configured (keyboard={}, mouse={}).\n",
            KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX
        ));
    }

    use super::syscall::setup_syscall;
    setup_syscall();
}

// ── MMIO NMI watchdog timer switching ───────────────────────────

const WATCHDOG_NMI_INITIAL_COUNT: u32 = 30_000_000; // ~4.8s at 100MHz bus /16 div

fn arm_watchdog_timer_impl() {
    instructions::interrupts::without_interrupts(|| {
        let guard = APIC_CONTROLLER.lock();
        if let Some(ref ctrl) = *guard {
            let lvt = ctrl.lapic_read(ApicOffsets::LVT_TIMER);
            let initcnt = ctrl.lapic_read(ApicOffsets::TMRINITCNT);
            mmio::watchdog_save_lvt(lvt, initcnt);

            ctrl.lapic_write(
                ApicOffsets::LVT_TIMER,
                ApicFlags::DELIVERY_MODE_NMI | ApicFlags::TIMER_ONESHOT,
            );
            ctrl.lapic_write(ApicOffsets::TMRINITCNT, WATCHDOG_NMI_INITIAL_COUNT);
        }
    });
}

fn restore_watchdog_timer_impl() {
    // NMI-safe: use try_lock to avoid blocking in the NMI handler.
    // If the lock is held, the timer will remain in NMI mode until the next
    // normal-path restore attempt — this is safe as the watchdog has already
    // fired and the system is in recovery.
    if let Some(guard) = APIC_CONTROLLER.try_lock() {
        if let Some(ref ctrl) = *guard {
            let saved_lvt = mmio::watchdog_saved_lvt();
            let saved_initcnt = mmio::watchdog_saved_initcnt();
            // IMPORTANT: Write TMRINITCNT BEFORE LVT_TIMER.
            // At this point TMRINITCNT still holds WATCHDOG_NMI_INITIAL_COUNT.
            // If LVT_TIMER is written first (periodic mode), the timer would start
            // at the stale watchdog count (~4.8s) before the correct count is restored.
            ctrl.lapic_write(ApicOffsets::TMRINITCNT, saved_initcnt);
            ctrl.lapic_write(ApicOffsets::LVT_TIMER, saved_lvt);
        }
    }
}

/// Register the MMIO NMI watchdog timer callbacks with the nitrogen mmio module.
/// Must be called once after APIC init and before WiFi init.
pub fn register_mmio_watchdog() {
    mmio::register_watchdog_timer_callbacks(arm_watchdog_timer_impl, restore_watchdog_timer_impl);
}
