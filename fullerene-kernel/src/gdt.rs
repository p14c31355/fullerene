#![allow(static_mut_refs)]

use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::{debug_log_no_alloc, mem_debug};
use spin::once::Once;
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, DS, ES, FS, GS, SS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const TIMER_IST_INDEX: u16 = 1;
pub const STACK_FAULT_IST_INDEX: u16 = 2;
pub const GP_FAULT_IST_INDEX: u16 = 3;
pub const PAGE_FAULT_IST_INDEX: u16 = 4;
pub const NMI_IST_INDEX: u16 = 5;
pub const MACHINE_CHECK_IST_INDEX: u16 = 6;
pub const GDT_TSS_STACK_SIZE: usize = 4096;
pub const GDT_TSS_STACK_COUNT: usize = 7;
pub const GDT_INIT_OVERHEAD: usize = GDT_TSS_STACK_COUNT * GDT_TSS_STACK_SIZE;

pub struct TssStacks {
    pub double_fault: VirtAddr,
    pub timer: VirtAddr,
    pub stack_fault: VirtAddr,
    pub gp_fault: VirtAddr,
    pub page_fault: VirtAddr,
    pub nmi: VirtAddr,
    pub machine_check: VirtAddr,
}

pub struct Gdt {
    pub gdt: GlobalDescriptorTable,
    pub code_selector: SegmentSelector,
    pub data_selector: SegmentSelector,
    pub tss_selector: SegmentSelector,
    pub user_data_selector: SegmentSelector,
    pub user_code_selector: SegmentSelector,
    pub tss: TaskStateSegment,
}

pub static GDT: Once<Gdt> = Once::new();
pub static GDT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Returns the kernel code segment selector.
pub fn kernel_code_selector() -> SegmentSelector {
    GDT.get().unwrap().code_selector
}

/// Returns the kernel data segment selector.
pub fn kernel_data_selector() -> SegmentSelector {
    GDT.get().unwrap().data_selector
}

/// Returns the TSS selector.
pub fn tss_selector() -> SegmentSelector {
    GDT.get().unwrap().tss_selector
}

/// Returns the user data selector.
pub fn user_data_selector() -> SegmentSelector {
    GDT.get().unwrap().user_data_selector
}

/// Returns the user code selector.
pub fn user_code_selector() -> SegmentSelector {
    GDT.get().unwrap().user_code_selector
}

/// Build GDT with the given TSS and return (code, data, tss, user_data, user_code) selectors.
///
/// The TSS must be `'static` because the GDT holds a reference to it.
/// We use `transmute` to convert the mutable reference to a static reference,
/// since the TSS is stored in a static mutable variable and will live for the
/// entire kernel lifetime.
pub unsafe fn build_gdt(tss: &mut TaskStateSegment) -> (GlobalDescriptorTable, SegmentSelector, SegmentSelector, SegmentSelector, SegmentSelector, SegmentSelector) {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let data_selector = gdt.append(Descriptor::kernel_data_segment());
    let user_data_selector = gdt.append(Descriptor::user_data_segment());
    let user_code_selector = gdt.append(Descriptor::user_code_segment());
    let tss_static: &'static TaskStateSegment = unsafe { core::mem::transmute(tss) };
    let tss_selector = gdt.append(Descriptor::tss_segment(tss_static));
    (gdt, code_selector, data_selector, tss_selector, user_data_selector, user_code_selector)
}

/// Builds a GDT with the given stack configuration.
///
/// # Parameters
///
/// * `stacks` - The stack addresses for various exceptions.
///
/// Returns a fully constructed `Gdt` instance.
pub fn build_gdt(stacks: TssStacks) -> Gdt {
    // Set up IST entries in the TSS.
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stacks.double_fault;
    tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = stacks.timer;
    tss.interrupt_stack_table[STACK_FAULT_IST_INDEX as usize] = stacks.stack_fault;
    tss.interrupt_stack_table[GP_FAULT_IST_INDEX as usize] = stacks.gp_fault;
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = stacks.page_fault;
    tss.interrupt_stack_table[NMI_IST_INDEX as usize] = stacks.nmi;
    tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = stacks.machine_check;

    // Build the GDT.
    // We need to create the Gdt struct first so that tss has a stable address,
    // then append the TSS descriptor referencing that stable tss.
    let mut gdt_struct = Gdt {
        gdt: GlobalDescriptorTable::new(),
        code_selector: SegmentSelector::NULL,
        data_selector: SegmentSelector::NULL,
        tss_selector: SegmentSelector::NULL,
        user_data_selector: SegmentSelector::NULL,
        user_code_selector: SegmentSelector::NULL,
        tss,
    };

    gdt_struct.code_selector = gdt_struct.gdt.append(Descriptor::kernel_code_segment());
    gdt_struct.data_selector = gdt_struct.gdt.append(Descriptor::kernel_data_segment());
    gdt_struct.user_data_selector = gdt_struct.gdt.append(Descriptor::user_data_segment());
    gdt_struct.user_code_selector = gdt_struct.gdt.append(Descriptor::user_code_segment());

    // SAFETY: `gdt_struct.tss` is a field of the pinned `gdt_struct` value on the stack,
    // and the Gdt is immediately returned (moved into the caller). The TSS descriptor
    // pointer is only used during GDT construction and the Gdt is not moved after init.
    let tss_ptr = core::ptr::addr_of!(gdt_struct.tss);
    gdt_struct.tss_selector = gdt_struct
        .gdt
        .append(Descriptor::tss_segment(unsafe { &*tss_ptr }));

    gdt_struct
}

