//! Apps-SMMU DMA ownership for the DWC3 stream.
//!
//! This module keeps the stage-1 identity table, the handoff decision, and
//! fault recovery together. The parent retains only the DMA objects whose
//! addresses are handed to DWC3.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use super::super::platform::bramble::DmaPoolResource;
use super::log::{log_hex, log_puts};
use super::mmio::*;
use super::trace::{
    TRACE_SMMU_BEGIN, TRACE_SMMU_FAULT, TRACE_SMMU_GLOBAL_FAULT, TRACE_SMMU_HANDOFF,
    TRACE_SMMU_PRESERVED, TRACE_SMMU_READY, trace_event,
};

// Parent-owned DMA adoption state. `adopt_smmu_dma_mapping` publishes the
// CPU/IOVA pair used by every EP0 transfer setup helper.
#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct SmmuTable([u64; 512]);

// With T0SZ=32 and a 4 KiB granule, TTBR0 points at a level-1 table. The
// vendor DT's 0x90000000..0xf0000000 pool is represented by level-2 2 MiB
// identity blocks; addresses outside the pool are left unmapped. These tables
// are cleared together with the other USB DMA objects.
#[unsafe(link_section = ".usb_dma")]
static mut SMMU_L1: SmmuTable = SmmuTable([0; 512]);
#[unsafe(link_section = ".usb_dma")]
static mut SMMU_L2: [SmmuTable; 4] = [SmmuTable([0; 512]); 4];
static mut SMMU_CONTEXT_PAGE: usize = usize::MAX;
static mut SMMU_CONTEXT_PAGE_SIZE: usize = 0;

/// Read-only Apps-SMMU stage-1 walk. Returns the physical output address for
/// `iova` when the stream's context bank translation maps it with a valid
/// 4 KiB page (or 2 MiB block whose base contains the whole 4 KiB window).
unsafe fn smmu_walk_iova(ttbr0: u64, three_level: bool, iova: u64) -> Option<u64> {
    const DESC_VALID: u64 = 1;
    const DESC_TABLE: u64 = 2;
    unsafe {
        let mut table = (ttbr0 & !0xfff) as usize;
        // 39-bit IOVA (T0SZ=25, 4 KiB granule): L1 root. 32-bit IOVA
        // (T0SZ=32): the walk starts at level 2 with 2 MiB blocks.
        let (l1_index, l2_index, l3_index) = if three_level {
            (
                ((iova >> 30) & 0x1ff) as usize,
                ((iova >> 21) & 0x1ff) as usize,
                ((iova >> 12) & 0x1ff) as usize,
            )
        } else {
            (
                0,
                ((iova >> 21) & 0x1ff) as usize,
                ((iova >> 12) & 0x1ff) as usize,
            )
        };
        if three_level {
            let descriptor = read_volatile((table + l1_index * 8) as *const u64);
            if descriptor & DESC_VALID == 0 || descriptor & DESC_TABLE == 0 {
                return None;
            }
            table = (descriptor & !0xfff) as usize;
        }
        let l2_descriptor = read_volatile((table + l2_index * 8) as *const u64);
        if l2_descriptor & DESC_VALID == 0 {
            return None;
        }
        if l2_descriptor & DESC_TABLE == 0 {
            // 2 MiB block: the whole 4 KiB window must fit inside it. The
            // block base is 2 MiB aligned, so the low attribute bits do not
            // overlap the output address.
            let block = (l2_descriptor & 0x000f_ffff_ffff_f000) as usize;
            let offset = (iova as usize) & 0x1f_ffff;
            if offset + 0x1000 > 0x20_0000 {
                return None;
            }
            return Some((block | offset) as u64);
        }
        let l3_table = (l2_descriptor & !0xfff) as usize;
        let l3_descriptor = read_volatile((l3_table + l3_index * 8) as *const u64);
        if l3_descriptor & DESC_VALID == 0 || l3_descriptor & DESC_TABLE != 0 {
            return None;
        }
        Some(l3_descriptor & 0x000f_ffff_ffff_f000)
    }
}

