#![allow(static_mut_refs)]
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::{debug_log_no_alloc, mem_debug};
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

// Direct serial logging without allocations using petroleum macros

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;

/// Size of the GDT TSS stack (bytes).
/// Each stack is 5 pages to accommodate interrupt handling for double fault and timer interrupts.
pub const GDT_TSS_STACK_SIZE: usize = 4096 * 5;

/// Number of GDT TSS stacks reserved.
/// Three stacks are reserved: double fault stack, timer stack, and an additional stack for future use.
/// This provides redundancy and prevents stack overflow during nested interrupts.
pub const GDT_TSS_STACK_COUNT: usize = 3;

/// Total overhead for GDT initialization in bytes.
/// This includes space for all TSS stacks and should be accounted for before heap allocation.
pub const GDT_INIT_OVERHEAD: usize = GDT_TSS_STACK_COUNT * GDT_TSS_STACK_SIZE;

static mut TSS: Option<TaskStateSegment> = None;
static mut GDT: Option<GlobalDescriptorTable> = None;
static mut CODE_SELECTOR: Option<SegmentSelector> = None;
static mut KERNEL_DATA_SELECTOR: Option<SegmentSelector> = None;
static mut TSS_SELECTOR: Option<SegmentSelector> = None;
static mut USER_DATA_SELECTOR: Option<SegmentSelector> = None;
static mut USER_CODE_SELECTOR: Option<SegmentSelector> = None;
static GDT_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[repr(align(4096))]
struct EarlyGdtBuffer([u8; 0x20000]);
static EARLY_GDT_BUFFER: EarlyGdtBuffer = EarlyGdtBuffer([0; 0x20000]);

pub fn init_early() {
    let addr = VirtAddr::from_ptr(&EARLY_GDT_BUFFER.0 as *const _ as *const u8);
    init(addr);
}

pub fn kernel_code_selector() -> SegmentSelector {
    unsafe { CODE_SELECTOR.expect("CODE_SELECTOR not initialized") }
}

pub fn load() {
    let gdt = unsafe {
        core::mem::transmute::<&GlobalDescriptorTable, &'static GlobalDescriptorTable>(
            GDT.as_ref().expect("GDT not initialized"),
        )
    };
    gdt.load();

    unsafe {
        CS::set_reg(CODE_SELECTOR.expect("CODE_SELECTOR not initialized"));
        load_tss(TSS_SELECTOR.expect("TSS_SELECTOR not initialized"));

        // Reload all data segment registers to ensure they point to the correct GDT entry
        if let Some(data_sel) = KERNEL_DATA_SELECTOR {
            use x86_64::registers::segmentation::{DS, ES, FS, GS, SS};
            DS::set_reg(data_sel);
            SS::set_reg(data_sel);
            ES::set_reg(data_sel);
            FS::set_reg(data_sel);
            GS::set_reg(data_sel);
        }
    }
}

pub struct TssStacks {
    pub double_fault: VirtAddr,
    pub timer: VirtAddr,
}

