use petroleum::mem_debug;
use spin::Once;
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

static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<GlobalDescriptorTable> = Once::new();
static CODE_SELECTOR: Once<SegmentSelector> = Once::new();
static KERNEL_DATA_SELECTOR: Once<SegmentSelector> = Once::new();
static TSS_SELECTOR: Once<SegmentSelector> = Once::new();
static USER_DATA_SELECTOR: Once<SegmentSelector> = Once::new();
static USER_CODE_SELECTOR: Once<SegmentSelector> = Once::new();
static GDT_INITIALIZED: Once<()> = Once::new();

pub fn kernel_code_selector() -> SegmentSelector {
    *CODE_SELECTOR.get().expect("GDT not initialized")
}

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    // If already initialized, just return the heap start (don't modify)
    if GDT_INITIALIZED.is_completed() {
        mem_debug!("GDT: Already initialized, skipping\n");
        return heap_start;
    }

    mem_debug!("GDT: Initializing with heap at ", heap_start.as_u64(), "\n");

    let double_fault_ist = heap_start + GDT_TSS_STACK_SIZE as u64;
    let timer_ist = double_fault_ist + GDT_TSS_STACK_SIZE as u64;
    // Reserve space for all TSS stacks (double fault, timer, and one spare).
    let new_heap_start = timer_ist + GDT_TSS_STACK_SIZE as u64;

    mem_debug!("GDT: Stack addresses calculated\n");

    mem_debug!("About to create TSS...\n");
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_ist;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = timer_ist;
        tss
    });
    mem_debug!("TSS created successfully\n");

    mem_debug!("GDT: TSS created\n");

    let gdt = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        // Add kernel data segment (ring 0)
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        // Add user data segment (ring 3)
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        // Add user code segment (ring 3)
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(tss));

        CODE_SELECTOR.call_once(|| code_selector);
        KERNEL_DATA_SELECTOR.call_once(|| data_selector);
        TSS_SELECTOR.call_once(|| tss_selector);

        USER_DATA_SELECTOR.call_once(|| user_data_selector);
        USER_CODE_SELECTOR.call_once(|| user_code_selector);
        gdt
    });

    mem_debug!("GDT: GDT built\n");

    #[cfg(not(target_os = "uefi"))]
    {
        // Load GDT - required for proper segmentation in BIOS mode
        mem_debug!("About to load GDT...\n");
        gdt.load();
        mem_debug!("GDT: GDT loaded\n");

        unsafe {
            // Reload CS register in BIOS mode as it's crucial after GDT reload
            mem_debug!("About to set CS register...\n");
            CS::set_reg(*CODE_SELECTOR.get().unwrap());
            mem_debug!("GDT: CS set\n");

            mem_debug!("About to load TSS...\n");
            load_tss(*TSS_SELECTOR.get().unwrap());
            mem_debug!("GDT: TSS loaded\n");
            mem_debug!("GDT: Loaded and segments set\n");

            // Set data segment registers to kernel data segment for proper I/O operations
            mem_debug!("Setting data segment registers...\n");
            if let Some(data_sel) = KERNEL_DATA_SELECTOR.get() {
                use x86_64::registers::segmentation::{DS, ES, FS, GS, SS};
                DS::set_reg(*data_sel);
                SS::set_reg(*data_sel);
                ES::set_reg(*data_sel);
                FS::set_reg(*data_sel);
                GS::set_reg(*data_sel);
            }
            mem_debug!("Data segment registers set\n");
        }
    }
    #[cfg(target_os = "uefi")]
    {
        // Skip GDT reload and TSS loading in UEFI mode to avoid stack pointer corruption
        mem_debug!("Skipping GDT reload and TSS loading in UEFI mode\n");
    }

    // Mark as initialized
    GDT_INITIALIZED.call_once(|| {});
    mem_debug!("GDT: About to return\n");
    new_heap_start
}

pub fn user_code_selector() -> SegmentSelector {
    *USER_CODE_SELECTOR.get().expect("GDT not initialized")
}

pub fn user_data_selector() -> SegmentSelector {
    *USER_DATA_SELECTOR.get().expect("GDT not initialized")
}
