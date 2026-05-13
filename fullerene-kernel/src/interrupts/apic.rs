//! APIC (Advanced Programmable Interrupt Controller) handling
//!
//! This module provides APIC initialization and management functions.

use petroleum::hardware::{pic::disable_legacy_pic, ApicFlags, ApicOffsets, IO_APIC_BASE};
use petroleum::init_io_apic;
use petroleum::common::utils::reset_mutex_lock;
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
        unsafe { addr.read_volatile() }
    }

    /// Write to APIC register
    ///
    /// # Safety
    /// This is safe because the base_addr is validated during initialization
    /// and the offset is a known APIC register offset.
    fn write(&self, offset: u32, value: u32) {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        unsafe { addr.write_volatile(value) }
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

/// Initialize APIC
pub fn init_apic() {
    petroleum::serial::serial_log(format_args!("Initializing APIC...\n"));

    // Force reset APIC lock state to 0 to handle cases where .bss is not cleared
    unsafe {
        reset_mutex_lock(&APIC);
        petroleum::serial::serial_log(format_args!("DEBUG: [init_apic] APIC lock reset to 0\n"));
    }

    disable_legacy_pic();
    petroleum::serial::serial_log(format_args!("Legacy PIC disabled.\n"));

    let base_addr = {
        let lapic_addr_lock = petroleum::LOCAL_APIC_ADDRESS.lock();
        let ptr = lapic_addr_lock.0;
        if !ptr.is_null() {
            ptr as u64
        } else {
            get_apic_base().unwrap_or(0xFEE00000) + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64
        }
    };
    let mut apic = ApicRaw { base_addr };
    enable_apic(&mut apic);

    apic.write(
        ApicOffsets::LVT_TIMER,
        TIMER_INTERRUPT_INDEX | ApicFlags::TIMER_PERIODIC,
    );
    apic.write(ApicOffsets::TMRDIV, 0x3); // Divide by 16
    apic.write(ApicOffsets::TMRINITCNT, 1000000);

    *APIC.lock() = Some(apic);
    let io_apic_virt_base = IO_APIC_BASE + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;
    init_io_apic(base_addr, io_apic_virt_base);

    use super::syscall::setup_syscall;
    setup_syscall();

    x86_64::instructions::interrupts::enable();
}
