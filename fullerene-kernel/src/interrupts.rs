// fullerene-kernel/src/interrupts.rs

use crate::gdt;
use core::fmt::Write;
use lazy_static::lazy_static;
use petroleum::init_io_apic;
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

// Include our new modules
use crate::process;

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

// APIC register definitions (grouped to reduce constants)
struct ApicOffsets;
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

// APIC control bits (grouped)
struct ApicFlags;
impl ApicFlags {
    const SW_ENABLE: u32 = 1 << 8;
    const DISABLE: u32 = 0x10000;
    const TIMER_PERIODIC: u32 = 1 << 17;
    const TIMER_MASKED: u32 = 1 << 16;
}

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
    let msr = Msr::new(ApicOffsets::BASE_MSR);
    let value = unsafe { msr.read() };
    if value & (1 << 11) != 0 {
        // APIC is enabled
        Some(value & ApicOffsets::BASE_ADDR_MASK)
    } else {
        None
    }
}

fn enable_apic(apic: &mut Apic) {
    unsafe {
        // Enable APIC by setting bit 8 in spurious vector register
        let spurious = apic.read(ApicOffsets::SPURIOUS_VECTOR);
        apic.write(
            ApicOffsets::SPURIOUS_VECTOR,
            spurious | ApicFlags::SW_ENABLE | 0xFF,
        );
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
        // Remove int 0x80 syscall handler - we now use the syscall instruction with LSTAR MSR

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
        apic.write(
            ApicOffsets::LVT_TIMER,
            TIMER_INTERRUPT_INDEX | ApicFlags::TIMER_PERIODIC,
        );
        apic.write(ApicOffsets::TMRDIV, 0x3); // Divide by 16
        apic.write(ApicOffsets::TMRINITCNT, 1000000); // Initial count for ~100ms at 10MHz
    }

    // Store APIC instance
    *APIC.lock() = Some(apic);

    // Initialize I/O APIC for legacy interrupts (keyboard, mouse, etc.)
    init_io_apic(base_addr);

    // Set up fast system call mechanism
    setup_syscall();

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
    use x86_64::registers::control::Cr2;

    // Get the faulting address
    let fault_addr = Cr2::read();

    let fault_addr = match fault_addr {
        Ok(addr) => addr,
        Err(_) => {
            petroleum::serial::serial_log(format_args!("\nEXCEPTION: PAGE FAULT but CR2 is invalid.\n"));
            return;
        }
    };

    petroleum::serial::serial_log(format_args!(
        "\nEXCEPTION: PAGE FAULT at address {:#x}\nError Code: {:?}\n",
        fault_addr.as_u64(),
        error_code
    ));

    // Page fault handling logic
    handle_page_fault(fault_addr, error_code, stack_frame);

    // After handling, execution can continue
}

