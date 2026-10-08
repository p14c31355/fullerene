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
const DESC_CLONE_PENDING: u64 = 1 << 56;
const DESC_OUTPUT_ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;
const PAGE_SIZE: u64 = 4096;
pub(crate) const MAX_USER_SPACES: usize = 8;
// Android's interpreter and its shared objects need room beside the main
// executable. Keep the table count statically bounded while providing a
// 128 MiB user window for the early dynamic-linker bring-up.
const USER_L2_TABLES: usize = 64;
pub(crate) const USER_SPACE_BASE: u64 = 0x4000_0000;
pub(crate) const USER_SPACE_END: u64 = USER_SPACE_BASE + USER_L2_TABLES as u64 * BLOCK_SIZE;

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
    user_l3: [PageTable; USER_L2_TABLES],
    ready: bool,
}

impl Aarch64UserAddressSpace {
    const EMPTY: Self = Self {
        root: PageTable::EMPTY,
        user_l2: PageTable::EMPTY,
        user_l3: [PageTable::EMPTY; USER_L2_TABLES],
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
        // Mark q: the L1/L2 identity tables and every user-space root are
        // built; the register programming below commits them.
        super::entry::ane_step_byte(b'q');
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
        // Mark r: TTBR0/MAIR/TCR are programmed and the TLB is invalidated.
        super::entry::ane_step_byte(b'r');
        // The loader handed over with its caches in an unknown state and the
        // entry shim cleared SCTLR.M only, so every access until now bypassed
        // the caches while stale lines from the loader's own boot can still
        // sit at any address this kernel is about to reuse. Drop the
        // instruction side wholesale before the switch; the data side stays
        // disabled below (C = 0, ANE only) so a first cacheable access cannot
        // hit an unknown stale line - the measured flaky death lives exactly
        // in this window (stepmarks13: count 6, between r and s).
        asm!("ic iallu", "dsb sy", "isb", options(nostack));
        // Mark t: the loader's instruction-cache state is gone; the SCTLR
        // write is next.
        super::entry::ane_step_byte(b't');

        let mut sctlr: u64;
        asm!("mrs {sctlr}, SCTLR_EL1", sctlr = out(reg) sctlr, options(nomem, nostack));
        sctlr &= !(1 << 19); // WXN would make the RW bootstrap blocks non-executable.
        #[cfg(fullerene_aarch64_ane)]
        {
            // 2026-10-04: data cache turned ON for the measurement. With C=0
            // (uncached Normal) the kernel's byte stores to the private log
            // block landed only on even addresses - the ZH/ZB probe read back
            // "46 00 4C 44 45 00 ..." for the log's "FULLERENE..." (odd bytes
            // stale), while the loader-side probe's own stores at the same
            // addresses were clean. Cacheable write-back is the one variable
            // that differs, so C=1 plus the tee_flush's clean is the fix test.
            //
            // 2026-10-07 (v232): the C=1 switch went in WITHOUT a data-side
            // invalidate, so the loader's stale lines (and the C=0 era's)
            // survived into the cacheable world. Observed consequence: the
            // first cacheable write to a never-touched line crawls (tens of
            // ms per line - the desktop's 24 KiB fill never finished within
            // the boot's collection window; see BUG_JOURNAL 046 addendum 20).
            // Invalidate the whole L1 data cache by set/way first - the
            // data-side counterpart of the `ic iallu` above, same operand
            // layout as the Linux `invalidate_dcache_all`.
            // v237: parked under audit - after v232 every draw of the
            // desktop images died pre-USB (v234-236, 0/7 draws) while v228
            // (no invalidate) boots; the set/way operand math or an
            // unpredictable clean/writeback is the prime suspect. Do not
            // re-enable until the operand layout is verified against a
            // primary source AND the draws behave.
            invalidate_data_cache();
            sctlr |= 1 << 2;
        }
        sctlr |= 1 | (1 << 12); // MMU and instruction cache only.
        asm!(
            "msr SCTLR_EL1, {sctlr}",
            "isb",
            sctlr = in(reg) sctlr,
            options(nostack)
        );
        asm!("ic iallu", "dsb sy", "isb", options(nostack));
        // Mark s: MMU and caches are enabled (SCTLR_EL1 written, I-cache
        // invalidated) - the last statement of init.
        super::entry::ane_step_byte(b's');
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
    if virtual_address < USER_SPACE_BASE
        || virtual_address >= USER_SPACE_END
        || space_id >= MAX_USER_SPACES
        || physical_address & (PAGE_SIZE - 1) != 0
        || virtual_address & (PAGE_SIZE - 1) != 0
    {
        return false;
    }
    let l1_index = ((virtual_address >> 30) & 0x1ff) as usize;
    let l2_index = ((virtual_address >> 21) & 0x1ff) as usize;
    let l3_index = ((virtual_address >> 12) & 0x1ff) as usize;
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
        return false;
    }

    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        if !(*space).ready {
            for table_index in 0..USER_L2_TABLES {
                for page_index in 0..TABLE_ENTRIES {
                    // Preserve the identity map as EL1-only pages; explicit
                    // user mappings replace individual entries below.
                    let physical = USER_SPACE_BASE
                        + table_index as u64 * BLOCK_SIZE
                        + page_index as u64 * PAGE_SIZE;
                    core::ptr::write_volatile(
                        core::ptr::addr_of_mut!((*space).user_l3[table_index].0[page_index]),
                        page_descriptor(physical, false, true, true, false),
                    );
                }
            }
            (*space).ready = true;
        }
        let l3 = core::ptr::addr_of_mut!((*space).user_l3[l2_index]);
        // A user table is installed lazily so untouched portions retain the
        // shared identity block mapping.
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*space).user_l2.0[l2_index]),
            table_descriptor(l3 as u64),
        );
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
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
        return None;
    }
    let physical = user_page_physical_in_space(space_id, virtual_address)?;
    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let identity = USER_SPACE_BASE + l2_index as u64 * BLOCK_SIZE + l3_index as u64 * PAGE_SIZE;
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*space).user_l3[l2_index].0[l3_index]),
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
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
        return None;
    }
    unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3[l2_index].0[l3_index]));
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
    if !readable && !writable && !executable {
        return false;
    }
    let Some((l1_index, l2_index, l3_index)) = user_indices(space_id, virtual_address) else {
        return false;
    };
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
        return false;
    }
    unsafe {
        let space = core::ptr::addr_of_mut!(USER_SPACES[space_id]);
        if !(*space).ready {
            return false;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3[l2_index].0[l3_index]));
        if descriptor & DESC_VALID == 0 {
            return false;
        }
        let physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
        let copy_on_write = descriptor & DESC_COW != 0;
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*space).user_l3[l2_index].0[l3_index]),
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
    let mut success = true;
    for table_index in 0..USER_L2_TABLES {
        for page_index in 0..TABLE_ENTRIES {
            let descriptor = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(
                    (*core::ptr::addr_of!(USER_SPACES[source_id])).user_l3[table_index].0
                        [page_index]
                ))
            };
            let access = descriptor & DESC_AP_MASK;
            if descriptor & DESC_VALID == 0 || !matches!(access, DESC_AP_USER_RW | DESC_AP_USER_RO)
            {
                continue;
            }
            let source_physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
            let virtual_address =
                USER_SPACE_BASE + table_index as u64 * BLOCK_SIZE + page_index as u64 * PAGE_SIZE;
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
                unsafe {
                    core::ptr::write_volatile(
                        core::ptr::addr_of_mut!(
                            (*core::ptr::addr_of_mut!(USER_SPACES[source_id])).user_l3[table_index]
                                .0[page_index]
                        ),
                        page_descriptor(
                            source_physical,
                            true,
                            false,
                            descriptor & DESC_UXN == 0,
                            true,
                        ) | DESC_CLONE_PENDING,
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
        if !success {
            break;
        }
    }
    let restored = activate_user_space(active_space);
    if success && restored {
        // The pending marker is only needed to make the failure rollback
        // allocation-free.  Successful clones keep ordinary COW state.
        switch_ttbr0(core::ptr::addr_of!(L1) as u64);
        unsafe {
            let source = core::ptr::addr_of_mut!(USER_SPACES[source_id]);
            for table_index in 0..USER_L2_TABLES {
                for page_index in 0..TABLE_ENTRIES {
                    let address = core::ptr::addr_of!((*source).user_l3[table_index].0[page_index]);
                    let descriptor = core::ptr::read_volatile(address);
                    if descriptor & DESC_CLONE_PENDING != 0 {
                        core::ptr::write_volatile(
                            core::ptr::addr_of_mut!((*source).user_l3[table_index].0[page_index]),
                            descriptor & !DESC_CLONE_PENDING,
                        );
                    }
                }
            }
        }
        let _ = activate_user_space(active_space);
    } else {
        switch_ttbr0(core::ptr::addr_of!(L1) as u64);
        unsafe {
            let source = core::ptr::addr_of_mut!(USER_SPACES[source_id]);
            for table_index in 0..USER_L2_TABLES {
                for page_index in 0..TABLE_ENTRIES {
                    let address = core::ptr::addr_of!((*source).user_l3[table_index].0[page_index]);
                    let descriptor = core::ptr::read_volatile(address);
                    if descriptor & DESC_CLONE_PENDING != 0 {
                        let physical = descriptor & DESC_OUTPUT_ADDRESS_MASK;
                        core::ptr::write_volatile(
                            core::ptr::addr_of_mut!((*source).user_l3[table_index].0[page_index]),
                            page_descriptor(
                                physical,
                                true,
                                true,
                                descriptor & DESC_UXN == 0,
                                false,
                            ),
                        );
                    }
                }
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

    switch_ttbr0(core::ptr::addr_of!(L1) as u64);
    let mut release_ok = true;
    unsafe {
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        'tables: for table_index in 0..USER_L2_TABLES {
            for page_index in 0..TABLE_ENTRIES {
                let descriptor = core::ptr::read_volatile(core::ptr::addr_of!(
                    (*space).user_l3[table_index].0[page_index]
                ));
                if descriptor & DESC_VALID != 0
                    && matches!(descriptor & DESC_AP_MASK, DESC_AP_USER_RW | DESC_AP_USER_RO)
                {
                    let virtual_address = USER_SPACE_BASE
                        + table_index as u64 * BLOCK_SIZE
                        + page_index as u64 * PAGE_SIZE;
                    let is_shared = shared_ranges.iter().any(|(base, length)| {
                        virtual_address >= *base && virtual_address < base.saturating_add(*length)
                    });
                    if !is_shared && !frames.release_frame(descriptor & DESC_OUTPUT_ADDRESS_MASK) {
                        release_ok = false;
                        break 'tables;
                    }
                }
            }
        }
    }
    if !release_ok {
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
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
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
        core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3[l2_index].0[l3_index]))
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
                core::ptr::addr_of_mut!((*space).user_l3[l2_index].0[l3_index]),
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
                core::ptr::addr_of_mut!((*space).user_l3[l2_index].0[l3_index]),
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
        for table_index in 0..USER_L2_TABLES {
            for page_index in 0..TABLE_ENTRIES {
                let physical = USER_SPACE_BASE
                    + table_index as u64 * BLOCK_SIZE
                    + page_index as u64 * PAGE_SIZE;
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*space).user_l3[table_index].0[page_index]),
                    page_descriptor(physical, false, true, true, false),
                );
            }
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*space).user_l2.0[table_index]),
                core::ptr::read_volatile(core::ptr::addr_of!(L2_1.0[table_index])),
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
        asm!(
            "dc cvac, {root}",
            "dc cvac, {user_l2}",
            "dsb sy",
            root = in(reg) root,
            user_l2 = in(reg) user_l2,
            options(nostack)
        );
        for table_index in 0..USER_L2_TABLES {
            let user_l3 = core::ptr::addr_of!((*space).user_l3[table_index]) as u64;
            asm!(
                "dc cvac, {user_l3}",
                user_l3 = in(reg) user_l3,
                options(nostack)
            );
        }
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
    if virtual_address < USER_SPACE_BASE
        || virtual_address >= USER_SPACE_END
        || virtual_address & (PAGE_SIZE - 1) != 0
    {
        return None;
    }
    let l1_index = ((virtual_address >> 30) & 0x1ff) as usize;
    let l2_index = ((virtual_address >> 21) & 0x1ff) as usize;
    let l3_index = ((virtual_address >> 12) & 0x1ff) as usize;
    if l1_index != 1 || l2_index >= USER_L2_TABLES {
        return None;
    }
    unsafe {
        let space_id = ACTIVE_USER_SPACE;
        let space = core::ptr::addr_of!(USER_SPACES[space_id]);
        if !(*space).ready {
            return None;
        }
        let descriptor =
            core::ptr::read_volatile(core::ptr::addr_of!((*space).user_l3[l2_index].0[l3_index]));
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
        || virtual_address < USER_SPACE_BASE
        || virtual_address >= USER_SPACE_END
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

/// Invalidate the whole data cache (by set/way, every data-cache level).
///
/// Faithful Rust port of the Linux arm64 `invalidate_dcache_all`
/// (arch/arm64/mm/proc.S, 4.4): levels 0, 2, 4 ... are the D-cache levels;
/// the set/way operand is `way << clz(way_max) | set << (line_words + 4)`.
/// `dc cisw` cleans dirty lines out to the PoC before dropping them, so the
/// loader's still-dirty data is not lost. Runs before SCTLR.C is set (v232:
/// the C=1 switch went in without this, and the loader's stale lines made
/// every first-touch cacheable store crawl - BUG_JOURNAL 046 addendum 20).
unsafe fn invalidate_data_cache() {
    use core::arch::asm;
    unsafe {
        asm!("dmb sy", options(nostack));
        let clidr: u64;
        asm!("mrs {}, clidr_el1", out(reg) clidr, options(nostack));
        let loc = (clidr >> 24) & 7;
        if loc == 0 {
            return;
        }
        let mut level = 0u64;
        while level < loc {
            let ctype = (clidr >> (level * 3)) & 7;
            if ctype >= 2 {
                asm!("msr csselr_el1, {}", "isb", in(reg) level, options(nostack));
                let ccsidr: u64;
                asm!("mrs {}, ccsidr_el1", out(reg) ccsidr, options(nostack));
                let set_shift = (ccsidr & 7) as u32 + 4;
                let ways_max = ((ccsidr >> 3) & 0x3ff) as u32;
                let sets_max = ((ccsidr >> 13) & 0x7fff) as u32;
                let way_shift = (ways_max | 1).leading_zeros();
                let mut way = ways_max as i64;
                while way >= 0 {
                    let mut set = sets_max as i64;
                    while set >= 0 {
                        let operand = ((way as u64) << way_shift)
                            | level
                            | ((set as u64) << set_shift);
                        asm!("dc cisw, {}", in(reg) operand, options(nostack));
                        set -= 1;
                    }
                    way -= 1;
                }
            }
            level += 2;
        }
        asm!("msr csselr_el1, {}", in(reg) 0u64, options(nostack));
        asm!("dsb sy", "isb", options(nostack));
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

/// The active platform's Device aperture for the early identity map.
///
/// The tables live in the platform modules because the classification is a
/// property of the SoC, not of the MMU: the same 2 MiB block can be a UART on
/// one chip and DRAM on another.
fn mmio_ranges() -> &'static [(u64, u64)] {
    if cfg!(fullerene_aarch64_ane) {
        super::platform::ane::MMIO_RANGES
    } else if cfg!(fullerene_aarch64_bramble) {
        super::platform::bramble::MMIO_RANGES
    } else {
        super::platform::qemu_virt::MMIO_RANGES
    }
}

/// True when the 2 MiB identity block containing `physical` overlaps any
/// Device range.
fn in_mmio_ranges(ranges: &[(u64, u64)], physical: u64) -> bool {
    let block_end = physical.saturating_add(BLOCK_SIZE - 1);
    ranges
        .iter()
        .any(|(start, end)| physical <= *end && block_end >= *start)
}

fn is_mmio(physical: u64) -> bool {
    in_mmio_ranges(mmio_ranges(), physical)
}

#[cfg(test)]
mod tests {
    use super::super::platform;
    use super::*;

    #[test]
    fn ane_aperture_is_device_and_ram_stays_normal() {
        let ranges = platform::ane::MMIO_RANGES;
        for address in [
            0xfdf0_2000u64, // PL011 console - the first post-MMU write
            0xe862_0000,    // DSS VG0
            0xe865_a000,    // DSS WCH0
            0xff10_0000,    // susb controller window
            0xfff3_5000,    // CRG
            0xffff_ffff,    // top of the 32-bit aperture
        ] {
            assert!(
                in_mmio_ranges(ranges, address),
                "{address:#x} must be Device on hi6250"
            );
        }
        for address in [
            0x3100_0000u64, // graphic carveout - the visible scan surface
            0x01ae_0000,    // the kernel's own embedded payload island
            0x0800_0000,    // plain RAM below 0xE000_0000 (legacy list entry)
            0x8000_0000,    // second RAM bank
            0x3490_0000,    // carveout region inside RAM
            0x1234_5678,    // ordinary low RAM
        ] {
            assert!(
                !in_mmio_ranges(ranges, address),
                "{address:#x} must stay Normal on hi6250"
            );
        }
    }

    #[test]
    fn bramble_and_qemu_tables_keep_their_peripherals() {
        for ranges in [
            platform::bramble::MMIO_RANGES,
            platform::qemu_virt::MMIO_RANGES,
        ] {
            assert!(in_mmio_ranges(ranges, 0x0098_8000), "SM7250 UART");
            assert!(in_mmio_ranges(ranges, 0x17a0_0000), "SM7250 GICD");
            assert!(in_mmio_ranges(ranges, 0x0900_0000), "QEMU PL011");
            assert!(in_mmio_ranges(ranges, 0x0800_0000), "QEMU GICD");
        }
    }
}
