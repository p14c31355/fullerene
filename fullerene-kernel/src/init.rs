use x86_64::instructions::port::Port;

/// Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::{fs, graphics, interrupts, loader, process, syscall, vga};

// Macro to reduce repetitive serial logging - local copy since we moved function here
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;

macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

unsafe fn init_vga_text_mode() {
    // Miscellaneous Output Register (0x3C2): 色モード, 0x3D4マップ有効
    let mut misc = Port::new(0x3c2 as u16);
    misc.write(0x67u8);  // bit0=1 for mono/color, Clock Select=1

    // Sequencer (0x3C4/0x3C5): アルファベットモード
    let mut seq_idx = Port::new(0x3c4 as u16);
    let mut seq_data = Port::new(0x3c5 as u16);
    seq_idx.write(0x00u8); seq_data.write(0x01u8);  // Reset 0
    seq_idx.write(0x01u8); seq_data.write(0x01u8);  // Reset 1 (9/8 Dot Mode=0)
    seq_idx.write(0x03u8); seq_data.write(0x00u8);  // Char Map Select
    seq_idx.write(0x04u8); seq_data.write(0x07u8);  // Memory Mode: Odd/Even=1, Chain4=0

    // CRTC (0x3D4/0x3D5): 80x25タイミング (unlock first)
    let mut crtc_idx = Port::new(0x3d4 as u16);
    let mut crtc_data = Port::new(0x3d5 as u16);
    let current_reg11 = crtc_data.read();
    crtc_idx.write(0x11u8); crtc_data.write(0x7fu8 & current_reg11);  // Unlock (clear bit7)
    // Horizontal
    crtc_idx.write(0x00u8); crtc_data.write(0x5fu8);  // Total
    crtc_idx.write(0x01u8); crtc_data.write(0x4fu8);  // Display End
    crtc_idx.write(0x02u8); crtc_data.write(0x50u8);  // Blank Start
    crtc_idx.write(0x03u8); crtc_data.write(0x82u8);  // Blank End
    crtc_idx.write(0x04u8); crtc_data.write(0x55u8);  // Retrace Start
    crtc_idx.write(0x05u8); crtc_data.write(0x81u8);  // Retrace End
    // Vertical
    crtc_idx.write(0x06u8); crtc_data.write(0xbFu8);  // Total
    crtc_idx.write(0x07u8); crtc_data.write(0x1Fu8);  // Overflow
    crtc_idx.write(0x09u8); crtc_data.write(0x4Fu8);  // Max Scan Line
    crtc_idx.write(0x10u8); crtc_data.write(0x9Cu8);  // V Retrace Start
    crtc_idx.write(0x11u8); crtc_data.write(0x8Eu8);  // V Retrace End
    crtc_idx.write(0x12u8); crtc_data.write(0x8Fu8);  // V Display End
    crtc_idx.write(0x13u8); crtc_data.write(0x28u8);  // Offset
    crtc_idx.write(0x15u8); crtc_data.write(0x96u8);  // V Blank Start
    crtc_idx.write(0x16u8); crtc_data.write(0xb9u8);  // V Blank End
    crtc_idx.write(0x17u8); crtc_data.write(0xa3u8);  // Mode Control

    // Attribute Controller (0x3C0): 簡易リセット
    let mut attr = Port::new(0x3c0 as u16);
    x86_64::instructions::port::Port::<u8>::new(0x3da as u16).read();  // Flip-flop reset
    attr.write(0x10u8);  // Mode Control: Text mode
    attr.write(0x12u8);  // Color Plane Enable: All
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
