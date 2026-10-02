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

// This is the only assembly needed to establish a valid stack, plus one guard:
// some loaders (measured on the ANE-LX2J's LK) may hand over with the MMU still
// enabled, and their page tables do not cover the regions a kernel is entitled
// to touch at handoff - the first store then faults before any output exists.
// Clearing SCTLR.M for the current EL before anything else switches to physical
// addressing, which is where the image was loaded. It is a no-op when the MMU
// is already off, and only x9/x10 are touched so the boot arguments in x0..x3
// stay intact. Rust owns zeroing BSS, relocation processing, EL2->EL1 setup,
// and context creation.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
     mrs x9, CurrentEL\n\
     cmp x9, #0x4\n\
     b.eq 8f\n\
     cmp x9, #0x8\n\
     b.eq 9f\n\
     b 7f\n\
     8: mrs x10, sctlr_el1\n\
     tbnz x10, #0, 10f\n\
     b 7f\n\
     9: mrs x10, sctlr_el2\n\
     tbnz x10, #0, 11f\n\
     b 7f\n\
     10: bic x10, x10, #1\n\
     msr sctlr_el1, x10\n\
     isb\n\
     b 7f\n\
     11: bic x10, x10, #1\n\
     msr sctlr_el2, x10\n\
     isb\n\
     7: adr x9, __aarch64_boot_stack_top\n\
         mov sp, x9\n\
         b aarch64_bootstrap\n\
     .size _start, . - _start\n\
     ",
);

/// The ANE-LX2J step-mark block, exactly as it sits in RAM.
///
/// The layout is a fixed structure rather than assembled on the fly: the
/// writer here and the paint probe (a bare-metal boot that reads the block
/// back on a later boot, see ane_paint_probe.S) are two independent readers
/// of the same bytes, so both are written against one definition - `magic`
/// at +0, `count` at +4, marks at +8. Every field is naturally aligned for
/// its width (the base is 16-byte aligned; a misaligned wide volatile read
/// killed five early boot runs).
#[cfg(fullerene_aarch64_ane)]
#[repr(C)]
pub(crate) struct StepMarks {
    /// "ANEM" little-endian (0x4D45_4E41). Written last by
    /// `ane_step_begin`, so a boot that dies in this very function leaves
    /// either the previous record or an empty one - both readable states.
    pub magic: u32,
    /// The number of committed marks below.
    pub count: u32,
    /// The marks themselves, in boot order.
    pub bytes: [u8; STEP_MARKS_CAPACITY],
}

#[cfg(fullerene_aarch64_ane)]
impl StepMarks {
    /// The scratch sits in the unused tail of the ramoops console zone
    /// (ramoops base 0x34800000 + console offset 0x60000 + 0x7F000), from
    /// the device's own tree. Two properties decide that address. First, no
    /// pstore reader interprets this area: the black box's console mirror
    /// writes at most a kilobyte at the zone's start, and the pmsg text log
    /// lives in the next zone along - so raw marks here corrupt nothing,
    /// where marks appended to the pmsg log used to interleave with its
    /// text. Second, the paint probe (a bare-metal boot, no ramoops driver)
    /// reads it back before Android can, which is the readout route: an
    /// earlier scheme wrote the pmsg zone directly and Android consumed it
    /// at boot before anything could read it.
    pub const ADDRESS: usize = 0x348d_f000;
    pub const MAGIC: u32 = 0x4D45_4E41; // "ANEM" little-endian
}

/// Marshal both readers' assumptions about the block layout into the build:
/// the paint probe's immediate offsets expect exactly 8 header bytes.
#[cfg(fullerene_aarch64_ane)]
const _: () = assert!(core::mem::size_of::<StepMarks>() == 8 + STEP_MARKS_CAPACITY);

/// How many marks the block holds; past it `ane_step_byte` stops writing so
/// a runaway loop cannot chew through the zone.
#[cfg(fullerene_aarch64_ane)]
const STEP_MARKS_CAPACITY: usize = 16;

/// Reset the ANE-LX2J's step-mark scratch to valid-and-empty, once per boot.
///
/// Called as the first statement of the bootstrap. The scratch lives in RAM
/// that survives a reset (that is the point), so without this each boot would
/// append to the previous boot's marks and no reader could tell the two runs
/// apart.
#[inline(always)]
pub(crate) unsafe fn ane_step_begin() {
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        let marks = StepMarks::ADDRESS as *mut StepMarks;
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*marks).count), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*marks).magic), StepMarks::MAGIC);
    }
}

/// One durable byte into the ANE-LX2J's step-mark block, written before
/// relocation processing can be assumed.
///
/// This runs before `aarch64_apply_relocations` has fixed up the static-PIE
/// image, so it deliberately touches no static memory - an absolute reference
/// to a static would be wrong at this point. Everything here is the raw
/// block address, which is also why it cannot use `blackbox`'s log helpers:
/// those live behind statics that do not exist yet.
///
/// The commit order is the whole synchronisation: the mark byte lands first,
/// the count second, so a reader that arrives mid-write sees either the
/// previous consistent state or this one.
#[inline(always)]
pub(crate) unsafe fn ane_step_byte(byte: u8) {
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        let marks = StepMarks::ADDRESS as *mut StepMarks;
        let count = core::ptr::read_volatile(core::ptr::addr_of!((*marks).count));
        if count >= STEP_MARKS_CAPACITY as u32 {
            return; // full or nonsense: leave the block alone
        }
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*marks).bytes[count as usize]),
            byte,
        );
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*marks).count), count + 1);
    }
    #[cfg(not(fullerene_aarch64_ane))]
    {
        let _ = byte;
    }
}

/// Finish the architecture-neutral part of the boot handoff in Rust.
///
/// The entry shim deliberately preserves x0..x3 and does not touch BSS before
/// this function. Relocation addresses are obtained PC-relatively first, then
/// BSS is cleared after the static-PIE fixups, so the bootstrap stack can live
/// outside the cleared range.
#[unsafe(no_mangle)]
extern "C" fn aarch64_bootstrap(x0: usize, x1: usize, x2: usize, x3: usize) -> ! {
    // Step marks for the ANE-LX2J port. The LK hands over with the MMU on (the
    // entry shim clears it) and the image may be loaded anywhere, so the first
    // question after a silent death is always "how far did it get". Each mark
    // is one byte in the step-mark scratch (see `ane_step_begin`), read back
    // by the paint probe on a later boot: 1 = bootstrap reached, 2 = boot
    // state read, 3 = relocations applied, 4 = BSS cleared, 5 = EL1
    // configured, and the Rust entry (main.rs) continues at 6 through c.
    unsafe { ane_step_begin() };
    unsafe { ane_step_byte(b'1') };
    let (current_el, runtime_entry) = read_boot_state();
    unsafe { ane_step_byte(b'2') };
    let relocation_delta = aarch64_apply_relocations(runtime_entry);
    unsafe { ane_step_byte(b'3') };
    zero_bss();
    unsafe { ane_step_byte(b'4') };

    // QEMU's virt machine can hand the kernel directly to EL1. Keep that
    // path and the EL2 transition in one assembly boundary: both must leave
    // CPACR_EL1 ready before Rust performs ordinary copies.
    unsafe { configure_el1(current_el & 0xc == 0x8) };
    unsafe { ane_step_byte(b'5') };

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
#[cfg(fullerene_aarch64_ane)]
const LINK_ENTRY: usize = 0x0048_0040;
#[cfg(not(any(fullerene_aarch64_bramble, fullerene_aarch64_ane)))]
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