fn handle_page_fault(
    fault_addr: x86_64::VirtAddr,
    error_code: PageFaultErrorCode,
    stack_frame: InterruptStackFrame,
) {
    use crate::memory_management;
    use x86_64::registers::control::Cr2;

    // Basic analysis of fault
    let is_present = error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION);
    let is_write = error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_user = error_code.contains(PageFaultErrorCode::USER_MODE);

    let mut writer = SERIAL1.lock();
    write!(writer, "Page fault analysis: ").ok();
    if is_present {
        write!(writer, "Protection violation ").ok();
    } else {
        write!(writer, "Page not present ").ok();
    }
    if is_write {
        write!(writer, "(write access) ").ok();
    }
    if is_user {
        write!(writer, "(user mode)").ok();
    }
    writeln!(writer).ok();

    // For now, we handle only user-space page faults
    // Kernel page faults indicate serious errors

    if !is_user {
        // Kernel page fault - this is critical
        panic!(
            "Kernel page fault at {:#x}: {:?}",
            fault_addr.as_u64(),
            error_code
        );
    }

    if is_present {
        // Protection violation in user space
        // This might be write to read-only page, etc.
        // For now, terminate the current process
        write!(
            writer,
            "Protection violation in user space - terminating process\n"
        )
        .ok();

        if let Some(pid) = crate::process::current_pid() {
            crate::process::terminate_process(pid, 1); // Exit code 1 for page fault
        }
    } else {
        // Page not present - need to handle demand paging or stack growth
        write!(writer, "Page not present - attempting to handle\n").ok();

        // For now, try to allocate a new page if it's in valid user space
        if memory_management::is_user_address(fault_addr) {
            // This is a simplified page fault handler
            // In a real system, we'd check if this is a valid allocation request
            // and allocate pages accordingly

            // For stack growth or heap allocation, we might allocate here
            // But current process doesn't have ProcessPageTable integration yet

            write!(writer, "Cannot handle page fault - terminating process\n").ok();
            if let Some(pid) = crate::process::current_pid() {
                crate::process::terminate_process(pid, 1);
            }
        } else {
            // Invalid user address
            write!(writer, "Invalid user address - terminating process\n").ok();
            if let Some(pid) = crate::process::current_pid() {
                crate::process::terminate_process(pid, 1);
            }
        }
    }
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("\nEXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// Macro to generate input device handlers (keyboard/mouse)
macro_rules! define_input_interrupt_handler {
    ($handler_name:ident, $port:expr, $process_input:expr) => {
        pub extern "x86-interrupt" fn $handler_name(_stack_frame: InterruptStackFrame) {
            // Common input handling pattern
            let mut port = Port::<u8>::new($port);
            let data = unsafe { port.read() };
            $process_input(data);
            send_eoi();
        }
    };
}

// Hardware interrupt handlers
pub extern "x86-interrupt" fn timer_handler(stack_frame: InterruptStackFrame) {
    // Timer interrupt - handle timer ticks and scheduling
    *TICK_COUNTER.lock() += 1;

    // Perform preemptive scheduling
    unsafe {
        // Get current process before scheduling
        let old_pid = process::current_pid();

        // Schedule next process
        process::schedule_next();

        // Get new process after scheduling
        let new_pid = process::current_pid();

        if let (Some(old_pid), Some(new_pid)) = (old_pid, new_pid) {
            if old_pid != new_pid {
                // Perform context switch
                process::context_switch(Some(old_pid), new_pid);
            }
        }
    }

    send_eoi();
}

define_input_interrupt_handler!(keyboard_handler, 0x60, |scancode: u8| {
    // Use new keyboard driver
    crate::keyboard::handle_keyboard_scancode(scancode);
});

define_input_interrupt_handler!(mouse_handler, 0x60, |byte: u8| {
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
});

// Send End-Of-Interrupt to APIC
fn send_eoi() {
    if let Some(apic) = APIC.lock().as_ref() {
        unsafe {
            apic.write(ApicOffsets::EOI, 0);
        }
    }
}

// System call entry point (called via syscall instruction, not interrupt)
#[unsafe(no_mangle)]
pub extern "C" fn syscall_entry() {
    use crate::syscall;

    // System call arguments (x86-64 System V ABI for syscall):
    // RAX contains syscall number
    // RDI, RSI, RDX, R10, R8, R9 contain arguments 1-6

    let syscall_num: u64;
    let arg1: u64;
    let arg2: u64;
    let arg3: u64;
    let arg4: u64;
    let arg5: u64;
    let arg6: u64;

    unsafe {
        core::arch::asm!(
            "mov {}, rax",
            "mov {}, rdi",
            "mov {}, rsi",
            "mov {}, rdx",
            "mov {}, r10",
            "mov {}, r8",
            "mov {}, r9",
            out(reg) syscall_num,
            out(reg) arg1,
            out(reg) arg2,
            out(reg) arg3,
            out(reg) arg4,
            out(reg) arg5,
            out(reg) arg6,
        );
    }

    // Dispatch to syscall handler
    let result = unsafe { syscall::handle_syscall(syscall_num, arg1, arg2, arg3, arg4, arg5) };

    // Return result in RAX and return via sysret
    unsafe {
        core::arch::asm!(
            "mov rax, {}",
            "sysretq", // Return from syscall
            in(reg) result,
            options(noreturn)
        );
    }
}

// Set up the syscall instruction to use the Fast System Call mechanism
pub fn setup_syscall() {
    use x86_64::registers::model_specific::{Efer, EferFlags, Msr};
    use x86_64::registers::model_specific::{LStar, SFMask, Star};
    use x86_64::registers::rflags::RFlags;

    // Enable syscall/sysret instructions by setting SCE bit in EFER
    unsafe {
        let mut efer = Msr::new(0xC0000080); // EFER MSR
        let current = efer.read();
        efer.write(current | (1 << 0)); // Set SCE (System Call Extension) bit
    }

    // Set the syscall entry point
    let entry_addr = syscall_entry as u64;
    unsafe {
        let mut lstar = Msr::new(0xC0000082); // LSTAR MSR
        lstar.write(entry_addr);
    }

    // For now, we don't have user segments set up properly in GDT,
    // so we'll use kernel segments for both kernel and user.
    // In a full implementation, we'd add userland descriptors to GDT.
    let star_value = (0x08u64 << 48) | (0x10u64 << 32) | (0x08u64 << 16) | 0x10u64;
    unsafe {
        let mut star = Msr::new(0xC0000081); // STAR MSR
        star.write(star_value);
    }

    // Mask RFLAGS during syscall (clear interrupt flag, etc.)
    // This masks bits that could be problematic during syscall
    unsafe {
        let mut sfmask = Msr::new(0xC0000084); // SFMASK MSR
        sfmask.write(RFlags::INTERRUPT_FLAG.bits() | RFlags::TRAP_FLAG.bits()); // Clear IF and TF
    }

    petroleum::serial::serial_log(format_args!(
        "Fast syscall mechanism initialized with LSTAR set to {:#x}\n",
        entry_addr
    ));
}
