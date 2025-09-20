// fullerene-kernel/src/gdt.rs

use core::mem;
use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::tables::{DescriptorTablePointer, load_tss};
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

static TSS: Once<TaskStateSegment> = Once::new();

static GDT_ENTRIES: Once<[u64; 4]> = Once::new();
static CODE_SELECTOR: Once<SegmentSelector> = Once::new();
static TSS_SELECTOR: Once<SegmentSelector> = Once::new();

pub fn init() {
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5; // 5 pages
            static STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&STACK);

            stack_start + STACK_SIZE as u64
        };
        tss
    });

    let gdt_entries = GDT_ENTRIES.call_once(|| {
        let mut entries = [0; 4]; // Null descriptor, Kernel Code, TSS_low, TSS_high

        let code_descriptor = Descriptor::kernel_code_segment();
        let tss_descriptor = Descriptor::tss_segment(tss);

        if let Descriptor::UserSegment(val) = code_descriptor {
            entries[1] = val;
        } else {
            panic!("Unexpected code descriptor type");
        }

        if let Descriptor::SystemSegment(val_low, val_high) = tss_descriptor {
            entries[2] = val_low;
            entries[3] = val_high;
        } else {
            panic!("Unexpected TSS descriptor type");
        }
        entries
    });

    // GDTをロード
    let ptr = DescriptorTablePointer {
        limit: (gdt_entries.len() * mem::size_of::<u64>() - 1) as u16,
        base: VirtAddr::from_ptr(gdt_entries.as_ptr()),
    };
    unsafe {
        x86_64::instructions::tables::lgdt(&ptr);
    }

    CODE_SELECTOR.call_once(|| SegmentSelector::new(1, x86_64::PrivilegeLevel::Ring0));
    TSS_SELECTOR.call_once(|| SegmentSelector::new(2, x86_64::PrivilegeLevel::Ring0));

    unsafe {
        CS::set_reg(*CODE_SELECTOR.get().unwrap());
        load_tss(*TSS_SELECTOR.get().unwrap());
    }
}
