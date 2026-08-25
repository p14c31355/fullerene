#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;

mod allocator;
mod exceptions;
mod fdt;
mod mmu;
#[path = "../../platform/mod.rs"]
mod platform;
mod timer;
mod uart;
#[cfg(fullerene_aarch64_bramble)]
mod usb;

const BOOT_STACK_SIZE: usize = 64 * 1024;

#[unsafe(no_mangle)]
static mut AARCH64_BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

// QEMU -kernel enters at _start without promising a usable SP. Establish a
// known, aligned stack before calling any Rust code. This is also the shape
// that the eventual Bramble entry path will replace with its boot contract.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
         adrp x1, AARCH64_BOOT_STACK\n\
         add x1, x1, :lo12:AARCH64_BOOT_STACK\n\
         mov x2, #{stack_size}\n\
         add sp, x1, x2\n\
         adrp x3, __bss_start\n\
         add x3, x3, :lo12:__bss_start\n\
         adrp x4, __bss_end\n\
         add x4, x4, :lo12:__bss_end\n\
     1:\n\
         cmp x3, x4\n\
         b.hs 2f\n\
         str xzr, [x3], #8\n\
         b 1b\n\
     2:\n\
         // QEMU may enter at EL1; Android-style AArch64 bootloaders may hand\n\
         // off at EL2. Normalize the latter to EL1h while preserving x0\n\
         // (the DTB address) and the bootstrap stack.\n\
         mrs x5, CurrentEL\n\
         and x5, x5, #0xc\n\
         cmp x5, #0x8\n\
         b.eq 3f\n\
         b aarch64_el1_entry\n\
     3:\n\
         mov x5, #(1 << 31)\n\
         msr HCR_EL2, x5\n\
         msr CPTR_EL2, xzr\n\
         mov x5, #9\n\
         msr ICC_SRE_EL2, x5\n\
         isb\n\
         mov x5, #3\n\
         msr CNTHCTL_EL2, x5\n\
         msr CNTVOFF_EL2, xzr\n\
         mov x5, #0x3c5\n\
         msr SPSR_EL2, x5\n\
         adrp x5, aarch64_el1_entry\n\
         add x5, x5, :lo12:aarch64_el1_entry\n\
         msr ELR_EL2, x5\n\
         mov x6, sp\n\
         msr SP_EL1, x6\n\
         isb\n\
         eret\n\
     .size _start, . - _start\n\
     // Execute the FP/SIMD enable at EL1. Some firmware/QEMU reset paths\n\
     // ignore an EL2 write to CPACR_EL1 until the lower exception level is\n\
     // active.\n\
     .global aarch64_el1_entry\n\
     .type aarch64_el1_entry, %function\n\
     aarch64_el1_entry:\n\
         mov x5, #(3 << 20)\n\
         msr CPACR_EL1, x5\n\
         isb\n\
         b aarch64_rust_entry\n\
     .size aarch64_el1_entry, . - aarch64_el1_entry\n\
     ",
    stack_size = const BOOT_STACK_SIZE,
);

