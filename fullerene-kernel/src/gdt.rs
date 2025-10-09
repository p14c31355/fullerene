use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use petroleum::serial::serial_log;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;

static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<GlobalDescriptorTable> = Once::new();
static CODE_SELECTOR: Once<SegmentSelector> = Once::new();
static TSS_SELECTOR: Once<SegmentSelector> = Once::new();
static GDT_INITIALIZED: Once<()> = Once::new();

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    // If already initialized, just return the heap start (don't modify)
    if GDT_INITIALIZED.is_completed() {
        serial_log("GDT: Already initialized, skipping\n");
        return heap_start;
    }

    serial_log("GDT: Initializing with heap at ");
    serial_log(&alloc::format!("{:#x}\n", heap_start.as_u64()));

    const STACK_SIZE: usize = 4096 * 5;
    let double_fault_ist = heap_start + STACK_SIZE as u64;
    let timer_ist = double_fault_ist + STACK_SIZE as u64;
    let new_heap_start = timer_ist + STACK_SIZE as u64; // Reserve space for both stacks

    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_ist;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = timer_ist;
        tss
    });

    let gdt = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(tss));

        CODE_SELECTOR.call_once(|| code_selector);
        TSS_SELECTOR.call_once(|| tss_selector);
        gdt
    });

    gdt.load();

    unsafe {
        CS::set_reg(*CODE_SELECTOR.get().unwrap());
        load_tss(*TSS_SELECTOR.get().unwrap());
        serial_log("GDT: Loaded and segments set\n");
    }

    // Mark as initialized
    GDT_INITIALIZED.call_once(|| {});
    new_heap_start
}
