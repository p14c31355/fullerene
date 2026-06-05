//! APIC (Advanced Programmable Interrupt Controller) handling
//!
//! This module provides APIC initialization and management functions.

use nitrogen::apic::{ApicFlags, ApicOffsets, IO_APIC_BASE};
use nitrogen::pic::disable_legacy_pic;
use petroleum::common::utils::reset_mutex_lock;
use petroleum::init_io_apic;
use spin::Mutex;
use x86_64::registers::model_specific::Msr;

/// Hardware interrupt vectors
pub const TIMER_INTERRUPT_INDEX: u32 = 32;
pub const KEYBOARD_INTERRUPT_INDEX: u32 = 33;
pub const MOUSE_INTERRUPT_INDEX: u32 = 44;

/// APIC raw access structure
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ApicRaw {
    base_addr: u64,
}

impl ApicRaw {
    /// Read from APIC register
    ///
    /// # Safety
    /// This is safe because the base_addr is validated during initialization
    /// and the offset is a known APIC register offset.
    fn read(&self, offset: u32) -> u32 {
        let addr = (self.base_addr + offset as u64) as *const u32;
        unsafe { core::ptr::read_volatile(addr) }
    }

    /// Write to APIC register
    ///
    /// # Safety
    /// This is safe because the base_addr is validated during initialization
    /// and the offset is a known APIC register offset.
    fn write(&self, offset: u32, value: u32) {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        unsafe { core::ptr::write_volatile(addr, value) }
    }
}

/// Global APIC instance
pub static APIC: Mutex<Option<ApicRaw>> = Mutex::new(None);

/// Get APIC base address
fn get_apic_base() -> Option<u64> {
    let value = unsafe { Msr::new(ApicOffsets::BASE_MSR).read() };
    if value & (1 << 11) != 0 {
        Some(value & ApicOffsets::BASE_ADDR_MASK)
    } else {
        None
    }
}

/// Enable APIC
fn enable_apic(apic: &mut ApicRaw) {
    let spurious = apic.read(ApicOffsets::SPURIOUS_VECTOR);
    apic.write(
        ApicOffsets::SPURIOUS_VECTOR,
        spurious | ApicFlags::SW_ENABLE | 0xFF,
    );
}

/// Send End-Of-Interrupt to APIC
pub fn send_eoi() {
    if let Some(apic) = APIC.lock().as_ref() {
        apic.write(ApicOffsets::EOI, 0);
    }
}

/// Hardware-only APIC initialisation (called BEFORE IDT/ISRs are ready).
///
/// Masks all Local APIC LVT entries, disables the legacy PIC, and enables
/// the Local APIC in software so that MSI/MSI-X interrupts from PCI devices
/// (e.g. VirtIO-GPU after SET_SCANOUT) are safely suppressed.  This function
/// does NOT configure the timer or IO APIC; those are set up later by
/// [`init_apic`].
pub fn init_apic_hw_only() {
    petroleum::serial::serial_log(format_args!("[init_apic_hw_only] Masking APIC LVTs early\n"));

    unsafe {
        reset_mutex_lock(&petroleum::LOCAL_APIC_ADDRESS);
    }

    let base_addr = {
        let lapic_addr_lock = petroleum::LOCAL_APIC_ADDRESS.lock();
        let ptr = lapic_addr_lock.0;
        if !ptr.is_null() {
            ptr as u64
        } else {
            // Fallback: query MSR and compute virtual address
            let phys = get_apic_base().unwrap_or(0xFEE00000);
            phys + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64
        }
    };

    if base_addr < 0xFFFF_8000_0000_0000 || (base_addr & 0xFFF) != 0 {
        petroleum::serial::serial_log(format_args!(
            "[init_apic_hw_only] Invalid APIC base {:#x}, skipping\n", base_addr
        ));
        return;
    }

    disable_legacy_pic();
    petroleum::serial::serial_log(format_args!("[init_apic_hw_only] Legacy PIC disabled\n"));

    let apic = ApicRaw { base_addr };

    // Enable software APIC (spurious vector)
    let spurious = apic.read(ApicOffsets::SPURIOUS_VECTOR);
    apic.write(
        ApicOffsets::SPURIOUS_VECTOR,
        spurious | ApicFlags::SW_ENABLE | 0xFF,
    );

    // Mask ALL LVT entries — any unmasked entry could trigger a spurious
    // interrupt while the IDT handlers are not yet registered.
    let lvt_mask: u32 = 1 << 16;
    apic.write(ApicOffsets::LVT_LINT0, lvt_mask);
    apic.write(ApicOffsets::LVT_LINT1, lvt_mask);
    apic.write(ApicOffsets::LVT_ERROR, lvt_mask);
    apic.write(ApicOffsets::LVT_PERF_COUNT, lvt_mask);
    apic.write(ApicOffsets::LVT_THERMAL, lvt_mask);
    apic.write(ApicOffsets::LVT_TIMER, lvt_mask | 0x10000); // masked + one-shot
    apic.write(ApicOffsets::TMRDIV, 0x3);
    apic.write(ApicOffsets::TMRINITCNT, 0); // Stop the timer entirely

    petroleum::serial::serial_log(format_args!(
        "[init_apic_hw_only] All LVTs masked, APIC enabled (timer stopped)\n"
    ));
}

