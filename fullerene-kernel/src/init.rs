//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use x86_64::instructions::port::Port;

use crate::{fs, graphics, interrupts, loader, process, syscall, vga};

// Macro to reduce repetitive serial logging - local copy since we moved function here
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;

macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

// VGA port constants
const VGA_MISC_OUTPUT: u16 = 0x3c2;
const VGA_MISC_ENABLE_COLOR_MODE: u8 = 0x67;  // bit0=1 for color mode, Clock Select=1

const VGA_SEQUENCER_INDEX: u16 = 0x3c4;
const VGA_SEQUENCER_DATA: u16 = 0x3c5;
const VGA_SEQ_RESET: u8 = 0x00;
const VGA_SEQ_CLOCKING_MODE: u8 = 0x01;
const VGA_SEQ_MAP_MASK: u8 = 0x02;
const VGA_SEQ_CHARACTER_MAP: u8 = 0x03;
const VGA_SEQ_MEMORY_MODE: u8 = 0x04;

const VGA_CRTC_INDEX: u16 = 0x3d4;
const VGA_CRTC_DATA: u16 = 0x3d5;
const VGA_CRTC_HORIZONTAL_TOTAL: u8 = 0x00;
const VGA_CRTC_HORIZONTAL_DISPLAY_END: u8 = 0x01;
const VGA_CRTC_HORIZONTAL_BLANK_START: u8 = 0x02;
const VGA_CRTC_HORIZONTAL_BLANK_END: u8 = 0x03;
const VGA_CRTC_HORIZONTAL_RETRACE_START: u8 = 0x04;
const VGA_CRTC_HORIZONTAL_RETRACE_END: u8 = 0x05;
const VGA_CRTC_VERTICAL_TOTAL: u8 = 0x06;
const VGA_CRTC_OVERFLOW: u8 = 0x07;
const VGA_CRTC_MAXIMUM_SCAN_LINE: u8 = 0x09;
const VGA_CRTC_VERTICAL_RETRACE_START: u8 = 0x10;
const VGA_CRTC_VERTICAL_RETRACE_END: u8 = 0x11;
const VGA_CRTC_VERTICAL_DISPLAY_END: u8 = 0x12;
const VGA_CRTC_OFFSET: u8 = 0x13;
const VGA_CRTC_VERTICAL_BLANK_START: u8 = 0x15;
const VGA_CRTC_VERTICAL_BLANK_END: u8 = 0x16;
const VGA_CRTC_MODE_CONTROL: u8 = 0x17;
const VGA_CRTC_LINE_COMPARE: u8 = 0x18;
const VGA_CRTC_UNLOCK: u8 = 0x11;
const VGA_CRTC_UNLOCK_MASK: u8 = 0x7f;

const VGA_ATTRIBUTE_INDEX: u16 = 0x3c0;
const VGA_ATTRIBUTE_MODE_CONTROL: u8 = 0x10;
const VGA_ATTRIBUTE_COLOR_PLANE_ENABLE: u8 = 0x12;
const VGA_STATUS_REGISTER: u16 = 0x3da;
const VGA_ATTRIBUTE_ENABLE_VIDEO: u8 = 0x20;

