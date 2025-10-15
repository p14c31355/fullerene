//! Legacy PIC (Programmable Interrupt Controller) handling
//!
//! This module provides functions to disable the legacy PIC,
//! which is necessary when using APIC.

use crate::gdt;
use crate::interrupts::send_eoi;
use crate::process::context_switch;
use crate::process::current_pid;
use crate::process::schedule_next;
use petroleum::port_read_u8;
use petroleum::port_write;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::InterruptStackFrame;

// PIC ports
pub struct PicPorts;
impl PicPorts {
    pub const MASTER_COMMAND: u16 = 0x20;
    pub const MASTER_DATA: u16 = 0x21;
    pub const SLAVE_COMMAND: u16 = 0xA0;
    pub const SLAVE_DATA: u16 = 0xA1;
}

// PIC ICW commands
pub const ICW1_INIT: u8 = 0x10;
pub const ICW4_8086: u8 = 0x01;

/// Macro to reduce repetitive port writes for PIC initialization
macro_rules! init_pic {
    ($pic:expr, $vector_offset:expr, $slave_on:expr) => {{
        unsafe {
            Port::<u8>::new($pic.command).write(ICW1_INIT);
            Port::<u8>::new($pic.data).write($vector_offset); // ICW2: vector offset
            Port::<u8>::new($pic.data).write($slave_on); // ICW3: slave configuration
            Port::<u8>::new($pic.data).write(ICW4_8086);
        }
    }};
}

/// PIC configuration structs for cleaner code
struct Pic {
    command: u16,
    data: u16,
}

const PIC_MASTER: Pic = Pic {
    command: PicPorts::MASTER_COMMAND,
    data: PicPorts::MASTER_DATA,
};

const PIC_SLAVE: Pic = Pic {
    command: PicPorts::SLAVE_COMMAND,
    data: PicPorts::SLAVE_DATA,
};

/// Disable legacy PIC by remapping IRQs and masking all interrupts
pub fn disable_legacy_pic() {
    // Remap PIC vectors to avoid conflicts
    init_pic!(PIC_MASTER, 0x20, 4); // PIC1: vectors 32-39, slave on IR2
    init_pic!(PIC_SLAVE, 0x28, 2); // PIC2: vectors 40-47, slave identity 2

    // Mask all interrupts on both PICs
    port_write!(PIC_MASTER.data, 0xFFu8);
    port_write!(PIC_SLAVE.data, 0xFFu8);
}

// Timer interrupt handling is now in input.rs
