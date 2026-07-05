// SAFETY: This file uses static mut for GDT/TSS state which is accessed only during
// single-threaded kernel initialization protected by GDT_INITIALIZED AtomicBool guard.
// All accessor functions check initialization state before use.
#![allow(static_mut_refs)]
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::{debug_log_no_alloc, mem_debug};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;
pub const STACK_FAULT_IST_INDEX: u16 = 2;
pub const GP_FAULT_IST_INDEX: u16 = 3;
pub const PAGE_FAULT_IST_INDEX: u16 = 4;
pub const NMI_IST_INDEX: u16 = 5;
pub const MACHINE_CHECK_IST_INDEX: u16 = 6;

pub const GDT_TSS_STACK_SIZE: usize = 4096 * 5;
pub const GDT_TSS_STACK_COUNT: usize = 7;
pub const GDT_INIT_OVERHEAD: usize = GDT_TSS_STACK_COUNT * GDT_TSS_STACK_SIZE;

#[allow(static_mut_refs)]
pub static mut TSS: Option<TaskStateSegment> = None;
#[allow(static_mut_refs)]
static mut GDT: Option<GlobalDescriptorTable> = None;
#[allow(static_mut_refs)]
static mut CODE_SELECTOR: Option<SegmentSelector> = None;
#[allow(static_mut_refs)]
static mut KERNEL_DATA_SELECTOR: Option<SegmentSelector> = None;
#[allow(static_mut_refs)]
static mut TSS_SELECTOR: Option<SegmentSelector> = None;
#[allow(static_mut_refs)]
static mut USER_DATA_SELECTOR: Option<SegmentSelector> = None;
#[allow(static_mut_refs)]
static mut USER_CODE_SELECTOR: Option<SegmentSelector> = None;
static GDT_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[repr(align(4096))]
struct EarlyGdtBuffer([u8; 0x20000]);
static EARLY_GDT_BUFFER: EarlyGdtBuffer = EarlyGdtBuffer([0; 0x20000]);

pub fn init_early() {
    let addr = VirtAddr::from_ptr(&EARLY_GDT_BUFFER.0 as *const _ as *const u8);
    init(addr);
}

/// Unified selector accessor. Returns `None` if the selector is not yet initialized.
macro_rules! sel {
    ($static:ident) => {
        unsafe { $static.as_ref().copied() }
    };
}

pub fn code() -> Option<SegmentSelector> {
    sel!(CODE_SELECTOR)
}
pub fn kernel_data() -> Option<SegmentSelector> {
    sel!(KERNEL_DATA_SELECTOR)
}
pub fn user_code() -> Option<SegmentSelector> {
    sel!(USER_CODE_SELECTOR)
}
pub fn user_data() -> Option<SegmentSelector> {
    sel!(USER_DATA_SELECTOR)
}

/// Panicking accessors for callers that know the GDT is initialized.
pub fn kernel_code_selector() -> SegmentSelector {
    code().expect("CODE_SELECTOR not initialized")
}
pub fn kernel_data_selector() -> SegmentSelector {
    kernel_data().expect("KERNEL_DATA_SELECTOR not initialized")
}
pub fn user_code_selector() -> SegmentSelector {
    user_code().expect("USER_CODE_SELECTOR not initialized")
}
pub fn user_data_selector() -> SegmentSelector {
    user_data().expect("USER_DATA_SELECTOR not initialized")
}

/// Checked accessors that return a fallback if the GDT is not yet initialized.
pub fn code_selector_checked() -> SegmentSelector {
    code().unwrap_or(SegmentSelector::new(1, x86_64::PrivilegeLevel::Ring0))
}
pub fn user_code_selector_checked() -> SegmentSelector {
    user_code().unwrap_or(SegmentSelector::new(4, x86_64::PrivilegeLevel::Ring3))
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
    pub stack_fault: VirtAddr,
    pub gp_fault: VirtAddr,
    pub page_fault: VirtAddr,
    pub nmi: VirtAddr,
    pub machine_check: VirtAddr,
}