pub fn init_with_stacks(stacks: TssStacks) {
    mem_debug!("GDT: Updating TSS stacks\n");

    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: init_with_stacks (unsafe mode)\n");

        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stacks.double_fault;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = stacks.timer;
        TSS = Some(tss);

        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: TSS created (unsafe)\n");

        mem_debug!("DEBUG: Creating GDT...\n");
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());

        let tss_selector = {
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"DEBUG: About to access TSS static for GDT descriptor\n",
            );
            let tss_ref = TSS.as_ref().expect("TSS not set");
            let selector = gdt.append(Descriptor::tss_segment(tss_ref));
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"DEBUG: Successfully accessed TSS static for GDT descriptor\n",
            );
            selector
        };

        CODE_SELECTOR = Some(code_selector);
        KERNEL_DATA_SELECTOR = Some(data_selector);
        TSS_SELECTOR = Some(tss_selector);
        USER_DATA_SELECTOR = Some(user_data_selector);
        USER_CODE_SELECTOR = Some(user_code_selector);
        GDT = Some(gdt);

        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT built successfully\n");
    }

    mem_debug!("GDT: GDT built\n");

    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    mem_debug!("GDT: About to return\n");
}

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    // If already initialized, just return the heap start (don't modify)
    if GDT_INITIALIZED.load(Ordering::SeqCst) {
        unsafe {
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"DEBUG: GDT: Already initialized, skipping\n",
            );
        }
        return heap_start;
    }

    unsafe {
        let mut buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(heap_start.as_u64(), &mut buf, 16);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT: Initializing with heap at 0x");
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
    }

    let double_fault_ist = heap_start + GDT_TSS_STACK_SIZE as u64;
    let timer_ist = double_fault_ist + GDT_TSS_STACK_SIZE as u64;
    // Reserve space for all TSS stacks (double fault, timer, and one spare).
    let new_heap_start = timer_ist + GDT_TSS_STACK_SIZE as u64;

    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT: Stack addresses calculated\n");
    }

    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT: About to access TSS static\n");
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_ist;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = timer_ist;
        TSS = Some(tss);
        petroleum::write_serial_bytes(
            0x3F8,
            0x3FD,
            b"DEBUG: GDT: TSS static accessed successfully\n",
        );
    }

    mem_debug!("GDT: TSS created\n");

    unsafe {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());

        let tss_selector = {
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"DEBUG: GDT: About to access TSS static for GDT descriptor\n",
            );
            let tss_ref = TSS.as_ref().expect("TSS must be initialized");
            let selector = gdt.append(Descriptor::tss_segment(tss_ref));
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"DEBUG: GDT: TSS static accessed successfully for GDT descriptor\n",
            );
            selector
        };

        CODE_SELECTOR = Some(code_selector);
        KERNEL_DATA_SELECTOR = Some(data_selector);
        TSS_SELECTOR = Some(tss_selector);
        USER_DATA_SELECTOR = Some(user_data_selector);
        USER_CODE_SELECTOR = Some(user_code_selector);
        GDT = Some(gdt);
    }

    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT: GDT built\n");
    }

    #[cfg(not(target_os = "uefi"))]
    {
        // Load GDT - required for proper segmentation in BIOS mode
        mem_debug!("About to load GDT...\n");
        let gdt = unsafe {
            core::mem::transmute::<&GlobalDescriptorTable, &'static GlobalDescriptorTable>(
                GDT.as_ref().expect("GDT not initialized"),
            )
        };
        gdt.load();
        mem_debug!("GDT loaded\n");

        unsafe {
            // Reload CS register in BIOS mode as it's crucial for GDT reload
            mem_debug!("About to set CS register...\n");
            CS::set_reg(CODE_SELECTOR.expect("CODE_SELECTOR not initialized"));
            mem_debug!("GDT: CS set\n");

            mem_debug!("About to load TSS...\n");
            load_tss(TSS_SELECTOR.expect("TSS_SELECTOR not initialized"));
            mem_debug!("GDT: TSS loaded\n");
            mem_debug!("GDT: Loaded and segments set\n");

            // Set data segment registers to kernel data segment for proper I/O operations
            mem_debug!("Setting data segment registers...\n");
            if let Some(data_sel) = KERNEL_DATA_SELECTOR {
                use x86_64::registers::segmentation::{DS, ES, FS, GS, SS};
                DS::set_reg(data_sel);
                SS::set_reg(data_sel);
                ES::set_reg(data_sel);
                FS::set_reg(data_sel);
                GS::set_reg(data_sel);
            }
            mem_debug!("Data segment registers set\n");
        }
    }
    #[cfg(target_os = "uefi")]
    {
        // Skip GDT reload and TSS loading in UEFI mode to avoid stack pointer corruption
        mem_debug!("Skipping GDT reload and TSS loading in UEFI mode\n");
    }

    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: GDT: About to return\n");
    }
    new_heap_start
}

pub fn user_code_selector() -> SegmentSelector {
    unsafe { USER_CODE_SELECTOR.expect("USER_CODE_SELECTOR not initialized") }
}

pub fn user_data_selector() -> SegmentSelector {
    unsafe { USER_DATA_SELECTOR.expect("USER_DATA_SELECTOR not initialized") }
}
