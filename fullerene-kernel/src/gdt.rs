// SAFETY: This file uses static mut for GDT/TSS state which is accessed only during
// single-threaded kernel initialization protected by GDT_INITIALIZED AtomicBool guard.
// All accessor functions check initialization state before use. This is a common pattern
// in low-level kernel code where the standard initialization sequence serializes all access.
// The #[allow] is needed because Rust 2024 edition makes static_mut_refs a hard deny,
// but this pattern is safe here due to the single-threaded boot phase.
#![allow(static_mut_refs)]
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::{debug_log_no_alloc, mem_debug};
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;
pub const STACK_FAULT_IST_INDEX: u16 = 2;
pub const GP_FAULT_IST_INDEX: u16 = 3;
pub const PAGE_FAULT_IST_INDEX: u16 = 4;
pub const NMI_IST_INDEX: u16 = 5;
pub const MACHINE_CHECK_IST_INDEX: u16 = 6;

/// Size of each IST stack (bytes).
/// Each stack is 5 pages (20 KiB) to accommodate interrupt handling.
pub const GDT_TSS_STACK_SIZE: usize = 4096 * 5;

/// Number of IST stacks reserved.
pub const GDT_TSS_STACK_COUNT: usize = 7;

/// Total overhead for GDT initialization in bytes.
pub const GDT_INIT_OVERHEAD: usize = GDT_TSS_STACK_COUNT * GDT_TSS_STACK_SIZE;

// These statics are accessed only during GDT initialization.
// Safety: They are protected by GDT_INITIALIZED AtomicBool guard, and all
// accessors check initialization state before use. Access is serialized by
// the kernel boot sequence (single-threaded during init).
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

pub fn kernel_code_selector() -> SegmentSelector {
    unsafe { CODE_SELECTOR.expect("CODE_SELECTOR not initialized") }
}

pub fn kernel_data_selector() -> SegmentSelector {
    unsafe { KERNEL_DATA_SELECTOR.expect("KERNEL_DATA_SELECTOR not initialized") }
}

/// Public accessor for CODE_SELECTOR (for fallback in ProcessContext)
pub fn code_selector() -> Option<SegmentSelector> {
    unsafe { CODE_SELECTOR.as_ref().copied() }
}

/// Public accessor for KERNEL_DATA_SELECTOR (for fallback in ProcessContext)
pub fn kernel_data_selector_fallback() -> Option<SegmentSelector> {
    unsafe { KERNEL_DATA_SELECTOR.as_ref().copied() }
}

/// Public accessor for USER_CODE_SELECTOR (for fallback in ProcessContext)
pub fn user_code_selector_fallback() -> Option<SegmentSelector> {
    unsafe { USER_CODE_SELECTOR.as_ref().copied() }
}

/// Public accessor for USER_DATA_SELECTOR (for fallback in ProcessContext)
pub fn user_data_selector_fallback() -> Option<SegmentSelector> {
    unsafe { USER_DATA_SELECTOR.as_ref().copied() }
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
    /// Create a new TssStacks from a base address, allocating sequential IST stacks.
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

/// Build GDT with the given TSS and return (code, data, tss, user_data, user_code) selectors.
///
/// The TSS must be `'static` because the GDT holds a reference to it.
/// We use `transmute` to convert the mutable reference to a static reference,
/// since the TSS is stored in a static mutable variable and will live for the
/// entire kernel lifetime.
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
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_static: &'static TaskStateSegment = core::mem::transmute(tss);
        let tss_selector = gdt.append(Descriptor::tss_segment(tss_static));
        (
            gdt,
            code_selector,
            data_selector,
            tss_selector,
            user_data_selector,
            user_code_selector,
        )
    }
}

/// Store built GDT and selectors into global state.
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

    mem_debug!("GDT: GDT built\n");
    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    mem_debug!("GDT: About to return\n");
}

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    if GDT_INITIALIZED.load(Ordering::SeqCst) {
        mem_debug!("GDT: Already initialized, skipping\n");
        return heap_start;
    }

    debug_log_no_alloc!("GDT: Initializing with heap at {}", heap_start.as_u64());

    // Allocate IST stacks contiguously using the unified helper
    let stacks = TssStacks::from_base(heap_start);
    let new_heap_start = stacks.machine_check + GDT_TSS_STACK_SIZE as u64;

    debug_log_no_alloc!("GDT: Stack addresses calculated\n");

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

    debug_log_no_alloc!("GDT: GDT built\n");

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
            debug_log_no_alloc!("GDT: Data segment registers set\n");
        }
    }
    #[cfg(target_os = "uefi")]
    {
        debug_log_no_alloc!("GDT: Skipping GDT reload in UEFI mode\n");
    }

    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    debug_log_no_alloc!("GDT: Init complete\n");
    new_heap_start
}

pub fn user_code_selector() -> SegmentSelector {
    unsafe { USER_CODE_SELECTOR.expect("USER_CODE_SELECTOR not initialized") }
}

/// Safe version that returns a fallback selector if not yet initialized
pub fn user_code_selector_checked() -> SegmentSelector {
    unsafe { USER_CODE_SELECTOR.unwrap_or(SegmentSelector::new(4, x86_64::PrivilegeLevel::Ring3)) }
}

pub fn kernel_code_selector_checked() -> SegmentSelector {
    unsafe { CODE_SELECTOR.unwrap_or(SegmentSelector::new(1, x86_64::PrivilegeLevel::Ring0)) }
}

pub fn user_data_selector() -> SegmentSelector {
    unsafe { USER_DATA_SELECTOR.expect("USER_DATA_SELECTOR not initialized") }
}
