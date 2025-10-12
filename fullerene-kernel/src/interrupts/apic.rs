//! APIC (Advanced Programmable Interrupt Controller) handling
//!
//! This module provides APIC initialization and management functions.

use super::pic::disable_legacy_pic;
use core::ptr;
use petroleum::init_io_apic;
use petroleum::port_write;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::registers::model_specific::Msr;

/// APIC register offsets
pub struct ApicOffsets;
impl ApicOffsets {
    const BASE_MSR: u32 = 0x1B;
    const BASE_ADDR_MASK: u64 = !0xFFF;
    const SPURIOUS_VECTOR: u32 = 0x0F0;
    const LVT_TIMER: u32 = 0x320;
    const LVT_LINT0: u32 = 0x350;
    const LVT_LINT1: u32 = 0x360;
    const LVT_ERROR: u32 = 0x370;
    const TMRDIV: u32 = 0x3E0;
    const TMRINITCNT: u32 = 0x380;
    const TMRCURRCNT: u32 = 0x390;
    const EOI: u32 = 0x0B0;
    const ID: u32 = 0x20;
    const VERSION: u32 = 0x30;
}

/// APIC control bits
struct ApicFlags;
impl ApicFlags {
    const SW_ENABLE: u32 = 1 << 8;
    const DISABLE: u32 = 0x10000;
    const TIMER_PERIODIC: u32 = 1 << 17;
    const TIMER_MASKED: u32 = 1 << 16;
}

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
    unsafe fn read(&self, offset: u32) -> u32 {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        addr.read_volatile()
    }

    /// Write to APIC register
    unsafe fn write(&self, offset: u32, value: u32) {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        unsafe { addr.write_volatile(value) }
    }
}

/// Global APIC instance
pub static APIC: Mutex<Option<ApicRaw>> = Mutex::new(None);

/// Get APIC base address
fn get_apic_base() -> Option<u64> {
    let msr = Msr::new(ApicOffsets::BASE_MSR);
    let value = unsafe { msr.read() };
    if value & (1 << 11) != 0 {
        Some(value & ApicOffsets::BASE_ADDR_MASK)
    } else {
        None
    }
}

/// Enable APIC
fn enable_apic(apic: &mut ApicRaw) {
    let spurious = unsafe { apic.read(ApicOffsets::SPURIOUS_VECTOR) };
    unsafe {
        apic.write(
            ApicOffsets::SPURIOUS_VECTOR,
            spurious | ApicFlags::SW_ENABLE | 0xFF,
        );
    }
}

/// Send End-Of-Interrupt to APIC
pub fn send_eoi() {
    if let Some(apic) = APIC.lock().as_ref() {
        unsafe {
            apic.write(ApicOffsets::EOI, 0);
        }
    }
}

/// Initialize APIC
pub fn init_apic() {
    petroleum::serial::serial_log(format_args!("Initializing APIC...\n"));

    disable_legacy_pic();
    petroleum::serial::serial_log(format_args!("Legacy PIC disabled.\n"));

    let base_addr = get_apic_base().unwrap_or(0xFEE00000);
    let mut apic = ApicRaw { base_addr };
    enable_apic(&mut apic);

    unsafe {
        apic.write(
            ApicOffsets::LVT_TIMER,
            TIMER_INTERRUPT_INDEX | ApicFlags::TIMER_PERIODIC,
        );
        apic.write(ApicOffsets::TMRDIV, 0x3); // Divide by 16
        apic.write(ApicOffsets::TMRINITCNT, 1000000);
    }

    *APIC.lock() = Some(apic);
    init_io_apic(base_addr);

    use super::syscall::setup_syscall;
    setup_syscall();

    x86_64::instructions::interrupts::enable();
}