unsafe fn init_vga_text_mode() {
    // Miscellaneous Output Register: Enable color mode, 0x3D4 map active
    let mut misc = Port::new(VGA_MISC_OUTPUT);
    misc.write(VGA_MISC_ENABLE_COLOR_MODE);

    // Sequencer: Reset and configure for alphanumeric mode
    let mut seq_idx = Port::new(VGA_SEQUENCER_INDEX);
    let mut seq_data = Port::new(VGA_SEQUENCER_DATA);
    seq_idx.write(VGA_SEQ_RESET); seq_data.write(0x01u8);  // Reset
    seq_idx.write(VGA_SEQ_CLOCKING_MODE); seq_data.write(0x01u8);  // 9/8 Dot Mode=0
    seq_idx.write(VGA_SEQ_CHARACTER_MAP); seq_data.write(0x00u8);  // Character Map Select
    seq_idx.write(VGA_SEQ_MEMORY_MODE); seq_data.write(0x03u8);  // Memory Mode: Alpha mode, Odd/Even disabled

    // CRTC: Configure 80x25 timing (unlock first)
    let mut crtc_idx = Port::new(VGA_CRTC_INDEX);
    let mut crtc_data = Port::new(VGA_CRTC_DATA);
    let current_reg11 = crtc_data.read();
    crtc_idx.write(VGA_CRTC_UNLOCK); crtc_data.write(VGA_CRTC_UNLOCK_MASK & current_reg11);  // Unlock (clear bit7)
    // Horizontal timing
    crtc_idx.write(VGA_CRTC_HORIZONTAL_TOTAL); crtc_data.write(0x5fu8);  // Total
    crtc_idx.write(VGA_CRTC_HORIZONTAL_DISPLAY_END); crtc_data.write(0x4fu8);  // Display End
    crtc_idx.write(VGA_CRTC_HORIZONTAL_BLANK_START); crtc_data.write(0x50u8);  // Blank Start
    crtc_idx.write(VGA_CRTC_HORIZONTAL_BLANK_END); crtc_data.write(0x82u8);  // Blank End
    crtc_idx.write(VGA_CRTC_HORIZONTAL_RETRACE_START); crtc_data.write(0x55u8);  // Retrace Start
    crtc_idx.write(VGA_CRTC_HORIZONTAL_RETRACE_END); crtc_data.write(0x81u8);  // Retrace End
    // Vertical timing
    crtc_idx.write(VGA_CRTC_VERTICAL_TOTAL); crtc_data.write(0xbfu8);  // Total
    crtc_idx.write(VGA_CRTC_OVERFLOW); crtc_data.write(0x1fu8);  // Overflow
    crtc_idx.write(VGA_CRTC_MAXIMUM_SCAN_LINE); crtc_data.write(0x4fu8);  // Max Scan Line
    crtc_idx.write(VGA_CRTC_VERTICAL_RETRACE_START); crtc_data.write(0x9cu8);  // V Retrace Start
    crtc_idx.write(VGA_CRTC_VERTICAL_RETRACE_END); crtc_data.write(0x8eu8);  // V Retrace End
    crtc_idx.write(VGA_CRTC_VERTICAL_DISPLAY_END); crtc_data.write(0x8fu8);  // V Display End
    crtc_idx.write(VGA_CRTC_OFFSET); crtc_data.write(0x28u8);  // Offset
    crtc_idx.write(VGA_CRTC_VERTICAL_BLANK_START); crtc_data.write(0x96u8);  // V Blank Start
    crtc_idx.write(VGA_CRTC_VERTICAL_BLANK_END); crtc_data.write(0xb9u8);  // V Blank End
    crtc_idx.write(VGA_CRTC_MODE_CONTROL); crtc_data.write(0xa3u8);  // Mode Control

    // Attribute Controller: Simple reset and setup
    let mut attr = Port::new(VGA_ATTRIBUTE_INDEX);
    x86_64::instructions::port::Port::<u8>::new(VGA_STATUS_REGISTER).read();  // Flip-flop reset
    attr.write(VGA_ATTRIBUTE_MODE_CONTROL);  // Mode Control: Text mode
    attr.write(VGA_ATTRIBUTE_COLOR_PLANE_ENABLE);  // Color Plane Enable: All
}

#[cfg(target_os = "uefi")]
pub fn init_common() {
    unsafe {
        init_vga_text_mode();
    }
    crate::vga::init_vga();
    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    kernel_log!("Kernel: APIC initialized and interrupts enabled");

    // Initialize process management
    process::init();
    kernel_log!("Kernel: Process management initialized");

    // Initialize system calls
    syscall::init();
    kernel_log!("Kernel: System calls initialized");

    // Initialize filesystem
    fs::init();
    kernel_log!("Kernel: Filesystem initialized");

    // Initialize program loader
    loader::init();
    kernel_log!("Kernel: Program loader initialized");

    // Create a test user process
    let test_entry = x86_64::VirtAddr::new(crate::test_process::test_process_main as usize as u64);
    let test_pid = process::create_process("test_process", test_entry);
    kernel_log!("Kernel: Created test process with PID {}", test_pid);

    // Test interrupt handling - should not panic or crash if APIC is working
    kernel_log!("Testing interrupt handling with int3...");
    unsafe {
        x86_64::instructions::interrupts::int3();
    }
    kernel_log!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common() {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
        [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        crate::heap::ALLOCATOR
            .lock()
            .init(heap_start_ptr, crate::heap::HEAP_SIZE);
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga();
}
