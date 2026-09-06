use core::arch::asm;

const TABLE_ENTRIES: usize = 512;
const BLOCK_SIZE: u64 = 0x20_0000;

// Linker symbols from the platform linker script. mmu::init() runs after the
// PIE relocation bootstrap, so these addresses are resolved by then.
unsafe extern "C" {
    static __usb_dma_start: u8;
    static __usb_dma_end: u8;
    static __usb_trace_start: u8;
    static __usb_trace_end: u8;
}

const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1;
const DESC_ATTR_DEVICE: u64 = 1 << 2;
const DESC_AF: u64 = 1 << 10;
const DESC_SH_INNER: u64 = 0b11 << 8;
const DESC_AP_USER_RW: u64 = 0b01 << 6;
const DESC_AP_MASK: u64 = 0b11 << 6;
const DESC_AP_USER_RO: u64 = 0b11 << 6;
const DESC_PXN: u64 = 1 << 53;
const DESC_UXN: u64 = 1 << 54;
// AArch64 reserves bits 55..58 for software use in a stage-1 descriptor.
const DESC_COW: u64 = 1 << 55;
const DESC_OUTPUT_ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;
const PAGE_SIZE: u64 = 4096;
pub(crate) const MAX_USER_SPACES: usize = 8;

#[derive(Clone, Copy)]
#[repr(C, align(4096))]
struct PageTable([u64; TABLE_ENTRIES]);

impl PageTable {
    const EMPTY: Self = Self([0; TABLE_ENTRIES]);
}

static mut L1: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_0: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_1: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_2: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_3: PageTable = PageTable([0; TABLE_ENTRIES]);

/// The bounded bootstrap address-space object owns the user-side walk while
/// pointing its root's kernel entries at the shared identity-map L2 tables.
/// This makes the TTBR0 switch real without pretending that the static tables
/// are already allocator-owned or ASID-managed.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
struct Aarch64UserAddressSpace {
    root: PageTable,
    user_l2: PageTable,
    user_l3: PageTable,
    ready: bool,
}

impl Aarch64UserAddressSpace {
    const EMPTY: Self = Self {
        root: PageTable::EMPTY,
        user_l2: PageTable::EMPTY,
        user_l3: PageTable::EMPTY,
        ready: false,
    };
}

static mut USER_SPACES: [Aarch64UserAddressSpace; MAX_USER_SPACES] =
    [Aarch64UserAddressSpace::EMPTY; MAX_USER_SPACES];
static mut ACTIVE_USER_SPACE: usize = 0;

/// Install a small identity map covering the first 4 GiB of physical memory.
///
/// The bootstrap image, QEMU virt MMIO window, Bramble DRAM, and the platform
/// DTB all live in this range. Each 1 GiB table uses 2 MiB blocks so
/// Qualcomm's GENI UART and SM7250 GIC can be marked Device memory instead of
/// Normal memory.
pub fn init() {
    unsafe {
        let tables = [
            (0usize, core::ptr::addr_of!(L2_0)),
            (1, core::ptr::addr_of!(L2_1)),
            (2, core::ptr::addr_of!(L2_2)),
            (3, core::ptr::addr_of!(L2_3)),
        ];
        for (l1_index, table) in tables {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(L1.0[l1_index]),
                table_descriptor(table as u64),
            );
            for entry in 0..TABLE_ENTRIES {
                let physical = (l1_index as u64 * 0x4000_0000) + entry as u64 * BLOCK_SIZE;
                core::ptr::write_volatile(
                    (table as *mut PageTable).cast::<u64>().add(entry),
                    block_descriptor(physical, is_mmio(physical)),
                );
            }
        }
        for space_id in 0..MAX_USER_SPACES {
            let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
            for index in 0..TABLE_ENTRIES {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*space).user_l2.0[index]),
                    core::ptr::read_volatile(core::ptr::addr_of!(L2_1.0[index])),
                );
            }
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).root.0[0]),
                table_descriptor(core::ptr::addr_of!(L2_0) as u64),
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).root.0[1]),
                table_descriptor(core::ptr::addr_of!((*space).user_l2) as u64),
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).root.0[2]),
                table_descriptor(core::ptr::addr_of!(L2_2) as u64),
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).root.0[3]),
                table_descriptor(core::ptr::addr_of!(L2_3) as u64),
            );
        }
        // QEMU places its DTB at 0x44000000; Bramble's normal DRAM load
        // address is 0x80080000 (DRAM base plus the arm64 Image text offset).
        // Mapping through 0xffffffff keeps both entry contracts identity
        // mapped while the early kernel switches on the MMU.
        // T0SZ=25 describes a 39-bit VA space, whose translation starts at
        // level 1 for a 4 KiB granule. TTBR0 therefore points directly at L1.
        let ttbr0 = core::ptr::addr_of!(L1) as u64;
        let mair = 0x04u64 << 8 | 0xff; // Device-nGnRE at index 1, Normal WBWA at 0.
        // Cortex-A72 exposes a 40-bit physical address space (IPS=0b010).
        let tcr = 25u64 | (1 << 8) | (1 << 10) | (0b11 << 12) | (2 << 32);
        asm!("msr MAIR_EL1, {mair}", mair = in(reg) mair, options(nostack));
        asm!("msr TCR_EL1, {tcr}", tcr = in(reg) tcr, options(nostack));
        asm!("msr TTBR0_EL1, {ttbr0}", ttbr0 = in(reg) ttbr0, options(nostack));
        asm!(
            "dsb ish",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        );

        let mut sctlr: u64;
        asm!("mrs {sctlr}, SCTLR_EL1", sctlr = out(reg) sctlr, options(nomem, nostack));
        sctlr &= !(1 << 19); // WXN would make the RW bootstrap blocks non-executable.
        sctlr |= 1 | (1 << 2) | (1 << 12); // MMU, data cache, instruction cache.
        asm!(
            "msr SCTLR_EL1, {sctlr}",
            "isb",
            sctlr = in(reg) sctlr,
            options(nostack)
        );
        asm!("ic iallu", "dsb sy", "isb", options(nostack));
    }
}

