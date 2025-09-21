// fullerene-kernel/src/gdt.rs

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
static TSS_SELECTOR: Once<SegmentSelector> = Once::new();

pub fn init() {
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
                tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5; // 5 pages
            static mut DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(unsafe { &raw const DOUBLE_FAULT_STACK });
            stack_start + STACK_SIZE as u64
        };
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5; // 5 pages
            static mut TIMER_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(unsafe { &raw const TIMER_STACK });
            stack_start + STACK_SIZE as u64
        };
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
    }
}
