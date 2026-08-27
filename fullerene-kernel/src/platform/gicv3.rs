use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

const TIMER_PPI: u32 = 30;

const GICD_CTLR: usize = 0x0000;
const GICD_IGROUPR: usize = 0x0080;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_ICFGR: usize = 0x0c00;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_IROUTER: usize = 0x6000;
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

unsafe fn enable_spi(gicd_base: usize, interrupt_id: u32) {
    unsafe {
        if !(32..1020).contains(&interrupt_id) {
            return;
        }

        let word = (interrupt_id / 32) as usize;
        let bit = 1u32 << (interrupt_id % 32);
        let group = gicd_base + GICD_IGROUPR + word * 4;
        let enable = gicd_base + GICD_ISENABLER + word * 4;
        let priority = gicd_base + GICD_IPRIORITYR + interrupt_id as usize;
        let config = gicd_base + GICD_ICFGR + (interrupt_id / 16) as usize * 4;
        let router = gicd_base + GICD_IROUTER + interrupt_id as usize * 8;

        // The Android DT describes the DWC3 interrupt as Group 1, level-high,
        // routed to the boot CPU (SPI 240 on Lito). Clear the trigger bit pair
        // to retain the GICv3 level-sensitive encoding.
        write32(group, read32(group) | bit);
        let trigger_shift = (interrupt_id % 16) * 2 + 1;
        write32(config, read32(config) & !(1u32 << trigger_shift));
        write8(priority, 0xa0);
        write64(router, 0);
        write32(enable, bit);
    }
}

/// Enable additional parent SPIs after platform-specific interrupt
/// controllers (such as Qualcomm PDC) have translated their child lines.
/// This keeps the generic GIC setup reusable while preserving the distinction
/// between a PDC pin and its GIC parent interrupt.
pub unsafe fn enable_spis(gicd_base: usize, interrupts: &[u32]) {
    for &interrupt_id in interrupts {
        unsafe { enable_spi(gicd_base, interrupt_id) };
    }
}

/// Mask level-sensitive platform SPIs while their threaded/deferred handler
/// clears the originating child interrupt. This is the top-half boundary used
/// by the Qualcomm Type-C parent path.
pub unsafe fn disable_spis(gicd_base: usize, interrupts: &[u32]) {
    for &interrupt_id in interrupts {
        if !(32..1020).contains(&interrupt_id) {
            continue;
        }
        let word = (interrupt_id / 32) as usize;
        let bit = 1u32 << (interrupt_id % 32);
        unsafe { write32(gicd_base + GICD_ICENABLER + word * 4, bit) };
    }
}

/// Apply the part of a Qualcomm USB PM QoS policy that this early kernel can
/// enforce in hardware: keep the USB SPI on the boot processing element and
/// raise its GIC priority while the latency request is active. CPU scheduler
/// latency accounting is not available during the early handoff, so the
/// platform must not pretend that a GIC write is a Linux PM QoS request.
pub unsafe fn set_spi_usb_latency_policy(
    gicd_base: usize,
    interrupt_id: u32,
    active: bool,
) -> bool {
    if !(32..1020).contains(&interrupt_id) {
        return false;
    }
    unsafe {
        let priority = gicd_base + GICD_IPRIORITYR + interrupt_id as usize;
        let router = gicd_base + GICD_IROUTER + interrupt_id as usize * 8;
        // 0x80 is the active USB priority used by this early path; 0xa0 is
        // the normal platform SPI priority restored when USB is suspended.
        write8(priority, if active { 0x80 } else { 0xa0 });
        // Affinity remains the boot CPU (Affinity 0) in both states. Keeping
        // the route stable prevents a wakeup from being lost during the
        // suspend transition; the priority is the actual QoS gate here.
        write64(router, 0);
        true
    }
}

unsafe fn write64(address: usize, value: u64) {
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
}

/// Bring up the local GICv3 redistributor, the EL1 physical timer PPI, and
/// optionally one platform SPI. Both QEMU virt and SM7250 expose an ARM
/// GICv3; only the MMIO bases and the platform SPI differ.
#[must_use]
pub fn init(gicd_base: usize, gicr_base: usize, usb_irq: Option<u32>) -> bool {
    let sgi_base = gicr_base + GICR_SGI_BASE;
    unsafe {
        let waker = read32(gicr_base + GICR_WAKER);
        write32(gicr_base + GICR_WAKER, waker & !(1 << 1));
        let mut awake = false;
        for _ in 0..100_000 {
            if read32(gicr_base + GICR_WAKER) & (1 << 2) == 0 {
                awake = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !awake {
            // A redistributor still owned by firmware is not safe to program.
            // Leave the caller's polling path alive, but do not spin forever.
            return false;
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
        if let Some(interrupt_id) = usb_irq {
            enable_spi(gicd_base, interrupt_id);
        }
        asm!("msr ICC_SRE_EL1, {value}", value = in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));
        asm!("msr ICC_PMR_EL1, {value}", value = in(reg) 0xffu64, options(nostack));
        asm!("msr ICC_IGRPEN1_EL1, {value}", value = in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));
    }
    true
}
