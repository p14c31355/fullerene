// fullerene-kernel/src/interrupts.rs

use crate::gdt;
use core::fmt::Write;
use lazy_static::lazy_static;
use petroleum::init_io_apic;
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

static TICK_COUNTER: Mutex<u64> = Mutex::new(0);

// Input handling structures
#[derive(Clone, Copy)]
struct KeyboardQueue {
    buffer: [u8; 256],
    head: usize,
    tail: usize,
}

#[derive(Clone, Copy)]
struct MouseState {
    x: i16,
    y: i16,
    buttons: u8,
    packet: [u8; 3],
    packet_idx: usize,
}

static KEYBOARD_QUEUE: Mutex<KeyboardQueue> = Mutex::new(KeyboardQueue {
    buffer: [0; 256],
    head: 0,
    tail: 0,
});

static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState {
    x: 0,
    y: 0,
    buttons: 0,
    packet: [0; 3],
    packet_idx: 0,
});

// APIC register definitions
const APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_ADDR_MASK: u64 = !0xFFF;
const APIC_SPURIOUS_VECTOR: u32 = 0x0F0;
const APIC_LVT_TIMER: u32 = 0x320;
const APIC_LVT_LINT0: u32 = 0x350;
const APIC_LVT_LINT1: u32 = 0x360;
const APIC_LVT_ERROR: u32 = 0x370;
const APIC_TMRDIV: u32 = 0x3E0;
const APIC_TMRINITCNT: u32 = 0x380;
const APIC_TMRCURRCNT: u32 = 0x390;
const APIC_EOI: u32 = 0x0B0;
const APIC_ID: u32 = 0x20;
const APIC_VERSION: u32 = 0x30;

// APIC control bits
const APIC_ENABLE: u32 = 1 << 8;
const APIC_SW_ENABLE: u32 = 1 << 8;
const APIC_DISABLE: u32 = 0x10000;
const APIC_TIMER_PERIODIC: u32 = 1 << 17;
const APIC_TIMER_MASKED: u32 = 1 << 16;

// Hardware interrupt vectors
pub const TIMER_INTERRUPT_INDEX: u32 = 32;
pub const KEYBOARD_INTERRUPT_INDEX: u32 = 33;
pub const MOUSE_INTERRUPT_INDEX: u32 = 44;

// PIC configuration structs and macros to reduce repetitive port writes
struct PicPorts {
    command: u16,
    data: u16,
}

const PIC1: PicPorts = PicPorts {
    command: 0x20,
    data: 0x21,
};

const PIC2: PicPorts = PicPorts {
    command: 0xA0,
    data: 0xA1,
};

const ICW1_INIT: u8 = 0x10;
const ICW4_8086: u8 = 0x01;

macro_rules! init_pic {
    ($pic:expr, $vector_offset:expr, $slave_on:expr) => {{
        unsafe {
            let mut cmd_port = Port::<u8>::new($pic.command);
            let mut data_port = Port::<u8>::new($pic.data);

            cmd_port.write(ICW1_INIT | ICW4_8086);
            data_port.write($vector_offset); // ICW2: vector offset
            data_port.write($slave_on); // ICW3: slave configuration
            data_port.write(ICW4_8086);
        }
    }};
}

// APIC structure for register access
struct Apic {
    base_addr: u64,
}

impl Apic {
    fn new(base_addr: u64) -> Self {
        Self { base_addr }
    }

    unsafe fn read(&self, offset: u32) -> u32 {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        unsafe { addr.read_volatile() }
    }

    unsafe fn write(&self, offset: u32, value: u32) {
        let addr = (self.base_addr + offset as u64) as *mut u32;
        unsafe { addr.write_volatile(value) }
    }
}

static APIC: Mutex<Option<Apic>> = Mutex::new(None);

// Helper functions for APIC setup
fn disable_legacy_pic() {
    // Remap and initialize PICs
    init_pic!(PIC1, 0x20, 4); // PIC1: vectors 32-39, slave on IR2
    init_pic!(PIC2, 0x28, 2); // PIC2: vectors 40-47, slave identity 2

    // Mask all interrupts
    unsafe {
        let mut pic1_data = Port::<u8>::new(PIC1.data);
        let mut pic2_data = Port::<u8>::new(PIC2.data);
        pic1_data.write(0xFF);
        pic2_data.write(0xFF);
    }
}

fn get_apic_base() -> Option<u64> {
    use x86_64::registers::model_specific::Msr;
    let msr = Msr::new(APIC_BASE_MSR);
    let value = unsafe { msr.read() };
    if value & (1 << 11) != 0 {
        // APIC is enabled
        Some(value & APIC_BASE_ADDR_MASK)
    } else {
        None
    }
}

fn enable_apic(apic: &mut Apic) {
    unsafe {
        // Enable APIC by setting bit 8 in spurious vector register
        let spurious = apic.read(APIC_SPURIOUS_VECTOR);
        apic.write(APIC_SPURIOUS_VECTOR, spurious | APIC_SW_ENABLE | 0xFF);
    }
}