/// Read-only Apps-SMMU stream/context discovery. Returns the context-bank
/// page index when the DWC3 stream matches a valid SMR whose S2CR selects an
/// active TRANSLATE context. Never writes the SMMU.
unsafe fn smmu_find_translate_context() -> Option<(usize, usize, usize)> {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return None;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let num_pages =
            1usize << (((id1 >> SMMU_ID1_NUMPAGENDXB_SHIFT) & SMMU_ID1_NUMPAGENDXB_MASK) + 1);
        let num_context_banks = (id1 & SMMU_ID1_NUMCB_MASK) as usize;
        if num_pages == 0 || num_context_banks == 0 {
            return None;
        }
        let stream_id = super::super::platform::bramble::usb_resources()
            .dma_pool
            .stream_id;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask != 0 {
                continue;
            }
            let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
            if s2cr & SMMU_S2CR_TYPE_MASK != SMMU_S2CR_TYPE_TRANS {
                // BYPASS or FAULT: the CPU address is already the DMA
                // address and the normal linker section works.
                return None;
            }
            let cbndx = (s2cr & SMMU_S2CR_CBNDX_MASK) as usize;
            if cbndx >= num_context_banks {
                return None;
            }
            return Some((cbndx, num_pages, num_context_banks));
        }
        None
    }
}

/// Find ANY valid 4 KiB page mapping inside the live context's stage-1 tables
/// and return (iova, physical). The bootloader context is no longer active
/// once `fastboot boot` jumped away, so any page it mapped is a fair DMA
/// window for Fullerene's EP0 objects.
unsafe fn smmu_find_any_mapping(ttbr0: u64, three_level: bool) -> Option<(u64, u64)> {
    const DESC_VALID: u64 = 1;
    const DESC_TABLE: u64 = 2;
    unsafe {
        let l1_table = (ttbr0 & !0xfff) as usize;
        let l1_range = if three_level { 512usize } else { 1 };
        for l1_index in 0..l1_range {
            let mut l2_table = l1_table;
            if three_level {
                let l1_descriptor = read_volatile((l1_table + l1_index * 8) as *const u64);
                if l1_descriptor & DESC_VALID == 0 || l1_descriptor & DESC_TABLE == 0 {
                    continue;
                }
                l2_table = (l1_descriptor & !0xfff) as usize;
            }
            for l2_index in 0..512usize {
                let l2_descriptor = read_volatile((l2_table + l2_index * 8) as *const u64);
                if l2_descriptor & DESC_VALID == 0 {
                    continue;
                }
                let iova_base = ((l1_index as u64) << 30) | ((l2_index as u64) << 21);
                if l2_descriptor & DESC_TABLE == 0 {
                    // A 2 MiB block: use its first 4 KiB window.
                    let block = l2_descriptor & 0x000f_ffff_ffff_f000;
                    return Some((iova_base, block));
                }
                let l3_table = (l2_descriptor & !0xfff) as usize;
                for l3_index in 0..512usize {
                    let l3_descriptor = read_volatile((l3_table + l3_index * 8) as *const u64);
                    if l3_descriptor & DESC_VALID == 0 || l3_descriptor & DESC_TABLE != 0 {
                        continue;
                    }
                    let physical = l3_descriptor & 0x000f_ffff_ffff_f000;
                    if physical == 0 {
                        continue;
                    }
                    return Some((iova_base | ((l3_index as u64) << 12), physical));
                }
            }
        }
        None
    }
}

/// Read-only Apps-SMMU stream-state ladder. Returns the deepest condition
/// that provably holds, so a host-visible attach gate can name the state one
/// run at a time:
///   0..=3 = an SMR matched the stream and its S2CR type is that value
///   251   = SMRs are implemented but none is valid
///   252   = at least one valid SMR exists but none matches the stream
///   253   = no SMRs are implemented (ID0.NUMSMRG == 0)
///   254   = the SMMU identification registers are unreadable (RAZ/all-ones)
pub(super) unsafe fn smmu_stream_s2cr_type() -> u32 {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return 254;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        if num_smrs == 0 {
            return 253;
        }
        let stream_id = super::super::platform::bramble::usb_resources()
            .dma_pool
            .stream_id;
        let mut any_valid = false;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            any_valid = true;
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask != 0 {
                continue;
            }
            let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
            return (s2cr & SMMU_S2CR_TYPE_MASK) >> 16;
        }
        if any_valid {
            // Stream unmatched. Distinguish an active SMMU (252) from a
            // globally bypassed one (250): with SMMUEN=0 or CLIENTPD=1 the
            // unmatched SMR is irrelevant and transactions already pass
            // untranslated.
            let scr0 = read_volatile(smmu_reg(SMMU_GR0_SCR0));
            let active = scr0 != u32::MAX && (scr0 & 1) != 0 && (scr0 & 2) == 0;
            return if active { 252 } else { 250 };
        }
        251
    }
}