/// Replace the identity 2 MiB block containing `virtual_address` with a
/// 4 KiB table and make one page accessible from EL0.
///
/// This is intentionally a bounded first mapping primitive. It gives the
/// AArch64 runtime a real user-page boundary without pretending that the
/// eventual per-process page-table allocator already exists.
pub(crate) fn map_user_page(
    space_id: usize,
    virtual_address: u64,
    physical_address: u64,
    readable: bool,
    writable: bool,
    executable: bool,
) -> bool {
    map_user_page_with_cow(
        space_id,
        virtual_address,
        physical_address,
        readable,
        writable,
        executable,
        false,
    )
}

fn map_user_page_with_cow(
    space_id: usize,
    virtual_address: u64,
    physical_address: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    copy_on_write: bool,
) -> bool {
    if virtual_address >= 0x1_0000_0000
        || space_id >= MAX_USER_SPACES
        || physical_address & (PAGE_SIZE - 1) != 0
        || virtual_address & (PAGE_SIZE - 1) != 0
    {
        return false;
    }
    let l1_index = ((virtual_address >> 30) & 0x1ff) as usize;
    let l2_index = ((virtual_address >> 21) & 0x1ff) as usize;
    let l3_index = ((virtual_address >> 12) & 0x1ff) as usize;
    if l1_index != 1 || l2_index != 0 {
        return false;
    }

    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        let l3 = core::ptr::addr_of_mut!((*space).user_l3);
        if !(*space).ready {
            for index in 0..TABLE_ENTRIES {
                // The current linker image and EL1 stack occupy the same
                // 2 MiB window as the first user VA. Preserve that window as
                // EL1-only identity pages; explicit user mappings below
                // replace individual entries with EL0 permissions.
                let physical = 0x4000_0000 + index as u64 * PAGE_SIZE;
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*l3).0[index]),
                    page_descriptor(physical, false, true, true, false),
                );
            }
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l2.0[l2_index]),
                table_descriptor(l3 as u64),
            );
            (*space).ready = true;
        }
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*l3).0[l3_index]),
            page_descriptor(
                physical_address,
                readable || writable || executable,
                writable,
                executable,
                copy_on_write,
            ),
        );
        flush_translations();
    }
    true
}