impl TssStacks {
    pub const fn from_base(base: VirtAddr) -> Self {
        let sz = GDT_TSS_STACK_SIZE as u64;
        Self {
            double_fault: VirtAddr::new(base.as_u64() + sz),
            timer: VirtAddr::new(base.as_u64() + sz * 2),
            stack_fault: VirtAddr::new(base.as_u64() + sz * 3),
            gp_fault: VirtAddr::new(base.as_u64() + sz * 4),
            page_fault: VirtAddr::new(base.as_u64() + sz * 5),
            nmi: VirtAddr::new(base.as_u64() + sz * 6),
            machine_check: VirtAddr::new(base.as_u64() + sz * 7),
        }
    }
}

#[allow(static_mut_refs)]
pub unsafe fn build_gdt(
    tss: &mut TaskStateSegment,
) -> (
    GlobalDescriptorTable,
    SegmentSelector,
    SegmentSelector,
    SegmentSelector,
    SegmentSelector,
    SegmentSelector,
) {
    unsafe {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let u_data_selector = gdt.append(Descriptor::user_data_segment());
        let u_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_static: &'static TaskStateSegment = core::mem::transmute(tss);
        let tss_selector = gdt.append(Descriptor::tss_segment(tss_static));
        (
            gdt,
            code_selector,
            data_selector,
            tss_selector,
            u_data_selector,
            u_code_selector,
        )
    }
}

#[allow(static_mut_refs)]
pub unsafe fn store_gdt(
    gdt: GlobalDescriptorTable,
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
    udata: SegmentSelector,
    ucode: SegmentSelector,
) {
    unsafe {
        CODE_SELECTOR = Some(code);
        KERNEL_DATA_SELECTOR = Some(data);
        TSS_SELECTOR = Some(tss);
        USER_DATA_SELECTOR = Some(udata);
        USER_CODE_SELECTOR = Some(ucode);
        GDT = Some(gdt);
    }
}

pub fn init_with_stacks(stacks: TssStacks) {
    mem_debug!("GDT: Updating TSS stacks\n");

    unsafe {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stacks.double_fault;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = stacks.timer;
        tss.interrupt_stack_table[STACK_FAULT_IST_INDEX as usize] = stacks.stack_fault;
        tss.interrupt_stack_table[GP_FAULT_IST_INDEX as usize] = stacks.gp_fault;
        tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = stacks.page_fault;
        tss.interrupt_stack_table[NMI_IST_INDEX as usize] = stacks.nmi;
        tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = stacks.machine_check;
        TSS = Some(tss);
    }

    GDT_INITIALIZED.store(true, Ordering::SeqCst);
}

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    if GDT_INITIALIZED.load(Ordering::SeqCst) {
        mem_debug!("GDT: Already initialized, skipping\n");
        return heap_start;
    }

    debug_log_no_alloc!("GDT: Initializing with heap at {}", heap_start.as_u64());

    let stacks = TssStacks::from_base(heap_start);
    let new_heap_start = stacks.machine_check + GDT_TSS_STACK_SIZE as u64;

    unsafe {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stacks.double_fault;
        tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = stacks.timer;
        tss.interrupt_stack_table[STACK_FAULT_IST_INDEX as usize] = stacks.stack_fault;
        tss.interrupt_stack_table[GP_FAULT_IST_INDEX as usize] = stacks.gp_fault;
        tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = stacks.page_fault;
        tss.interrupt_stack_table[NMI_IST_INDEX as usize] = stacks.nmi;
        tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = stacks.machine_check;
        TSS = Some(tss);
    }

    #[cfg(not(target_os = "uefi"))]
    {
        debug_log_no_alloc!("GDT: Loading GDT...\n");
        let gdt = unsafe {
            core::mem::transmute::<&GlobalDescriptorTable, &'static GlobalDescriptorTable>(
                GDT.as_ref().expect("GDT not initialized"),
            )
        };
        gdt.load();

        unsafe {
            CS::set_reg(CODE_SELECTOR.expect("CODE_SELECTOR not initialized"));
            load_tss(TSS_SELECTOR.expect("TSS_SELECTOR not initialized"));

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
    #[cfg(target_os = "uefi")]
    {
        debug_log_no_alloc!("GDT: Skipping GDT reload in UEFI mode\n");
    }

    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    new_heap_start
}
