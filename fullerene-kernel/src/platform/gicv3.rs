use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

const TIMER_PPI: u32 = 30;

const GICD_CTLR: usize = 0x0000;
const GICR_WAKER: usize = 0x0014;
const GICR_SGI_BASE: usize = 0x1_0000;
const GICR_IGROUPR0: usize = 0x80;
const GICR_ISENABLER0: usize = 0x100;
const GICR_IPRIORITYR0: usize = 0x400;

unsafe fn read32(address: usize) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}

unsafe fn write32(address: usize, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) };
}

unsafe fn write8(address: usize, value: u8) {
    unsafe { write_volatile(address as *mut u8, value) };
}

/// Bring up the local GICv3 redistributor and the EL1 physical timer PPI.
/// Both QEMU virt and SM7250 expose an ARM GICv3; only the MMIO bases differ.
pub fn init(gicd_base: usize, gicr_base: usize) {
    let sgi_base = gicr_base + GICR_SGI_BASE;
    unsafe {
        let waker = read32(gicr_base + GICR_WAKER);
        write32(gicr_base + GICR_WAKER, waker & !(1 << 1));
        while read32(gicr_base + GICR_WAKER) & (1 << 2) != 0 {
            core::hint::spin_loop();
        }

        write32(
            sgi_base + GICR_IGROUPR0,
            read32(sgi_base + GICR_IGROUPR0) | (1 << TIMER_PPI),
        );
        write8(sgi_base + GICR_IPRIORITYR0 + TIMER_PPI as usize, 0xa0);
        write32(sgi_base + GICR_ISENABLER0, 1 << TIMER_PPI);

        write32(
            gicd_base + GICD_CTLR,
            read32(gicd_base + GICD_CTLR) | (1 << 1),
        );
        asm!("msr ICC_SRE_EL1, {value}", value = in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));
        asm!("msr ICC_PMR_EL1, {value}", value = in(reg) 0xffu64, options(nostack));
        asm!("msr ICC_IGRPEN1_EL1, {value}", value = in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));
    }
}