/// Remove one explicit user mapping while restoring the EL1-only identity
/// page that keeps the bootstrap kernel runnable in this 2 MiB window.
pub(crate) fn unmap_user_page(space_id: usize, virtual_address: u64) -> Option<u64> {
    let Some((l1_index, l2_index, l3_index)) = user_indices(space_id, virtual_address) else {
        return None;
    };
    if l1_index != 1 || l2_index != 0 {
        return None;
    }
    let physical = user_page_physical_in_space(space_id, virtual_address)?;
    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let identity = 0x4000_0000 + l3_index as u64 * PAGE_SIZE;
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*space).user_l3.0[l3_index]),
            page_descriptor(identity, false, true, true, false),
        );
        flush_translations();
    }
    Some(physical)
}

/// Resolve one explicit user mapping in a specific bounded address space.
///
/// Unlike `user_page_physical`, this does not depend on the currently active
/// TTBR0 root, so lifecycle code can validate a mapping before changing it.
pub(crate) fn user_page_physical_in_space(space_id: usize, virtual_address: u64) -> Option<u64> {
    let Some((l1_index, l2_index, l3_index)) = user_indices(space_id, virtual_address) else {
        return None;
    };
    if l1_index != 1 || l2_index != 0 {
        return None;
    }
    unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3.0[l3_index]));
        if descriptor & DESC_VALID == 0
            || !matches!(descriptor & DESC_AP_MASK, DESC_AP_USER_RW | DESC_AP_USER_RO)
        {
            return None;
        }
        Some(descriptor & DESC_OUTPUT_ADDRESS_MASK)
    }
}

/// Change access permissions for one already mapped user page.
pub(crate) fn protect_user_page(
    space_id: usize,
    virtual_address: u64,
    readable: bool,
    writable: bool,
    executable: bool,
) -> bool {
    let Some((l1_index, l2_index, l3_index)) = user_indices(space_id, virtual_address) else {
        return false;
    };
    if l1_index != 1 || l2_index != 0 {
        return false;
    }
    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        if !(*space).ready {
            return false;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3.0[l3_index]));
        if descriptor & DESC_VALID == 0 {
            return false;
        }
        let physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
        let copy_on_write = descriptor & DESC_COW != 0;
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*space).user_l3.0[l3_index]),
            page_descriptor(
                physical,
                readable || writable || executable,
                writable && !copy_on_write,
                executable,
                copy_on_write,
            ),
        );
        flush_translations();
    }
    true
}

/// Clone the explicit EL0 mappings from one bounded root into another using
/// copy-on-write for pages that were writable in the source.
///
/// Both roots receive read-only descriptors for a shared writable page. The
/// first subsequent EL0 store is resolved by `resolve_copy_on_write`, which
/// either makes a last-owner page private or copies it into a fresh frame.
pub(crate) fn clone_user_space(
    source_id: usize,
    target_id: usize,
    frames: &mut super::allocator::PhysicalFrameAllocator,
    shared_ranges: &[(u64, u64)],
) -> bool {
    if source_id >= MAX_USER_SPACES || target_id >= MAX_USER_SPACES || source_id == target_id {
        return false;
    }
    unsafe {
        let source = core::ptr::addr_of!(USER_SPACES[source_id]);
        if !(*source).ready {
            return false;
        }
    }
    if !reset_user_space(target_id) {
        return false;
    }
    let active_space = unsafe { ACTIVE_USER_SPACE };
    switch_ttbr0(core::ptr::addr_of!(L1) as u64);
    let mut modified_indices = [0usize; TABLE_ENTRIES];
    let mut modified_descriptors = [0u64; TABLE_ENTRIES];
    let mut modified_count = 0usize;
    let mut success = true;
    for index in 0..TABLE_ENTRIES {
        let descriptor = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(
                (*core::ptr::addr_of!(USER_SPACES[source_id])).user_l3.0[index]
            ))
        };
        let access = descriptor & DESC_AP_MASK;
        if descriptor & DESC_VALID == 0 || !matches!(access, DESC_AP_USER_RW | DESC_AP_USER_RO) {
            continue;
        }
        let source_physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
        let virtual_address = 0x4000_0000 + index as u64 * PAGE_SIZE;
        let is_shared = shared_ranges.iter().any(|(base, length)| {
            virtual_address >= *base && virtual_address < base.saturating_add(*length)
        });
        if is_shared {
            if !map_user_page_with_cow(
                target_id,
                virtual_address,
                source_physical,
                true,
                access == DESC_AP_USER_RW,
                descriptor & DESC_UXN == 0,
                false,
            ) {
                success = false;
                break;
            }
            continue;
        }
        if !frames.retain_shared_frame(source_physical) {
            success = false;
            break;
        }
        let source_writable = access == DESC_AP_USER_RW;
        let source_copy_on_write = descriptor & DESC_COW != 0;
        let copy_on_write = source_writable || source_copy_on_write;
        if source_writable && !source_copy_on_write {
            modified_indices[modified_count] = index;
            modified_descriptors[modified_count] = descriptor;
            modified_count += 1;
            unsafe {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!(
                        (*core::ptr::addr_of_mut!(USER_SPACES[source_id])).user_l3.0[index]
                    ),
                    page_descriptor(
                        source_physical,
                        true,
                        false,
                        descriptor & DESC_UXN == 0,
                        true,
                    ),
                );
            }
        }
        let executable = descriptor & DESC_UXN == 0;
        if !map_user_page_with_cow(
            target_id,
            virtual_address,
            source_physical,
            true,
            false,
            executable,
            copy_on_write,
        ) {
            let _ = frames.release_frame(source_physical);
            success = false;
            break;
        }
    }
    let restored = activate_user_space(active_space);
    if !success {
        switch_ttbr0(core::ptr::addr_of!(L1) as u64);
        unsafe {
            let source = core::ptr::addr_of_mut!(USER_SPACES[source_id]);
            for position in 0..modified_count {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*source).user_l3.0[modified_indices[position]]),
                    modified_descriptors[position],
                );
            }
        }
        let _ = release_user_space(target_id, frames, shared_ranges);
        let _ = activate_user_space(active_space);
    }
    success && restored
}