/// Claim a free Apps-SMMU SMR slot for the DWC3 stream and point its S2CR at
/// BYPASS so the stream's transactions pass untranslated (CPU address ==
/// DMA address). Only slots whose VALID bit is clear are claimed, both
/// writes are verified by readback, and a rejected (secure-owned) write is
/// reported as failure instead of being assumed. The stream is known to be
/// unmatched (ladder 252) when this runs, so no live mapping is displaced.
pub(super) unsafe fn smmu_install_stream_bypass() -> bool {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return false;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        if num_smrs == 0 {
            return false;
        }
        let stream_id = super::super::platform::bramble::usb_resources()
            .dma_pool
            .stream_id
            & 0xffff;
        // An existing match means the stream already has an owner: leave the
        // configuration alone and report success (nothing to install).
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask == 0 {
                trace_event(TRACE_SMMU_HANDOFF, 0x494E, index as u32, 0, 1, 0);
                return true;
            }
        }
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID != 0 {
                continue;
            }
            // Claim this slot. Write the inert S2CR first (it only applies
            // once SMR.VALID is set), then publish the SMR, then verify both
            // by readback before declaring the stream owned. The catch-all
            // mode matches every stream ID so a misreported DWC3 stream ID
            // cannot keep the stream faulting.
            let catch_all = option_env!("FULLERENE_USB_SMMU_INSTALL_ALL") == Some("1");
            let smr_value = if catch_all {
                SMMU_SMR_VALID | (0x7fff << SMMU_SMR_MASK_SHIFT)
            } else {
                SMMU_SMR_VALID | stream_id
            };
            let s2cr_address = SMMU_S2CR_BASE + index * 4;
            let old_s2cr = read_volatile(smmu_reg(s2cr_address));
            let new_s2cr = (old_s2cr & !SMMU_S2CR_TYPE_MASK) | SMMU_S2CR_TYPE_BYPASS;
            write_volatile(smmu_reg(s2cr_address), new_s2cr);
            core::arch::asm!("dsb sy", options(nostack));
            write_volatile(smmu_reg(SMMU_SMR_BASE + index * 4), smr_value);
            core::arch::asm!("dsb sy", options(nostack));
            let smr_readback = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            let s2cr_readback = read_volatile(smmu_reg(s2cr_address));
            let smr_ok = smr_readback & SMMU_SMR_VALID != 0 && smr_readback == smr_value;
            let s2cr_ok = s2cr_readback & SMMU_S2CR_TYPE_MASK == SMMU_S2CR_TYPE_BYPASS;
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x534D_5200 | index as u32,
                smr_readback,
                s2cr_readback,
                smr_ok as u32,
                s2cr_ok as u32,
            );
            if smr_ok && s2cr_ok {
                smmu_tlb_sync();
                log_hex("usb: installed SMMU bypass SMR index=", index as u64);
                return true;
            }
            // The secure side rejected at least one write. Restore the slot
            // to its inert state as far as non-secure writes reach and
            // report the failure.
            write_volatile(smmu_reg(SMMU_SMR_BASE + index * 4), smr);
            write_volatile(smmu_reg(s2cr_address), old_s2cr);
            core::arch::asm!("dsb sy", options(nostack));
            return false;
        }
        false
    }
}
/// Relocate the EP0 DMA objects into a page that the live Apps-SMMU context
/// already maps. The stream's S2CR is TRANSLATE and software cannot rewrite
/// it from non-secure state, so the only working DMA window is one the
/// bootloader context maps: capture the bootloader's event-ring IOVA, walk
/// its page tables read-only, and adopt that page (CPU side = physical,
/// DWC3 side = IOVA). Returns the IOVA of the adopted page.
pub(super) unsafe fn adopt_smmu_dma_mapping() -> Option<u64> {
    unsafe {
        let (cbndx, num_pages, _) = smmu_find_translate_context()?;
        let page_size = 0x1000usize;
        let cb_page = num_pages + cbndx;
        let sctlr = smmu_page_read(page_size, cb_page, SMMU_CB_SCTLR);
        if sctlr & SMMU_SCTLR_M == 0 {
            // Translation disabled: the CPU address is the DMA address.
            return None;
        }
        let ttbr0 = smmu_page_read64(page_size, cb_page, SMMU_CB_TTBR0);
        if ttbr0 == 0 || ttbr0 == u64::MAX {
            return None;
        }
        let three_level = super::super::platform::bramble::usb_resources().smmu_use_3_level_tables;
        let iova =
            (read_volatile(reg(GEVNTADRHI0)) as u64) << 32 | read_volatile(reg(GEVNTADRLO0)) as u64;
        if iova == 0
            || iova == u64::MAX
            || iova & 0xfff != 0
            || iova > usize::MAX as u64
            || iova >= 1 << (if three_level { 39 } else { 32 })
        {
            return None;
        }
        let (iova, physical) = match smmu_walk_iova(ttbr0, three_level, iova) {
            Some(physical) if physical != 0 => (iova, physical as u64),
            // The bootloader's own event-ring IOVA is no longer resolvable
            // (its teardown may have cleared GEVNTADR or the page). Adopt
            // ANY page the live context still maps instead: the bootloader
            // is gone and no other master owns the stream.
            _ => smmu_find_any_mapping(ttbr0, three_level)?,
        };
        if physical == 0 || iova & 0xfff != 0 {
            return None;
        }
        super::DMA_ADOPTED_CPU = physical as usize;
        super::DMA_ADOPTED_IOVA = iova;
        super::DMA_ADOPTED = true;
        log_hex("usb: adopted SMMU page physical=", physical);
        log_hex("usb: adopted SMMU page iova=", iova);
        Some(iova)
    }
}