#[unsafe(no_mangle)]
extern "C" fn aarch64_rust_entry(fdt_address: u64, _arg1: u64, fdt_arg2: u64) -> ! {
    // QEMU's direct kernel loader leaves the DTB at the virt machine's
    // conventional address, while Android/Linux-style AArch64 handoff puts
    // it in x0. Prefer an actually valid FDT over either convention. A
    // Bramble bootloader is expected to pass a DTB from vendor_boot, but the
    // hard-coded platform backend keeps the first UART diagnostic alive if a
    // development boot path omits it.
    let dtb_address = if cfg!(fullerene_aarch64_bramble) {
        // The Android arm64 boot contract passes the vendor_boot DTB in x0.
        // Do not probe x2 on Bramble: it is not a DTB argument there and may
        // point at unrelated physical memory.
        fdt::inspect(fdt_address).map(|_| fdt_address)
    } else {
        [fdt_address, fdt_arg2]
            .into_iter()
            .find(|address| fdt::inspect(*address).is_some())
            .or_else(|| Some(platform::qemu_virt::DTB_BASE))
    };
    let qcom_uart = dtb_address.and_then(|address| {
        fdt::find_compatible(address, b"qcom,geni-debug-uart")
            .or_else(|| fdt::find_compatible(address, b"qcom,geni-uart"))
    });
    let pl011_uart = dtb_address.and_then(|address| fdt::find_compatible(address, b"arm,pl011"));
    let gicd_region = dtb_address.and_then(|address| fdt::find_compatible(address, b"arm,gic-v3"));
    let gicr_region =
        dtb_address.and_then(|address| fdt::find_compatible_nth(address, b"arm,gic-v3", 1));
    let bramble = cfg!(fullerene_aarch64_bramble) || qcom_uart.is_some();
    let gicd_base = gicd_region.map(|region| region.base as usize);
    let gicr_base = gicr_region.map(|region| region.base as usize);
    let uart_region = if bramble { qcom_uart } else { pl011_uart };
    let uart_base = uart_region
        .map(|region| region.base as usize)
        .unwrap_or(if bramble {
            platform::bramble::UART_BASE
        } else {
            platform::qemu_virt::UART_BASE
        });
    if bramble {
        uart::init_qcom_geni(uart_base as u64);
    } else {
        uart::init_at(uart_base as u64);
    }
    uart::puts("hello from fullerene aarch64\n");
    if bramble {
        uart::puts("platform: bramble, uart: qcom-geni\n");
    } else {
        uart::puts("platform: qemu-virt, uart: pl011\n");
    }
    uart::put_hex("uart: base=", uart_base as u64);
    if let Some(region) = uart_region {
        if region.size != 0 {
            uart::put_hex("uart: size=", region.size);
        }
    }
    uart::put_hex("boot: x0=", fdt_address);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex(
        "gicd: base=",
        gicd_base.unwrap_or(if bramble {
            platform::bramble::GICD_BASE
        } else {
            platform::qemu_virt::GICD_BASE
        }) as u64,
    );
    uart::put_hex(
        "gicr: base=",
        gicr_base.unwrap_or(if bramble {
            platform::bramble::GICR_BASE
        } else {
            platform::qemu_virt::GICR_BASE
        }) as u64,
    );

    if let Some(address) = dtb_address {
        if let Some(header) = fdt::inspect(address) {
            uart::put_hex("dtb: address=", header.address);
            uart::put_hex("dtb: size=", header.total_size as u64);
            uart::put_hex("dtb: struct_offset=", header.structure_offset as u64);
            uart::put_hex("dtb: strings_offset=", header.strings_offset as u64);
            uart::put_hex("dtb: version=", header.version as u64);
        } else {
            uart::puts("dtb: unavailable or invalid\n");
        }
    } else {
        uart::puts("dtb: not supplied; using compiled platform defaults\n");
    }

    exceptions::install();
    uart::puts("arch: aarch64, exception vectors: ready\n");
    uart::put_hex("currentel: ", exceptions::current_el() as u64);

    mmu::init();
    uart::puts("mmu: identity map and caches ready\n");

    allocator::smoke();
    uart::puts("allocator: bump heap ready\n");

    timer::init();
    let before = timer::counter();
    timer::delay_ms(10);
    let elapsed = timer::counter().wrapping_sub(before);
    uart::puts("timer: generic counter ready, ticks=");
    uart::put_hex_value(elapsed);

    // Bring up the USB handoff before touching the GIC redistributor.  On a
    // phone boot path the redistributor may still be owned by firmware; USB
    // is polled during this early diagnostic phase and does not depend on it.
    #[cfg(fullerene_aarch64_bramble)]
    if usb::init() {
        uart::puts("platform: bramble USB gadget: ready\n");
    } else {
        uart::puts("platform: bramble USB gadget: failed\n");
    }

    if bramble {
        platform::bramble::init_interrupt_controller(gicd_base, gicr_base);
    } else {
        platform::qemu_virt::init_interrupt_controller(gicd_base, gicr_base);
    }
    timer::arm_ms(100);
    exceptions::enable_irqs();
    uart::puts("aarch64 early boot complete; waiting for timer irq\n");
    loop {
        #[cfg(fullerene_aarch64_bramble)]
        usb::poll();
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    uart::puts("fullerene aarch64 panic\n");
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
