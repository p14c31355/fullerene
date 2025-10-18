use log;
use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;

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
        log::info!("GDT: Already initialized, skipping");
        return heap_start;
    }

    log::info!("GDT: Initializing with heap at {:#x}", heap_start.as_u64());

    const STACK_SIZE: usize = 4096 * 5;
    let double_fault_ist = heap_start + STACK_SIZE as u64;
    let timer_ist = double_fault_ist + STACK_SIZE as u64;
    let new_heap_start = timer_ist + STACK_SIZE as u64; // Reserve space for both stacks

    log::info!("GDT: Stack addresses calculated");

    log::info!("About to create TSS...");
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_ist;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = timer_ist;
        tss
    });
    log::info!("TSS created successfully");

    log::info!("GDT: TSS created");

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

    log::info!("GDT: GDT built");

    #[cfg(not(target_os = "uefi"))]
    {
        // Load GDT - required for proper segmentation in BIOS mode
        log::info!("About to load GDT...");
        gdt.load();
        log::info!("GDT: GDT loaded");

        unsafe {
            // Reload CS register in BIOS mode as it's crucial after GDT reload
            log::info!("About to set CS register...");
            CS::set_reg(*CODE_SELECTOR.get().unwrap());
            log::info!("GDT: CS set");

            log::info!("About to load TSS...");
            load_tss(*TSS_SELECTOR.get().unwrap());
            log::info!("GDT: TSS loaded");
            log::info!("GDT: Loaded and segments set");

            // Set data segment registers to kernel data segment for proper I/O operations
            log::info!("Setting data segment registers...");
            if let Some(data_sel) = KERNEL_DATA_SELECTOR.get() {
                use x86_64::registers::segmentation::{DS, ES, FS, GS, SS};
                DS::set_reg(*data_sel);
                SS::set_reg(*data_sel);
                ES::set_reg(*data_sel);
                FS::set_reg(*data_sel);
                GS::set_reg(*data_sel);
            }
            log::info!("Data segment registers set");
        }
    }
    #[cfg(target_os = "uefi")]
    {
        // Skip GDT reload and TSS loading in UEFI mode to avoid stack pointer corruption
        log::info!("Skipping GDT reload and TSS loading in UEFI mode");
    }

    // Mark as initialized
    GDT_INITIALIZED.call_once(|| {});
    log::info!("GDT: About to return");
    new_heap_start
}

pub fn user_code_selector() -> SegmentSelector {
    *USER_CODE_SELECTOR.get().expect("GDT not initialized")
}

pub fn user_data_selector() -> SegmentSelector {
    *USER_DATA_SELECTOR.get().expect("GDT not initialized")
}
