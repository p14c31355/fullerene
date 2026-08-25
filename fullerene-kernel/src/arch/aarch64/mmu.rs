use core::arch::asm;

const TABLE_ENTRIES: usize = 512;
const BLOCK_SIZE: u64 = 0x20_0000;

const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1;
const DESC_ATTR_DEVICE: u64 = 1 << 2;
const DESC_AF: u64 = 1 << 10;
const DESC_SH_INNER: u64 = 0b11 << 8;
const DESC_PXN: u64 = 1 << 53;
const DESC_UXN: u64 = 1 << 54;

#[derive(Clone, Copy)]
#[repr(C, align(4096))]
struct PageTable([u64; TABLE_ENTRIES]);

static mut L1: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_0: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_1: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_2: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut L2_3: PageTable = PageTable([0; TABLE_ENTRIES]);

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
        asm!("msr SCTLR_EL1, {sctlr}", sctlr = in(reg) sctlr, options(nostack));
        asm!("ic iallu", "dsb sy", "isb", options(nostack));
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

fn is_mmio(physical: u64) -> bool {
    matches!(
        physical,
            // QEMU virt GIC and PL011.
            0x0800_0000..=0x09ff_ffff
            // SM7250 GCC clock/reset controller at 0x00100000.
            | 0x0010_0000..=0x001f_ffff
            // SM7250 QUP/GENI UART2 at 0x00988000.
            | 0x0080_0000..=0x009f_ffff
            // SM7250 GICD/GICR region.
            | 0x17a0_0000..=0x17bf_ffff
            // SM7250 DWC3 USB controller at 0x0a600000.
            | 0x0a60_0000..=0x0a6f_ffff
            // SM7250 Apps SMMU global register space at 0x15000000.
            | 0x1500_0000..=0x150f_ffff
    )
}
