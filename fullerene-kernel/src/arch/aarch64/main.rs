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
         adrp x9, AARCH64_BOOT_STACK\n\
         add x9, x9, :lo12:AARCH64_BOOT_STACK\n\
         mov x10, #{stack_size}\n\
         add sp, x9, x10\n\
         adrp x11, __bss_start\n\
         add x11, x11, :lo12:__bss_start\n\
         adrp x12, __bss_end\n\
         add x12, x12, :lo12:__bss_end\n\
     1:\n\
         cmp x11, x12\n\
         b.hs 2f\n\
         str xzr, [x11], #8\n\
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
         // Android bootloaders may place an Image at a different physical\n\
         // base. Apply the PIE's relative relocations before entering Rust;\n\
         // x0 remains the bootloader-provided DTB address.\n\
         adr x7, _start\n\
         sub sp, sp, #32\n\
         stp x0, x1, [sp]\n\
         stp x2, x3, [sp, #16]\n\
         mov x0, x7\n\
         bl aarch64_apply_relocations\n\
         ldp x2, x3, [sp, #16]\n\
         ldp x0, x1, [sp]\n\
         add sp, sp, #32\n\
         b aarch64_rust_entry\n\
     .size aarch64_el1_entry, . - aarch64_el1_entry\n\
     ",
    stack_size = const BOOT_STACK_SIZE,
);

#[cfg(fullerene_aarch64_bramble)]
const LINK_ENTRY: usize = 0x8008_0040;
#[cfg(not(fullerene_aarch64_bramble))]
const LINK_ENTRY: usize = 0x4200_0040;

// This runs before the PIE's GOT is valid. Keep it as position-independent
// assembly embedded in this Rust source: a Rust implementation can itself
// acquire a GOT relocation before it has had a chance to apply the records.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global aarch64_apply_relocations\n\
     .type aarch64_apply_relocations, %function\n\
     aarch64_apply_relocations:\n\
         movz x11, #{entry_0}\n\
         movk x11, #{entry_1}, lsl #16\n\
         movk x11, #{entry_2}, lsl #32\n\
         movk x11, #{entry_3}, lsl #48\n\
         sub x10, x0, x11\n\
         adr x8, __rela_dyn_start\n\
         adr x9, __rela_dyn_end\n\
     1:\n\
         cmp x8, x9\n\
         b.hs 2f\n\
         ldr x11, [x8]\n\
         ldr w12, [x8, #8]\n\
         cmp w12, #0x403\n\
         b.eq 3f\n\
         cmp w12, #0x101\n\
         b.ne 4f\n\
     3:\n\
         ldr x13, [x8, #16]\n\
         add x14, x11, x10\n\
         add x13, x13, x10\n\
         str x13, [x14]\n\
     4:\n\
         add x8, x8, #24\n\
         b 1b\n\
     2:\n\
         ret\n\
     .size aarch64_apply_relocations, . - aarch64_apply_relocations\n\
     ",
    entry_0 = const (LINK_ENTRY & 0xffff),
    entry_1 = const ((LINK_ENTRY >> 16) & 0xffff),
    entry_2 = const ((LINK_ENTRY >> 32) & 0xffff),
    entry_3 = const ((LINK_ENTRY >> 48) & 0xffff),
);

#[unsafe(no_mangle)]
extern "C" fn aarch64_rust_entry(fdt_address: u64, arg1: u64, fdt_arg2: u64, arg3: u64) -> ! {
    // Establish a compiled-in console before looking at any bootloader
    // pointer. A vendor trampoline can hand us an invalid or absent DTB;
    // touching that address before VBAR and UART are ready turns a useful
    // handoff failure into an invisible synchronous abort.
    let compiled_bramble = cfg!(fullerene_aarch64_bramble);
    let early_uart_base = if compiled_bramble {
        platform::bramble::UART_BASE
    } else {
        platform::qemu_virt::UART_BASE
    };
    if compiled_bramble {
        uart::init_qcom_geni(early_uart_base as u64);
    } else {
        uart::init_at(early_uart_base as u64);
    }
    exceptions::install();
    uart::puts("fullerene: entered Rust before DTB discovery\n");
    uart::put_hex("boot: x0=", fdt_address);
    uart::put_hex("boot: x1=", arg1);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex("boot: x3=", arg3);

    // The architectural arm64 boot contract puts the physical DTB address in
    // x0 and requires x1..x3 to be zero.  A vendor fastboot path is allowed
    // to use a different trampoline, however, so accept x2 as a guarded
    // fallback for bring-up.  Never prefer x2 over a valid x0: otherwise a
    // normal Android handoff can be mistaken for a missing DTB.
    let dtb_address = if compiled_bramble {
        // The Pixel fastboot trampoline is not required to expose the
        // vendor_boot DTB to an arbitrary replacement kernel.  All early
        // Bramble addresses below are compiled from the vendor DT and do not
        // need a speculative read through x0/x2.  Once the fixed platform
        // path is stable, the handoff DTB can be adopted as an optional
        // source of overlays rather than a prerequisite for entering Rust.
        None
    } else {
        [fdt_address, fdt_arg2]
            .into_iter()
            .filter(|address| *address != 0 && *address % 8 == 0)
            .find(|address| fdt::inspect(*address).is_some())
            .or(Some(platform::qemu_virt::DTB_BASE))
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
    uart::put_hex("boot: x1=", arg1);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex("boot: x3=", arg3);
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
    if let Some(typec) = unsafe { platform::bramble::prepare_usb_device_role() } {
        uart::put_hex("platform: PMIC arbiter=", typec.arbiter_version as u64);
        uart::put_hex("platform: Type-C status=", typec.misc_status as u64);
        uart::put_hex("platform: Type-C mode=", typec.mode as u64);
        uart::put_hex(
            "platform: Type-C orientation=",
            typec.orientation_reverse as u64,
        );
        if typec.sink_mode_written {
            uart::puts("platform: Type-C sink-only selected\n");
        }
    } else {
        uart::puts("platform: Type-C SPMI state unavailable\n");
    }
    #[cfg(fullerene_aarch64_bramble)]
    usb::clear_dma_memory();
    #[cfg(fullerene_aarch64_bramble)]
    if usb::init_usb2_handoff() {
        uart::puts("platform: bramble USB2 gadget handoff: ready\n");
    } else {
        uart::puts("platform: bramble USB2 gadget handoff: failed\n");
        // `fastboot boot` may jump through a vendor trampoline that tears
        // down the Fastboot controller before entering the image.  In that
        // case preserving the bootloader's PHY state cannot work; retry with
        // the complete Qualcomm USB2 platform sequence.
        if usb::init_usb2_only() {
            uart::puts("platform: bramble USB2 cold fallback: ready\n");
        } else {
            uart::puts("platform: bramble USB2 cold fallback: failed\n");
        }
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
