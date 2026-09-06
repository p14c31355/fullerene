use core::arch::{asm, global_asm};

const BOOT_STACK_SIZE: usize = 64 * 1024;

/// Values captured from the bootloader before the normal Rust entry starts.
/// The bootstrap shim only establishes a stack and branches to
/// `aarch64_bootstrap`; the rest of this typed handoff is assembled by Rust.
#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct Aarch64BootContext {
    pub(crate) x0: usize,
    pub(crate) x1: usize,
    pub(crate) x2: usize,
    pub(crate) x3: usize,
    pub(crate) current_el: usize,
    pub(crate) relocation_delta: isize,
}

impl Aarch64BootContext {
    /// Copy the bootstrap frame before Rust starts using that stack for locals.
    pub(crate) fn read(pointer: *const Self) -> Self {
        unsafe { core::ptr::read(pointer) }
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".boot_stack")]
static mut AARCH64_BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

// This is the only assembly needed to establish a valid stack. Rust owns
// zeroing BSS, relocation processing, EL2->EL1 setup, and context creation.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
         adr x9, __aarch64_boot_stack_top\n\
         mov sp, x9\n\
         b aarch64_bootstrap\n\
     .size _start, . - _start\n\
     ",
);

/// Finish the architecture-neutral part of the boot handoff in Rust.
///
/// The entry shim deliberately preserves x0..x3 and does not touch BSS before
/// this function. Relocation addresses are obtained PC-relatively first, then
/// BSS is cleared after the static-PIE fixups, so the bootstrap stack can live
/// outside the cleared range.
#[unsafe(no_mangle)]
extern "C" fn aarch64_bootstrap(x0: usize, x1: usize, x2: usize, x3: usize) -> ! {
    let (current_el, runtime_entry) = read_boot_state();
    let relocation_delta = aarch64_apply_relocations(runtime_entry);
    zero_bss();

    // QEMU's virt machine can hand the kernel directly to EL1. Keep that
    // path and the EL2 transition in one assembly boundary: both must leave
    // CPACR_EL1 ready before Rust performs ordinary copies.
    unsafe { configure_el1(current_el & 0xc == 0x8) };

    let context = Aarch64BootContext {
        x0,
        x1,
        x2,
        x3,
        current_el,
        relocation_delta,
    };
    super::aarch64_rust_entry(&context)
}

#[inline(always)]
fn read_boot_state() -> (usize, usize) {
    let current_el: usize;
    let runtime_entry: usize;
    unsafe {
        asm!(
            "mrs {current_el}, CurrentEL",
            "adr {runtime_entry}, _start",
            current_el = out(reg) current_el,
            runtime_entry = out(reg) runtime_entry,
            options(nomem, nostack, preserves_flags),
        );
    }
    (current_el, runtime_entry)
}

/// Transition from EL2 to EL1, or enable the EL1 FP/SIMD trap path when the
/// bootloader already entered at EL1. One block owns both cases so the
/// architecture entry does not grow duplicate inline-assembly boundaries.
unsafe fn configure_el1(from_el2: bool) {
    unsafe {
        asm!(
            "cbz {from_el2}, 1f",
            "mov x5, #(1 << 31)",
            "msr HCR_EL2, x5",
            "msr CPTR_EL2, xzr",
            "mov x5, #9",
            "msr ICC_SRE_EL2, x5",
            "isb",
            "mov x5, #3",
            "msr CNTHCTL_EL2, x5",
            "msr CNTVOFF_EL2, xzr",
            "mov x5, #0x3c5",
            "msr SPSR_EL2, x5",
            "adr x5, 2f",
            "msr ELR_EL2, x5",
            "mov x6, sp",
            "msr SP_EL1, x6",
            "mov x5, #(3 << 20)",
            "msr CPACR_EL1, x5",
            "isb",
            "eret",
            "1:",
            "mov x5, #(3 << 20)",
            "msr CPACR_EL1, x5",
            "isb",
            "2:",
            from_el2 = in(reg) from_el2 as usize,
            out("x5") _,
            out("x6") _,
            options(nostack),
        );
    }
}

unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
}

fn zero_bss() {
    let start = core::ptr::addr_of!(__bss_start) as usize;
    let end = core::ptr::addr_of!(__bss_end) as usize;
    if end > start {
        unsafe { core::ptr::write_bytes(start as *mut u8, 0, end - start) };
    }
}

#[cfg(fullerene_aarch64_bramble)]
const LINK_ENTRY: usize = 0x8008_0040;
#[cfg(not(fullerene_aarch64_bramble))]
const LINK_ENTRY: usize = 0x4200_0040;

/// Apply the small relocation set emitted by the static-PIE linker.
#[unsafe(no_mangle)]
extern "C" fn aarch64_apply_relocations(runtime_entry: usize) -> isize {
    let relocation_delta = runtime_entry.wrapping_sub(LINK_ENTRY) as isize;
    let (mut cursor, end): (usize, usize);
    unsafe {
        asm!(
            "adr {cursor}, __rela_dyn_start",
            "adr {end}, __rela_dyn_end",
            cursor = out(reg) cursor,
            end = out(reg) end,
            options(nomem, nostack, preserves_flags),
        );
    }

    while cursor < end {
        let offset = unsafe { core::ptr::read_unaligned(cursor as *const usize) };
        let relocation_type = unsafe { core::ptr::read_unaligned((cursor + 8) as *const u32) };
        if relocation_type == 0x403 || relocation_type == 0x101 {
            let addend = unsafe { core::ptr::read_unaligned((cursor + 16) as *const usize) };
            let target = offset.wrapping_add(relocation_delta as usize) as *mut usize;
            unsafe {
                target.write(addend.wrapping_add(relocation_delta as usize));
            }
        }
        cursor += 24;
    }

    relocation_delta
}