/// Initializes the GDT with the given stack configuration and loads it.
///
/// This function should be called early in boot, before the heap is fully set up.
pub fn init_with_stacks(stacks: TssStacks) {
    GDT.call_once(|| build_gdt(stacks));

    // Load GDT and set segment registers in non-UEFI builds.
    #[cfg(not(target_os = "uefi"))]
    {
        debug_log_no_alloc!("GDT: Loading GDT...\n");
        let gdt_ref = GDT.get().unwrap();
        gdt_ref.gdt.load();

        unsafe {
            CS::set_reg(gdt_ref.code_selector);
            load_tss(gdt_ref.tss_selector);

            DS::set_reg(gdt_ref.data_selector);
            ES::set_reg(gdt_ref.data_selector);
            FS::set_reg(gdt_ref.data_selector);
            GS::set_reg(gdt_ref.data_selector);
            SS::set_reg(gdt_ref.data_selector);
        }
        debug_log_no_alloc!("GDT: Data segment registers set\n");
    }

    debug_log_no_alloc!("GDT: Initialized and loaded\n");
}

/// Loads the existing GDT and configures segment registers.
///
/// This is used after the GDT has been initialized.
pub fn load() {
    let gdt_ref = GDT.get().expect("GDT not initialized");
    unsafe {
        gdt_ref.gdt.load();
        CS::set_reg(gdt_ref.code_selector);
        load_tss(gdt_ref.tss_selector);

        DS::set_reg(gdt_ref.data_selector);
        ES::set_reg(gdt_ref.data_selector);
        FS::set_reg(gdt_ref.data_selector);
        GS::set_reg(gdt_ref.data_selector);
        SS::set_reg(gdt_ref.data_selector);
    }
}

/// Allocate IST stacks contiguously and return (new_heap_start, tss)
unsafe fn allocate_ist_stacks(mut heap_start: VirtAddr) -> (VirtAddr, TaskStateSegment) {
    // Allocate IST stacks contiguously
    let double_fault_ist = heap_start + GDT_TSS_STACK_SIZE as u64;
    let timer_ist = double_fault_ist + GDT_TSS_STACK_SIZE as u64;
    let stack_fault_ist = timer_ist + GDT_TSS_STACK_SIZE as u64;
    let gp_fault_ist = stack_fault_ist + GDT_TSS_STACK_SIZE as u64;
    let page_fault_ist = gp_fault_ist + GDT_TSS_STACK_SIZE as u64;
    let nmi_ist = page_fault_ist + GDT_TSS_STACK_SIZE as u64;
    let machine_check_ist = nmi_ist + GDT_TSS_STACK_SIZE as u64;
    let new_heap_start = machine_check_ist + GDT_TSS_STACK_SIZE as u64;

    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_ist;
    tss.interrupt_stack_table[TIMER_IST_INDEX as usize] = timer_ist;
    tss.interrupt_stack_table[STACK_FAULT_IST_INDEX as usize] = stack_fault_ist;
    tss.interrupt_stack_table[GP_FAULT_IST_INDEX as usize] = gp_fault_ist;
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = page_fault_ist;
    tss.interrupt_stack_table[NMI_IST_INDEX as usize] = nmi_ist;
    tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = machine_check_ist;

    (new_heap_start, tss)
}

pub fn init(heap_start: VirtAddr) -> VirtAddr {
    if GDT_INITIALIZED.load(Ordering::SeqCst) {
        mem_debug!("GDT: Already initialized, skipping\n");
        return heap_start;
    }

    debug_log_no_alloc!("GDT: Initializing with heap at ", heap_start.as_u64());

    let (new_heap_start, tss) = unsafe { allocate_ist_stacks(heap_start) };

    debug_log_no_alloc!("GDT: Stack addresses calculated\n");

    unsafe {
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

    // Initialize GDT with stacks.
    init_with_stacks(stacks);
    GDT_INITIALIZED.store(true, Ordering::SeqCst);
    debug_log_no_alloc!("GDT: Init complete\n");
    new_heap_start
}