/// Release every explicit EL0 page owned by one inactive user root.
///
/// Page tables are static in this bounded port, but user frames are supplied
/// by the physical allocator. Teardown therefore has to inspect the target
/// root while the shared identity root is active, return the whole batch, and
/// only then reset the descriptors. The active task is never eligible for
/// this operation.
pub(crate) fn release_user_space(
    space_id: usize,
    frames: &mut super::allocator::PhysicalFrameAllocator,
    shared_ranges: &[(u64, u64)],
) -> bool {
    if space_id >= MAX_USER_SPACES {
        return false;
    }
    let active_space = unsafe { ACTIVE_USER_SPACE };
    if active_space == space_id {
        return false;
    }
    let ready = unsafe { (*core::ptr::addr_of!(USER_SPACES[space_id])).ready };
    if !ready {
        return true;
    }

    let mut physical_pages = [0u64; TABLE_ENTRIES];
    let mut page_count = 0usize;
    switch_ttbr0(core::ptr::addr_of!(L1) as u64);
    unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        for index in 0..TABLE_ENTRIES {
            let descriptor =
                core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3.0[index]));
            if descriptor & DESC_VALID != 0
                && matches!(descriptor & DESC_AP_MASK, DESC_AP_USER_RW | DESC_AP_USER_RO)
            {
                let virtual_address = 0x4000_0000 + index as u64 * PAGE_SIZE;
                let is_shared = shared_ranges.iter().any(|(base, length)| {
                    virtual_address >= *base && virtual_address < base.saturating_add(*length)
                });
                if !is_shared {
                    physical_pages[page_count] = descriptor & DESC_OUTPUT_ADDRESS_MASK;
                    page_count += 1;
                }
            }
        }
    }
    if !frames.release_frames(&physical_pages[..page_count]) {
        let _ = activate_user_space(active_space);
        return false;
    }
    if !reset_user_space(space_id) {
        let _ = activate_user_space(active_space);
        return false;
    }
    activate_user_space(active_space)
}