/// Initialize APIC
///
/// # Safety
///
/// This function must be called AFTER the IDT is set up (via interrupts::init())
/// and AFTER interrupt handlers are registered.  On real hardware (InsydeH2O),
/// calling this before IDT setup can cause a triple fault if a spurious
/// interrupt arrives.
pub fn init_apic() {
    petroleum::serial::serial_log(format_args!("Initializing APIC...\n"));

    // Force reset APIC lock state to 0 to handle cases where .bss is not cleared
    unsafe {
        reset_mutex_lock(&APIC);
        reset_mutex_lock(&petroleum::LOCAL_APIC_ADDRESS);
        petroleum::serial::serial_log(format_args!(
            "DEBUG: [init_apic] APIC and LOCAL_APIC_ADDRESS locks reset to 0\n"
        ));
    }

    disable_legacy_pic();
    petroleum::serial::serial_log(format_args!("Legacy PIC disabled.\n"));

    let base_addr = {
        let lapic_addr_lock = petroleum::LOCAL_APIC_ADDRESS.lock();
        let ptr = lapic_addr_lock.0;
        if !ptr.is_null() {
            let addr = ptr as u64;
            petroleum::serial::serial_log(format_args!(
                "DEBUG: [init_apic] Using pre-mapped LAPIC at {:#x}\n", addr
            ));
            addr
        } else {
            // Fallback: query MSR and compute virtual address
            let phys = get_apic_base().unwrap_or(0xFEE00000);
            let virt = phys + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;
            petroleum::serial::serial_log(format_args!(
                "DEBUG: [init_apic] Using MSR-discovered LAPIC: phys={:#x} virt={:#x}\n",
                phys, virt
            ));
            virt
        }
    };

    // Validate the APIC base address before accessing it.
    // A null or misaligned address means MMIO mapping failed earlier.
    if base_addr < 0xFFFF_8000_0000_0000 || (base_addr & 0xFFF) != 0 {
        petroleum::serial::serial_log(format_args!(
            "ERROR: [init_apic] Invalid APIC base address {:#x} — MMIO mapping may be missing\n",
            base_addr
        ));
        // Don't initialize APIC with a bad address; fall through to
        // scheduler loop without timer interrupts (system will still
        // work via cooperative scheduling).
        return;
    }

    let mut apic = ApicRaw { base_addr };
    enable_apic(&mut apic);

    // CRITICAL: Mask all LVT entries BEFORE programming the timer.
    // On some hardware (InsydeH2O), unmasked LVT entries can trigger
    // spurious interrupts during initialization.
    //
    // LVT LINT0, LINT1, Error, Performance Counters
    let lvt_mask: u32 = 1 << 16; // Mask bit
    apic.write(ApicOffsets::LVT_LINT0, lvt_mask);
    apic.write(ApicOffsets::LVT_LINT1, lvt_mask);
    apic.write(ApicOffsets::LVT_ERROR, lvt_mask);
    apic.write(ApicOffsets::LVT_PERF_COUNT, lvt_mask);
    // Thermal sensor LVT (if supported)
    apic.write(ApicOffsets::LVT_THERMAL, lvt_mask);

    petroleum::serial::serial_log(format_args!("APIC LVT entries masked.\n"));

    // Configure timer LVT: one-shot mode initially to prevent runaway interrupts.
    // The timer will be reprogrammed later if periodic mode is needed.
    apic.write(
        ApicOffsets::LVT_TIMER,
        TIMER_INTERRUPT_INDEX | ApicFlags::TIMER_ONESHOT,
    );
    apic.write(ApicOffsets::TMRDIV, 0x3); // Divide by 16

    // Program initial count to a reasonable value.
    // The APIC timer frequency is bus-speed dependent.  On InsydeH2O,
    // the bus clock is typically ~100 MHz.  With divide-by-16, one
    // tick = 16 / bus_clock ≈ 160 ns.  1,000,000 ticks ≈ 160 ms.
    // This gives a reasonable periodic rate (~6 Hz) when switched to
    // periodic mode later.
    apic.write(ApicOffsets::TMRINITCNT, 1000000);

    petroleum::serial::serial_log(format_args!(
        "APIC timer configured (one-shot, div=16, initial_count=1000000).\n"
    ));

    *APIC.lock() = Some(apic);

    // Initialize I/O APIC for legacy interrupt routing.
    //
    // SAFETY: On InsydeH2O, the I/O APIC base address is typically
    // 0xFEC00000.  We validate the virtual address is in the higher
    // half before proceeding.
    let io_apic_virt_base =
        IO_APIC_BASE + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;

    if io_apic_virt_base >= 0xFFFF_8000_0000_0000
        && io_apic_virt_base < 0xFFFF_FFFF_FFFF_FFFF
    {
        petroleum::serial::serial_log(format_args!(
            "Initializing I/O APIC at virt={:#x}\n", io_apic_virt_base
        ));
        init_io_apic(base_addr, io_apic_virt_base);
    } else {
        petroleum::serial::serial_log(format_args!(
            "WARNING: I/O APIC virtual address {:#x} out of range — skipping I/O APIC init\n",
            io_apic_virt_base
        ));
    }

    use super::syscall::setup_syscall;
    setup_syscall();
    // Interrupts are enabled in kernel_main_higher_half after all setup is complete,
    // not here, to avoid premature timer interrupts during process creation.
}
