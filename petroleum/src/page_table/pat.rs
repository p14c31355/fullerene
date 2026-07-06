//! Page Attribute Table (PAT) initialization.
//!
//! Corresponds to Linux `pat_bp_init()` in `arch/x86/mm/pat/memtype.c`.
//!
//! The IA32_CR_PAT MSR (0x277) maps the 3-bit (PAT, PCD, PWT) combinations
//! in page table entries to actual memory cache types.  Without this setup
//! the firmware-default PAT often leaves PAT[1] as WT instead of WC, which
//! means `WRITE_THROUGH` page table flags do NOT give write-combining
//! semantics — breaking framebuffer performance (and sometimes correctness).
//!
//! # PAT encoding (full PAT, modern CPUs)
//!
//! ```text
//! PAT  PCD  PWT   Slot   Type
//!  0    0    0      0     WB
//!  0    0    1      1     WC   ★ framebuffer uses this
//!  0    1    0      2     UC-
//!  0    1    1      3     UC
//!  1    0    0      4     WB
//!  1    0    1      5     WP
//!  1    1    0      6     UC-
//!  1    1    1      7     WT
//! ```
//!
//! MSR value: `0x0407050106040706`

use x86_64::registers::model_specific::Msr;

/// MSR address for IA32_CR_PAT.
pub const MSR_IA32_CR_PAT: u32 = 0x0277;

/// Write the PAT MSR with the OS-defined value that enables WC on PAT[1].
///
/// # Safety
/// Must be called once per CPU during boot.  Writes to an MSR that affects
/// all subsequent memory type determinations.
pub unsafe fn init_pat() {
    // Same value Linux uses for modern CPUs with full PAT support.
    // PAT_VALUE(WB, WC, UC_MINUS, UC, WB, WP, UC_MINUS, WT)
    let pat_value: u64 = 0x0407_0501_0604_0706;
    unsafe {
        Msr::new(MSR_IA32_CR_PAT).write(pat_value);
    }
}