/// Resolve a write into a shared COW page for the active user space.
///
/// A page whose reference count has fallen to one can simply regain write
/// permission. Otherwise the old frame remains mapped read-only in the other
/// address spaces and this routine installs a private copied frame here.
pub(crate) fn resolve_copy_on_write(
    space_id: usize,
    virtual_address: u64,
    frames: &mut super::allocator::PhysicalFrameAllocator,
) -> bool {
    if space_id >= MAX_USER_SPACES {
        return false;
    }
    let virtual_address = virtual_address & !(PAGE_SIZE - 1);
    let Some((l1_index, l2_index, l3_index)) = user_indices(space_id, virtual_address) else {
        return false;
    };
    if l1_index != 1 || l2_index != 0 {
        return false;
    }
    if unsafe { ACTIVE_USER_SPACE != space_id } {
        return false;
    }
    let descriptor = unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return false;
        }
        core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3.0[l3_index]))
    };
    if descriptor & DESC_VALID == 0
        || descriptor & DESC_COW == 0
        || descriptor & DESC_AP_MASK != DESC_AP_USER_RO
    {
        return false;
    }
    let old_physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
    let executable = descriptor & DESC_UXN == 0;
    let Some(references) = frames.shared_frame_references(old_physical) else {
        return false;
    };

    switch_ttbr0(core::ptr::addr_of!(L1) as u64);
    if references == 1 {
        if !frames.make_frame_private(old_physical) {
            let _ = activate_user_space(space_id);
            return false;
        }
        unsafe {
            let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l3.0[l3_index]),
                page_descriptor(old_physical, true, true, executable, false),
            );
        }
        flush_translations();
        return activate_user_space(space_id);
    }

    let Some(new_physical) = frames.next_frame() else {
        let _ = activate_user_space(space_id);
        return false;
    };
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_physical as *const u8,
            new_physical as *mut u8,
            PAGE_SIZE as usize,
        );
    }
    let mapped = map_user_page_with_cow(
        space_id,
        virtual_address,
        new_physical,
        true,
        true,
        executable,
        false,
    );
    if !mapped || !frames.release_frame(old_physical) {
        unsafe {
            let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l3.0[l3_index]),
                descriptor,
            );
        }
        let _ = frames.release_frame(new_physical);
        let _ = activate_user_space(space_id);
        return false;
    }
    sync_code(new_physical);
    activate_user_space(space_id)
}

/// Clear one bounded user root before reusing its process slot.
pub(crate) fn reset_user_space(space_id: usize) -> bool {
    if space_id >= MAX_USER_SPACES {
        return false;
    }
    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        for index in 0..TABLE_ENTRIES {
            let physical = 0x4000_0000 + index as u64 * PAGE_SIZE;
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l3.0[index]),
                page_descriptor(physical, false, true, true, false),
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l2.0[index]),
                core::ptr::read_volatile(core::ptr::addr_of!(L2_1.0[index])),
            );
        }
        (*space).ready = false;
    }
    true
}

/// Make one bounded user-space root the active translation for EL0.
///
/// Each root points at the shared kernel identity-map L2 tables and owns its
/// user L2/L3. ASIDs are deliberately not enabled yet, so the root switch
/// flushes all EL1 translations before returning to the selected task.
pub(crate) fn activate_user_space(space_id: usize) -> bool {
    if space_id >= MAX_USER_SPACES {
        return false;
    }
    unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return false;
        }
        let root = core::ptr::addr_of!((*space).root) as u64;
        let user_l2 = core::ptr::addr_of!((*space).user_l2) as u64;
        let user_l3 = core::ptr::addr_of!((*space).user_l3) as u64;
        asm!(
            "dc cvac, {root}",
            "dc cvac, {user_l2}",
            "dc cvac, {user_l3}",
            "dsb sy",
            root = in(reg) root,
            user_l2 = in(reg) user_l2,
            user_l3 = in(reg) user_l3,
            options(nostack)
        );
        switch_ttbr0(root);
        ACTIVE_USER_SPACE = space_id;
    }
    true
}

fn switch_ttbr0(root: u64) {
    unsafe {
        asm!(
            "dsb sy",
            "msr TTBR0_EL1, {root}",
            "dsb sy",
            "tlbi vmalle1",
            "dsb sy",
            "isb",
            root = in(reg) root,
            options(nostack)
        );
    }
}

/// Read back the hardware root used by the active address space.
pub(crate) fn active_ttbr0() -> u64 {
    let root: u64;
    unsafe {
        asm!("mrs {root}, TTBR0_EL1", root = out(reg) root, options(nomem, nostack));
    }
    root & DESC_OUTPUT_ADDRESS_MASK
}

pub(crate) fn activate_kernel_identity_space() {
    switch_ttbr0(core::ptr::addr_of!(L1) as u64);
}

pub(crate) fn active_user_space() -> usize {
    unsafe { ACTIVE_USER_SPACE }
}