// Macro to reduce repetitive IDT handler setup
macro_rules! setup_idt_handler {
    ($idt:expr, $field:ident, $handler:ident) => {
        $idt.$field.set_handler_fn($handler);
    };
}

lazy_static! {
    // The Interrupt Descriptor Table (IDT)
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up handlers for CPU exceptions
        setup_idt_handler!(idt, breakpoint, breakpoint_handler);
        setup_idt_handler!(idt, page_fault, page_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Set up hardware interrupt handlers
        unsafe {
            idt[TIMER_INTERRUPT_INDEX as u8]
                .set_handler_fn(timer_handler)
                .set_stack_index(gdt::TIMER_IST_INDEX);
        }
        idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
        idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);

        idt
    };
}

// Initialize IDT and optionally APIC
pub fn init() {
    IDT.load();
    petroleum::serial::serial_log(format_args!("IDT loaded with exception handlers.\n"));
}

pub fn init_apic() {
    petroleum::serial::serial_log(format_args!("Initializing APIC...\n"));

    // Disable legacy PIC
    disable_legacy_pic();
    petroleum::serial::serial_log(format_args!("Legacy PIC disabled.\n"));

    // Get APIC base address
    let base_addr = get_apic_base().unwrap_or(0xFEE00000); // Default local APIC address

    // Initialize APIC
    let mut apic = Apic::new(base_addr);
    enable_apic(&mut apic);

    // Configure timer interrupt
    unsafe {
        apic.write(APIC_LVT_TIMER, TIMER_INTERRUPT_INDEX | APIC_TIMER_PERIODIC);
        apic.write(APIC_TMRDIV, 0x3); // Divide by 16
        apic.write(APIC_TMRINITCNT, 1000000); // Initial count for ~100ms at 10MHz
    }

    // Store APIC instance
    *APIC.lock() = Some(apic);

    // Initialize I/O APIC for legacy interrupts (keyboard, mouse, etc.)
    init_io_apic(base_addr);

    // Enable interrupts
    x86_64::instructions::interrupts::enable();
}

pub fn disable_interrupts() {
    x86_64::instructions::interrupts::disable();
}

pub fn enable_interrupts() {
    x86_64::instructions::interrupts::enable();
}

// Exception handlers
pub extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    let mut writer = SERIAL1.lock();
    writeln!(writer, "\nEXCEPTION: BREAKPOINT\n{:#?}", stack_frame).ok();
}

pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let mut writer = SERIAL1.lock();
    writeln!(
        writer,
        "\nEXCEPTION: PAGE FAULT\n{:#?}\nError Code: {:?}",
        stack_frame, error_code
    )
    .ok();
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("\nEXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// Hardware interrupt handlers
pub extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Timer interrupt - handle timer ticks
    *TICK_COUNTER.lock() += 1;
    // Basic scheduling: could schedule tasks here in future, but for now just tick
    send_eoi();
}

pub extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    // Keyboard interrupt - handle keyboard input
    let mut port = Port::<u8>::new(0x60);
    let scancode = unsafe { port.read() };
    // Process scancode and add to input buffer
    let mut keyboard_queue = KEYBOARD_QUEUE.lock();
    let head = keyboard_queue.head;
    let tail = keyboard_queue.tail;
    let next_tail = (tail + 1) % 256;
    if next_tail != head {
        // Not full
        keyboard_queue.buffer[tail] = scancode;
        keyboard_queue.tail = next_tail;
    }
    // If full, drop the input for simplicity
    send_eoi();
}

pub extern "x86-interrupt" fn mouse_handler(_stack_frame: InterruptStackFrame) {
    // Mouse interrupt - handle mouse input
    let mut port = Port::<u8>::new(0x60);
    let byte = unsafe { port.read() };
    let mut mouse_state = MOUSE_STATE.lock();
    let current_idx = mouse_state.packet_idx;
    mouse_state.packet[current_idx] = byte;
    mouse_state.packet_idx += 1;
    if mouse_state.packet_idx == 3 {
        // Full packet received, process
        let status = mouse_state.packet[0];
        let dx = mouse_state.packet[1] as i8 as i16;
        let dy = mouse_state.packet[2] as i8 as i16;
        mouse_state.x = mouse_state.x.wrapping_add(dx);
        mouse_state.y = mouse_state.y.wrapping_add(dy);
        mouse_state.buttons = status & 0x07; // Left, right, middle bits
        mouse_state.packet_idx = 0; // Reset for next packet
        mouse_state.packet = [0; 3];
    }
    send_eoi();
}

// Send End-Of-Interrupt to APIC
fn send_eoi() {
    if let Some(apic) = APIC.lock().as_ref() {
        unsafe {
            apic.write(APIC_EOI, 0);
        }
    }
}