/// Verify that an already-live S1 context translates the complete linker DMA
/// section as an identity map. A Fastboot handoff may safely preserve Linux's
/// context only when its page tables cover every Fullerene TRB/event object;
/// preserving an unrelated bootloader map would make the first DMA fault look
/// like an EP0 protocol failure.
unsafe fn smmu_context_maps_identity(ttbr0: u64, tcr: u32, start: usize, end: usize) -> bool {
    let iova_start = start as u64;
    let iova_end = end as u64;
    if ttbr0 == 0
        || ttbr0 == u64::MAX
        || start >= end
        || iova_end > (1u64 << 39)
        || (tcr >> 14) & 0x3 != 0
    {
        return false;
    }

    // Linux's qcom,use-3-lvl-tables caps the AArch64 aperture at 39 bits;
    // T0SZ=25 (39-bit) and T0SZ=32 (32-bit) both use an L1->L2->L3 walk for
    // a 4 KiB granule. This checker intentionally refuses an unfamiliar
    // format instead of guessing at a table level.
    let iova_bits = 64u32.saturating_sub(tcr & 0x3f);
    if !(30..=39).contains(&iova_bits) {
        return false;
    }

    // Linux stores the context ASID in TTBR0[63:48]. It is not part of the
    // physical page-table address and must be removed before the CPU-side
    // identity walk below; otherwise a live Android context can appear
    // unmapped solely because its ASID is non-zero.
    let table_root = ttbr0 & 0x0000_ffff_ffff_f000;
    let read_entry = |base: u64, index: u64| -> Option<u64> {
        let offset = index.checked_mul(8)?;
        let address = base.checked_add(offset)?;
        (address <= usize::MAX as u64)
            .then(|| unsafe { read_volatile(address as usize as *const u64) })
    };
    let maps_one_2m_block = |address: u64| -> bool {
        let l1_index = (address >> 30) & 0x1ff;
        let Some(l1) = read_entry(table_root, l1_index) else {
            return false;
        };
        match l1 & SMMU_DESC_TYPE_MASK {
            // An existing Android/IOMMU mapping may use a 1 GiB identity
            // block for a large DMA aperture. It is just as valid for the
            // preservation check as the finer-grained L2/L3 forms.
            SMMU_DESC_BLOCK => {
                l1 & SMMU_DESC_ADDRESS_MASK == address & !((1u64 << 30) - 1)
                    && l1 & SMMU_DESC_VALID != 0
            }
            SMMU_DESC_TABLE => {
                let l2_base = l1 & SMMU_DESC_ADDRESS_MASK;
                let l2_index = (address >> 21) & 0x1ff;
                let Some(l2) = read_entry(l2_base, l2_index) else {
                    return false;
                };
                let block_base = address & !((1u64 << 21) - 1);
                match l2 & SMMU_DESC_TYPE_MASK {
                    SMMU_DESC_BLOCK => {
                        l2 & SMMU_DESC_ADDRESS_MASK == block_base && l2 & SMMU_DESC_VALID != 0
                    }
                    SMMU_DESC_TABLE => {
                        let l3_base = l2 & SMMU_DESC_ADDRESS_MASK;
                        let l3_index = (address >> 12) & 0x1ff;
                        let Some(l3) = read_entry(l3_base, l3_index) else {
                            return false;
                        };
                        l3 & SMMU_DESC_TYPE_MASK == SMMU_DESC_TABLE
                            && l3 & SMMU_DESC_ADDRESS_MASK == address & !0xfff
                            && l3 & SMMU_DESC_VALID != 0
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    };

    let mut address = iova_start & !((1u64 << 21) - 1);
    while address < iova_end {
        if !maps_one_2m_block(address) {
            return false;
        }
        address = address.saturating_add(1u64 << 21);
    }
    true
}

unsafe fn smmu_tlb_sync() {
    unsafe {
        write_volatile(smmu_reg(SMMU_TLB_ALL_H), 0);
        write_volatile(smmu_reg(SMMU_TLB_SYNC), 0);
        for _ in 0..100_000u32 {
            if read_volatile(smmu_reg(SMMU_TLB_STATUS)) & SMMU_TLB_STATUS_ACTIVE == 0 {
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Consume an Apps-SMMU context fault using the same ordering as Linux's
/// arm_smmu_context_fault(): sample FSR/FAR/FSYNR0, retain the evidence, then
/// clear FSR and terminate a stalled transaction. This is polled because the
/// The normal entry is now the Apps-SMMU global/context IRQ path; polling
/// remains as a recovery path for the short interval before the GIC owner is
/// installed or when firmware leaves a latched fault asserted.
pub(super) unsafe fn service_smmu_fault() {
    let global_fsr = unsafe { read_volatile(smmu_reg(SMMU_GR0_FSR)) };
    if global_fsr != 0 && global_fsr != u32::MAX && global_fsr & SMMU_GLOBAL_FSR_FAULT != 0 {
        let global_fsynr0 = unsafe { read_volatile(smmu_reg(SMMU_GR0_FSYNR0)) };
        trace_event(TRACE_SMMU_GLOBAL_FAULT, global_fsr, global_fsynr0, 0, 0, 0);
        log_hex("usb: Apps SMMU global fault FSR=", global_fsr as u64);
        unsafe { write_volatile(smmu_reg(SMMU_GR0_FSR), global_fsr) };
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }
    let page = unsafe { SMMU_CONTEXT_PAGE };
    let page_size = unsafe { SMMU_CONTEXT_PAGE_SIZE };
    if page == usize::MAX || page_size == 0 {
        return;
    }
    let fsr = unsafe { smmu_page_read(page_size, page, SMMU_CB_FSR) };
    if fsr == 0 || fsr == u32::MAX || fsr & SMMU_FSR_FAULT == 0 {
        return;
    }
    let far = unsafe { smmu_page_read64(page_size, page, SMMU_CB_FAR) };
    let fsynr0 = unsafe { smmu_page_read(page_size, page, SMMU_CB_FSYNR0) };
    trace_event(
        TRACE_SMMU_FAULT,
        fsr,
        fsynr0,
        far as u32,
        (far >> 32) as u32,
        page as u32,
    );
    log_hex("usb: Apps SMMU context fault FSR=", fsr as u64);
    log_hex("usb: Apps SMMU FAR=", far);
    unsafe { smmu_page_write(page_size, page, SMMU_CB_FSR, fsr) };
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    if fsr & SMMU_FSR_SS != 0 {
        unsafe { smmu_page_write(page_size, page, SMMU_CB_RESUME, SMMU_RESUME_TERMINATE) };
    }
}

unsafe fn install_smmu_identity_table(pool: DmaPoolResource) -> bool {
    let pool_base = pool.iova_base as u64;
    let Some(pool_end) = pool_base.checked_add(pool.size as u64) else {
        return false;
    };
    // This table format intentionally maps only complete 2 MiB blocks. The
    // Android Lito pool is aligned to that granularity; refusing an unusual
    // DT pool is safer than exposing bytes outside the declared DMA window.
    if pool_base & 0x1f_ffff != 0
        || pool_end & 0x1f_ffff != 0
        || pool_end > (1u64 << 32)
        || pool_base >= pool_end
    {
        return false;
    }

    unsafe {
        for index in 0..512usize {
            write_volatile(addr_of_mut!(SMMU_L1.0[index]), 0);
        }
        for table in 0..4usize {
            for index in 0..512usize {
                write_volatile(addr_of_mut!(SMMU_L2[table].0[index]), 0);
            }
        }

        for l1_index in 0..4usize {
            let l1_base = (l1_index as u64) << 30;
            let l1_end = l1_base + (1u64 << 30);
            if pool_base >= l1_end || pool_end <= l1_base {
                continue;
            }
            // The L1 table descriptor points at the corresponding 4 KiB
            // level-2 table. Each L2 entry describes a 2 MiB block.
            let table_address = addr_of!(SMMU_L2[l1_index]) as usize as u64;
            write_volatile(
                addr_of_mut!(SMMU_L1.0[l1_index]),
                (table_address & !0xfff) | SMMU_DESC_TABLE,
            );
            for l2_index in 0..512usize {
                let physical = l1_base + (l2_index as u64) * (1u64 << 21);
                if physical >= pool_base && physical + (1u64 << 21) <= pool_end {
                    let descriptor = physical
                        | SMMU_DESC_VALID
                        | SMMU_DESC_AF
                        | SMMU_DESC_SH_INNER
                        | SMMU_DESC_ATTR_NORMAL
                        | SMMU_DESC_XN;
                    write_volatile(addr_of_mut!(SMMU_L2[l1_index].0[l2_index]), descriptor);
                }
            }
        }
        super::cache_clean(
            addr_of!(SMMU_L1) as usize,
            core::mem::size_of::<SmmuTable>(),
        );
        super::cache_clean(
            addr_of!(SMMU_L2) as usize,
            core::mem::size_of::<[SmmuTable; 4]>(),
        );
    }
    true
}

/// Install an AArch64 stage-1 identity mapping for DWC3's stream ID.
///
/// Bramble's vendor DT assigns DWC3 stream ID 0xe0 to the Apps SMMU and puts
/// USB buffers in the 0x90000000..0xf0000000 IOVA pool. Qualcomm's SMMU-500
/// firmware can reject a direct BYPASS write by turning it into FAULT, so a
/// real context-bank map is required here. We preserve the existing SMR and
/// route it to a context bank configured as S1 translation + S2 bypass.
pub fn configure_dwc3_smmu() -> bool {
    unsafe {
        // A failed reconfiguration must not leave the fault poller pointing
        // at a context bank from an earlier controller lifetime.
        SMMU_CONTEXT_PAGE = usize::MAX;
        SMMU_CONTEXT_PAGE_SIZE = 0;
        let pool = super::super::platform::bramble::usb_resources().dma_pool;
        let dma_start = addr_of!(super::__usb_dma_start) as usize;
        let dma_end = addr_of!(super::__usb_dma_end) as usize;
        trace_event(
            TRACE_SMMU_BEGIN,
            apps_smmu_base() as u32,
            pool.stream_id,
            dma_start as u32,
            dma_end as u32,
            pool.iova_base as u32,
        );
        let Some(pool_end) = pool.iova_base.checked_add(pool.size) else {
            log_puts("usb: invalid DT DMA pool\n");
            return false;
        };
        if dma_start < pool.iova_base || dma_end > pool_end || dma_start >= dma_end {
            log_puts("usb: DMA section is outside the DT IOVA pool\n");
            return false;
        }
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            log_puts("usb: Apps SMMU identification unavailable\n");
            return false;
        }

        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let page_size = if id1 & SMMU_ID1_PAGESIZE != 0 {
            0x10000
        } else {
            0x1000
        };
        if page_size != 0x1000 {
            // The table below is intentionally 4 KiB-granule LPAE. Do not
            // enable a mismatched table on a future 64 KiB-only SMMU.
            log_puts("usb: Apps SMMU requires unsupported 64K tables\n");
            return false;
        }

        let num_pages =
            1usize << (((id1 >> SMMU_ID1_NUMPAGENDXB_SHIFT) & SMMU_ID1_NUMPAGENDXB_MASK) + 1);
        let num_s2_context_banks =
            ((id1 >> SMMU_ID1_NUMS2CB_SHIFT) & SMMU_ID1_NUMS2CB_MASK) as usize;
        let num_context_banks = (id1 & SMMU_ID1_NUMCB_MASK) as usize;
        if num_pages == 0 || num_context_banks == 0 {
            log_puts("usb: Apps SMMU has no usable context banks\n");
            return false;
        }
        // The GR0 window is page 0 and GR1 is page 1. Context-bank pages start
        // after the implementation-defined number of global pages.
        let gr1_page = 1usize;
        let cb_base_page = num_pages;
        log_hex("usb: Apps SMMU ID0=", id0 as u64);
        log_hex("usb: Apps SMMU ID1=", id1 as u64);
        log_hex("usb: Apps SMMU pages=", num_pages as u64);

        let mut matched = None;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if ((pool.stream_id ^ id) & !mask) == 0 {
                matched = Some((index, read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4))));
                break;
            }
        }
        let Some((smr_index, old_s2cr)) = matched else {
            log_puts("usb: DWC3 stream 0xe0 has no SMMU match\n");
            return false;
        };

        let old_type = old_s2cr & SMMU_S2CR_TYPE_MASK;
        let old_cb = (old_s2cr & SMMU_S2CR_CBNDX_MASK) as usize;
        // Fastboot commonly keeps the DWC3 stream in S2CR.BYPASS while its
        // USB buffers are identity-addressed. This is already a valid
        // physical=IOVA mapping for the linker DMA pool. Replacing it with a
        // freshly selected context bank during `fastboot boot` changes an
        // ownership boundary that Linux would preserve until its IOMMU
        // domain is fully attached, and can make the first EP0 TRB fault.
        if old_type == SMMU_S2CR_TYPE_BYPASS {
            trace_event(
                TRACE_SMMU_PRESERVED,
                smr_index as u32,
                old_cb as u32,
                old_s2cr,
                dma_start as u32,
                dma_end as u32,
            );
            log_puts("usb: preserving Fastboot Apps SMMU bypass\n");
            return true;
        }
        let cbndx = if old_type == SMMU_S2CR_TYPE_TRANS {
            if old_cb >= num_context_banks || old_cb < num_s2_context_banks {
                log_puts("usb: DWC3 SMMU context bank is out of range\n");
                return false;
            }
            old_cb
        } else {
            // This is the same reserved-last-context-bank strategy used by
            // Linux's qcom_smmu bypass-quirk path for firmware that refuses
            // BYPASS S2CR values.
            num_context_banks - 1
        };
        log_hex("usb: DWC3 SMMU SMR=", smr_index as u64);
        log_hex("usb: DWC3 SMMU CB=", cbndx as u64);
        // CBAR.IRPTNDX is an index into the dense context-bank IRQ list in
        // the provider DT, not a GIC SPI number.  Linux's irq-domain setup
        // programs the same association before enabling context faults.
        let context_irq_count =
            super::super::platform::bramble::usb_resources().smmu_context_irq_count;
        let irptndx = if context_irq_count != 0 {
            cbndx % context_irq_count
        } else {
            cbndx
        };
        SMMU_CONTEXT_PAGE = cb_base_page + cbndx;
        SMMU_CONTEXT_PAGE_SIZE = page_size;

        // Linux's SMMU driver owns this context, not the DWC3 glue. A
        // Fastboot handoff can therefore arrive with a valid stage-1 map
        // already installed for the same stream. Replacing that context
        // underneath firmware is unsafe: the controller may still have an
        // outstanding transaction using the old page tables, and changing
        // TTBR0 can turn a benign handoff into a stream fault. Preserve a
        // live translation context and use the declared pool check above as
        // the ownership boundary for Fullerene's DMA objects.
        if old_type == SMMU_S2CR_TYPE_TRANS {
            let cb_page = cb_base_page + cbndx;
            let sctlr = smmu_page_read(page_size, cb_page, SMMU_CB_SCTLR);
            let ttbr0 = smmu_page_read64(page_size, cb_page, SMMU_CB_TTBR0);
            let tcr = smmu_page_read(page_size, cb_page, SMMU_CB_TCR);
            if sctlr & SMMU_SCTLR_M != 0
                && smmu_context_maps_identity(ttbr0, tcr, dma_start, dma_end)
            {
                trace_event(
                    TRACE_SMMU_PRESERVED,
                    smr_index as u32,
                    cbndx as u32,
                    sctlr,
                    ttbr0 as u32,
                    (ttbr0 >> 32) as u32,
                );
                log_puts("usb: preserving active Apps SMMU translation\n");
                return true;
            }
            log_puts("usb: active Apps SMMU map does not cover Fullerene DMA\n");
        }

        if !install_smmu_identity_table(pool) {
            log_puts("usb: DT DMA pool is not 2 MiB aligned/32-bit addressable\n");
            return false;
        }

        // Stop the bank before changing its format and page-table pointer.
        smmu_page_write(page_size, cb_base_page + cbndx, SMMU_CB_SCTLR, 0);
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBA2R_BASE + cbndx * 4,
            SMMU_CBA2R_VA64,
        );
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBAR_BASE + cbndx * 4,
            SMMU_CBAR_S1_TRANS_S2_BYPASS
                | SMMU_CBAR_S1_MEMATTR_WB
                | SMMU_CBAR_S1_BPSHCFG_NSH
                | (irptndx as u32 & SMMU_CBAR_IRPTNDX_MASK),
        );

        // SCR0 interrupt enables are deliberately NOT written here. The
        // global/CFG fault lines are polled (service_smmu_fault) and the
        // secure-side SCR0 bits can reject non-secure writes on Qualcomm
        // firmware, which aborts the whole handoff before the pull-up.

        let cb_page = cb_base_page + cbndx;
        // 4 KiB granule, 32-bit IOVA, inner-shareable WBWA walks, and a
        // 40-bit output address size. TCR2 selects the AArch64 format.
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR2,
            SMMU_TCR2_SEP_UPSTREAM | SMMU_TCR2_AS | SMMU_TCR2_PASIZE_40BIT,
        );
        let t0sz = if super::super::platform::bramble::usb_resources().smmu_use_3_level_tables {
            SMMU_TCR_T0SZ_39BIT
        } else {
            SMMU_TCR_T0SZ_32BIT
        };
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR,
            SMMU_TCR_EPD1 | SMMU_TCR_SH0_INNER | SMMU_TCR_ORGN0_WBWA | SMMU_TCR_IRGN0_WBWA | t0sz,
        );
        smmu_page_write64(
            page_size,
            cb_page,
            SMMU_CB_TTBR0,
            addr_of!(SMMU_L1) as usize as u64,
        );
        smmu_page_write64(page_size, cb_page, SMMU_CB_TTBR1, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_CONTEXTIDR, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR0, 0xff);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR1, 0);
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_SCTLR,
            SMMU_SCTLR_S1_ASIDPNE
                | SMMU_SCTLR_CFIE
                | SMMU_SCTLR_CFRE
                | SMMU_SCTLR_AFE
                | SMMU_SCTLR_TRE
                | SMMU_SCTLR_M,
        );

        // S2CR type TRANS is zero; preserve privilege and EXID bits from the
        // firmware entry while replacing only the context-bank selector.
        let new_s2cr = (old_s2cr & !SMMU_S2CR_CBNDX_MASK) & !SMMU_S2CR_TYPE_MASK
            | ((cbndx as u32) & SMMU_S2CR_CBNDX_MASK)
            | SMMU_S2CR_TYPE_TRANS;
        let s2cr_address = SMMU_S2CR_BASE + smr_index * 4;
        write_volatile(smmu_reg(s2cr_address), new_s2cr);
        core::arch::asm!("dsb sy", options(nostack));
        let readback = read_volatile(smmu_reg(s2cr_address));
        if readback & SMMU_S2CR_TYPE_MASK != SMMU_S2CR_TYPE_TRANS
            || (readback & SMMU_S2CR_CBNDX_MASK) as usize != cbndx
        {
            log_puts("usb: DWC3 SMMU S2CR translation rejected\n");
            return false;
        }
        smmu_tlb_sync();
        trace_event(
            TRACE_SMMU_READY,
            smr_index as u32,
            cbndx as u32,
            id0,
            id1,
            0,
        );
        true
    }
}

/// Read-only Apps-SMMU state for the DWC3 stream. This never writes the
/// SMMU; if the aperture is clock-gated or secure-owned the access aborts and
/// the handset reboots before the pull-up is published, which is itself a
/// distinct host-visible outcome.
///   6 = SMR matched the stream and S2CR selects TRANSLATE
///   7 = SMR matched the stream and S2CR selects BYPASS
///   8 = SMMU readable but no SMR matches the DWC3 stream
///   9 = SMMU identification registers are unreadable
pub fn probe_smmu_stream_state() -> u32 {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return 9;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let stream_id = super::super::platform::bramble::usb_resources()
            .dma_pool
            .stream_id;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask == 0 {
                let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
                return if s2cr & SMMU_S2CR_TYPE_MASK == SMMU_S2CR_TYPE_BYPASS {
                    7
                } else {
                    6
                };
            }
        }
        8
    }
}
