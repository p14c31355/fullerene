//! Minimal DPU (display processing unit) experiments that need no readback channel.
//!
//! Why this exists
//! ---------------
//! Every kernel-side readback channel tried on this handset is dead (see
//! `docs/DISPLAY_BRINGUP.md`, retractions #1-#3): `boot-reason` cannot tell a park
//! from a panic, the Android-return time ignores the kernel's park, and the
//! attach-cycle channel needs a host observer faster than the harness's 1 Hz poll.
//! The only reliable readout is the panel, watched by a human.
//!
//! So these experiments report nothing. They *act*, and the observer decides.
//!
//! Sources (all from the device tree, which is authoritative for this board):
//!   * `lito-sde.dtsi`: `qcom,sde-off = <0x1000>`,
//!     `qcom,sde-ctl-off = <0x2000 0x2200 0x2400 0x2600>`,
//!     `qcom,sde-mixer-off = <0x45000 0x46000 0x47000 0x48000>`
//!   * SSPP layer blocks sit at `MDSS + 0x1400 + n*0x200`, and the layer's
//!     source-address register is `+0x14` (`SSPP_SRC0_ADDR`); this was confirmed by
//!     reading back plausible frame-buffer addresses on hardware (the `paint2` probe).
//!
//! Deliberately *no* DPU programming: the bootloader already configured a working
//! scanout (it displays the Google logo), so the experiment reuses that state and
//! only changes where it fetches from. That is the same reasoning that worked for the
//! DPU-reuse approach earlier in this workstream.

/// MDSS register block base (`lito.dtsi`).
pub const MDSS_BASE: usize = 0x0ae0_0000;
/// DPU sub-block offset (`qcom,sde-off`).
pub const SDE_OFF: usize = 0x1000;
/// First SSPP layer block and the stride between layers (`dpu_hw_catalog`, `paint2`).
pub const SSPP_FIRST: usize = 0x1400;
pub const SSPP_STRIDE: usize = 0x200;
pub const SSPP_SRC0_ADDR: usize = 0x14;

/// A DDR range that is plausible as a frame buffer. The check keeps a mis-read
/// register from being treated as a buffer address and scribbled over.
fn looks_like_framebuffer(candidate: usize) -> bool {
    // 4 KiB aligned, inside the low 4 GiB, and not in the MMIO hole.
    candidate & 0xfff == 0 && (0x8000_0000..0xf000_0000).contains(&candidate)
}

/// Find which SSPP layer the bootloader's scanout is currently fetching from.
///
/// Returns `(block_offset, address)` for the first layer whose source address looks
/// like a real frame buffer.
pub unsafe fn active_layer() -> Option<(usize, usize)> {
    for layer in 0..8 {
        let block = MDSS_BASE + SSPP_FIRST + layer * SSPP_STRIDE;
        let addr = unsafe { core::ptr::read_volatile((block + SSPP_SRC0_ADDR) as *const u32) }
            as usize;
        if looks_like_framebuffer(addr) {
            return Some((SSPP_FIRST + layer * SSPP_STRIDE, addr));
        }
    }
    None
}

/// Paint the *existing* frame buffer white, in place.
///
/// The buffer is assumed to be 1080 x 2340 x 4 bytes (the panel's geometry at 32 bpp,
/// the usual DPU format). If the bootloader's scanout is live, the panel should show
/// white - and the observer is the instrument.
///
/// Returns the number of 32-bit words written, or `None` if no active layer was found.
pub unsafe fn paint_active_framebuffer_white() -> Option<usize> {
    let (_block, addr) = unsafe { active_layer() }?;
    // The DT's "Display Reserved" region is 0xA0000000 + 0x0240_0000 (36 MiB), so a
    // full-size buffer fits. Stay conservative: paint 1080 * 2340 pixels.
    let words = 1080usize * 2340;
    let dst = addr as *mut u32;
    for i in 0..words {
        unsafe {
            core::ptr::write_volatile(dst.add(i), 0xffff_ffff);
        }
    }
    Some(words)
}