/// Publish instructions written through the identity map before EL0 fetches
/// them through the executable user mapping.
pub fn sync_code(address: u64) {
    unsafe {
        asm!(
            "dc cvac, {address}",
            "dsb ish",
            "ic ivau, {address}",
            "ic iallu",
            "dsb ish",
            "isb",
            address = in(reg) address,
            options(nostack)
        );
    }
}

/// Resolve one page only when its descriptor grants EL0 access.
pub(crate) fn user_page_physical(virtual_address: u64) -> Option<u64> {
    user_page_physical_with_access(virtual_address, true)
}

/// Resolve a user page for a kernel read. Both EL0-RW and EL0-RO/X pages are
/// readable; the copy-to-user path above continues to require EL0-RW.
pub(crate) fn user_page_physical_read(virtual_address: u64) -> Option<u64> {
    user_page_physical_with_access(virtual_address, false)
}

fn user_page_physical_with_access(virtual_address: u64, require_write: bool) -> Option<u64> {
    if virtual_address >= 0x1_0000_0000 || virtual_address & (PAGE_SIZE - 1) != 0 {
        return None;
    }
    let l1_index = ((virtual_address >> 30) & 0x1ff) as usize;
    let l2_index = ((virtual_address >> 21) & 0x1ff) as usize;
    let l3_index = ((virtual_address >> 12) & 0x1ff) as usize;
    if l1_index != 1 || l2_index != 0 {
        return None;
    }
    unsafe {
        let space_id = ACTIVE_USER_SPACE;
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3.0[l3_index]));
        if descriptor & DESC_VALID == 0
            || (require_write && descriptor & DESC_AP_MASK != DESC_AP_USER_RW)
            || (!require_write
                && !matches!(descriptor & DESC_AP_MASK, DESC_AP_USER_RW | DESC_AP_USER_RO))
        {
            return None;
        }
        Some(descriptor & DESC_OUTPUT_ADDRESS_MASK)
    }
}

fn user_indices(space_id: usize, virtual_address: u64) -> Option<(usize, usize, usize)> {
    if space_id >= MAX_USER_SPACES
        || virtual_address >= 0x1_0000_0000
        || virtual_address & (PAGE_SIZE - 1) != 0
    {
        return None;
    }
    Some((
        ((virtual_address >> 30) & 0x1ff) as usize,
        ((virtual_address >> 21) & 0x1ff) as usize,
        ((virtual_address >> 12) & 0x1ff) as usize,
    ))
}

fn flush_translations() {
    unsafe {
        asm!(
            "dsb ish",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        );
    }
}

fn table_descriptor(address: u64) -> u64 {
    (address & !0xfff) | DESC_VALID | DESC_TABLE
}

fn block_descriptor(physical: u64, device: bool) -> u64 {
    let mut descriptor = (physical & !(BLOCK_SIZE - 1)) | DESC_VALID | DESC_AF;
    if device {
        descriptor |= DESC_ATTR_DEVICE | DESC_PXN | DESC_UXN;
    } else {
        descriptor |= DESC_SH_INNER;
    }
    descriptor
}

fn page_descriptor(
    physical: u64,
    user: bool,
    writable: bool,
    executable: bool,
    copy_on_write: bool,
) -> u64 {
    let mut descriptor =
        (physical & !(PAGE_SIZE - 1)) | DESC_VALID | DESC_TABLE | DESC_AF | DESC_SH_INNER;
    if user {
        descriptor |= if writable {
            DESC_AP_USER_RW
        } else {
            DESC_AP_USER_RO
        };
    }
    if !executable {
        descriptor |= DESC_UXN;
    }
    if user && copy_on_write {
        descriptor |= DESC_COW;
    }
    descriptor
}

fn is_mmio(physical: u64) -> bool {
    const MMIO_RANGES: &[(u64, u64)] = &[
        (0x0800_0000, 0x09ff_ffff),
        (0x0010_0000, 0x001f_ffff),
        (0x0080_0000, 0x009f_ffff),
        (0x17a0_0000, 0x17c1_ffff),
        (0x0a60_0000, 0x0a6f_ffff),
        (0x1500_0000, 0x153f_ffff),
        (0x0c40_0000, 0x0e7f_ffff),
    ];
    let block_end = physical.saturating_add(BLOCK_SIZE - 1);
    if MMIO_RANGES
        .iter()
        .any(|(start, end)| physical <= *end && block_end >= *start)
    {
        return true;
    }
    false
}
