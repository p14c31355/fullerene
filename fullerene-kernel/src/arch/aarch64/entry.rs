use core::arch::{asm, global_asm};

const BOOT_STACK_SIZE: usize = 64 * 1024;

/// Values captured from the bootloader before the bootstrap starts using
/// caller-saved registers. The assembly entry owns this layout until Rust
/// receives a pointer to it.
#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct Aarch64BootContext {
    pub(crate) x0: usize,
    pub(crate) x1: usize,
    pub(crate) x2: usize,
    pub(crate) x3: usize,
    pub(crate) current_el: usize,
    pub(crate) entry_sp: usize,
    pub(crate) relocation_delta: isize,
}

impl Aarch64BootContext {
    /// Copy the bootstrap frame before Rust starts using that stack for locals.
    pub(crate) fn read(pointer: *const Self) -> Self {
        unsafe { core::ptr::read(pointer) }
    }
}

const BOOT_CONTEXT_SIZE: usize = (core::mem::size_of::<Aarch64BootContext>() + 15) & !15;

#[unsafe(no_mangle)]
static mut AARCH64_BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

// Keep the unavoidably low-level boot contract in one module. The Rust entry
// point in main.rs receives only the typed Aarch64BootContext.
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
         mov x6, sp\n\
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
         sub sp, sp, #{context_size}\n\
         mov x19, sp\n\
         stp x0, x1, [x19]\n\
         stp x2, x3, [x19, #16]\n\
         mrs x5, CurrentEL\n\
         str x5, [x19, #32]\n\
         str x6, [x19, #40]\n\
         str xzr, [x19, #48]\n\
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
     .global aarch64_el1_entry\n\
     .type aarch64_el1_entry, %function\n\
     aarch64_el1_entry:\n\
         mov x5, #(3 << 20)\n\
         msr CPACR_EL1, x5\n\
         isb\n\
         adr x7, _start\n\
         mov x0, x7\n\
         bl aarch64_apply_relocations\n\
         str x0, [x19, #48]\n\
         mov x0, x19\n\
         b aarch64_rust_entry\n\
     .size aarch64_el1_entry, . - aarch64_el1_entry\n\
     ",
    stack_size = const BOOT_STACK_SIZE,
    context_size = const BOOT_CONTEXT_SIZE,
);

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
