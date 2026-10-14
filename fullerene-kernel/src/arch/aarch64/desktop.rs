//! The ANE desktop bring-up: solvent + lattice on the phone's canvas.
//!
//! The x86 boots the desktop through `gui.rs` (CMOS clock, APIC watchdog,
//! GPU renderer, kernel-heap callbacks). The ANE has none of those: the
//! canvas at 0x31000000 is the scan-out surface and large buffers go
//! through `PageBuf`, which needs a frame-allocator adapter. This module
//! is the small ANE counterpart of `gui::init` + `gui::render`:
//!
//! 1. install the frame-allocator adapter (PageBuf's backing store),
//! 2. initialise the solvent runtime with a nominal millisecond clock
//!    (`set_tsc_per_ms(1000)`; the timer IRQ advances `solvent::ANE_TICKS`),
//! 3. render one desktop frame into the canvas,
//! 4. blit the canvas to the display windows (`ane_fb::flush_panel`).
//!
//! It runs before `launchd::run()`: the launchd's VFS work can park
//! forever, and the desktop paint makes the screen carry the result
//! either way. The boot instruments stop painting when the desktop takes
//! the canvas (`ane_fb::DESKTOP_OWNED`); the durable ring is unaffected.

use core::slice;
use core::sync::atomic::Ordering;

use petroleum::graphics::FramebufferGuard;
use petroleum::page_table::constants::{AneFrameAllocator, init_frame_allocator};

use super::platform::ane_fb;
use super::uart;

/// Bring up the desktop on the ANE canvas. Called once, after the heap is
/// live and before the user task starts.
/// v315: set by the pump's park-recovery path before calling bring_up, so
/// bring_up skips the inline 9.6 MB panel flush (it would block the IRQ for
/// minutes and starve the SP805). The tick-driven row flushes push the
/// canvas out afterwards.
pub static PARK_RECOVERY_RUN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn bring_up() {
    // Silence every IRQ-side canvas paint/row transfer before the first
    // cacheable probe. The probe must measure its own memory path, not the
    // diagnostic framebuffer traffic that runs concurrently in the timer IRQ.
    ane_fb::IRQ_QUIET.store(true, Ordering::Relaxed);
    uart::puts("desktop: bring-up\n");

    // v247: measure FIRST (instruments live, arm_ms(100) already set by the
    // v246b timer re-arrangement). The v243 rings showed the fill parked at
    // 0x1C027F8 with TM: never printed - the fill time itself was never
    // seen. TM1 = the cold+warm 24 KiB static fill with the timer running
    // at 100 ms period; if the crawl is IRQ starvation this now completes
    // and prints. IRQQ comes after, so its effect is measurable too.
    #[cfg(fullerene_ane_desktop_memory_probe)]
    {
        static mut PROBE_BUF: [u32; 6144] = [0; 6144];
        let t0 = super::timer::uptime_us();
        uart::puts("SF:0\n");
        unsafe {
            let base = core::ptr::addr_of_mut!(PROBE_BUF) as *mut u32;
            for index in 0..6144usize {
                core::ptr::write_volatile(base.add(index), 0x00CC_CCCC);
            }
        }
        let t1 = super::timer::uptime_us();
        uart::puts("SW:0\n");
        unsafe {
            let base = core::ptr::addr_of_mut!(PROBE_BUF) as *mut u32;
            for index in 0..6144usize {
                core::ptr::write_volatile(base.add(index), 0x00CC_CCCC);
            }
        }
        let t2 = super::timer::uptime_us();
        uart::puts("TM1: cold=");
        uart::put_hex("", t1.wrapping_sub(t0));
        uart::puts(" warm=");
        uart::put_hex("", t2.wrapping_sub(t1));
        uart::putc(b'\n');
    }

    uart::puts("IRQQ\n");

    // TM2: the same fill again under IRQ_QUIET - the delta vs TM1 tells
    // whether the instrument paints were the whole cost.
    #[cfg(fullerene_ane_desktop_memory_probe)]
    {
        static mut PROBE_BUF2: [u32; 6144] = [0; 6144];
        let t0 = super::timer::uptime_us();
        unsafe {
            let base = core::ptr::addr_of_mut!(PROBE_BUF2) as *mut u32;
            for index in 0..6144usize {
                core::ptr::write_volatile(base.add(index), 0x00CC_CCCC);
            }
        }
        let t1 = super::timer::uptime_us();
        uart::puts("TM2: quiet_cold=");
        uart::put_hex("", t1.wrapping_sub(t0));
        uart::putc(b'\n');
    }
    // v232: the mmu now invalidates the data cache by set/way before
    // enabling it (see mmu.rs), so the first-touch cacheable store should
    // no longer crawl. This probe is the same 24 KiB fill that parked in
    // v230/v231; its completion marker in the ring is the acceptance test.
    #[cfg(fullerene_ane_desktop_memory_probe)]
    {
        use alloc::vec::Vec as AVec;
        uart::puts("PR: fill start\n");
        let mut probe: AVec<[u32; 3]> = AVec::with_capacity(2000);
        uart::puts("PP: ptr=");
        uart::put_hex("", probe.as_ptr() as u64);
        uart::putc(b'\n');
        let t0 = super::timer::uptime_us();
        for i in 0..2000usize {
            probe.push([0x00CC_CCCC, 0, 0x20]);
            if i % 250 == 0 {
                uart::puts("PF:");
            }
        }
        let t1 = super::timer::uptime_us();
        uart::puts("TM: heap_us=");
        uart::put_hex("", t1.wrapping_sub(t0));
        uart::putc(b'\n');
        uart::puts("PR: fill done\n");
        core::hint::black_box(&probe);
    }

    // 1. PageBuf backing store: the adapter calls back into the kernel's
    //    DTB frame allocator.
    init_frame_allocator(AneFrameAllocator::new(ane_alloc_frames, ane_free_frames));
    uart::puts("D1\n");

    // 2. Nominal clock. ANE_TICKS carries milliseconds; the timer IRQ
    //    keeps it fresh, so solvent's frame pacing and the clock run at
    //    the tick rate of the phone's own timer.
    solvent::set_tsc_per_ms(1000);
    solvent::set_render_progress_fn(render_progress);
    uart::puts("D2\n");
    solvent::init();
    #[cfg(fullerene_aarch64_ane)]
    {
        ane_fb::STG_STAGE.store(b'S', Ordering::Relaxed);
        ane_fb::event(b"STG SOLVENT-INIT-OK");
    }
    solvent::apply_settings(1.0, 100, true);
    solvent::tick_core(solvent::now_tsc());
    uart::puts("desktop: init done\n");

    // 3. The desktop owns the canvas from here on: the boot instruments
    //    stop painting so their text rows cannot scribble over it.
    ane_fb::DESKTOP_OWNED.store(true, Ordering::Relaxed);

    // v331m: the landscape desktop mode - the solvent layout fits the
    // landscape natively (the user's design call); the flush rotates.
    #[cfg(fullerene_ane_desktop_landscape)]
    let (width, height) = (ane_fb::HEIGHT, ane_fb::WIDTH);
    #[cfg(not(fullerene_ane_desktop_landscape))]
    let (width, height) = (ane_fb::WIDTH, ane_fb::HEIGHT);
    let canvas = unsafe {
        slice::from_raw_parts_mut(
            ane_fb::CANVAS as *mut u32,
            (width as usize) * (height as usize),
        )
    };
    // v330: the phone-native home screen renders FIRST (primitive drawing,
    // no font dependency) so the device gets a usable portrait UI even if
    // the solvent render path hangs. It is the first backend of the
    // reusable mobile shell.
    #[cfg(fullerene_aarch64_ane)]
    {
        super::mobile_home::render_home();
        ane_fb::STG_STAGE.store(b'H', Ordering::Relaxed);
        ane_fb::event(b"STG HOME-OK");
        ane_fb::flush_panel();
        uart::puts("mobile home: rendered\n");
    }
    let Some(mut framebuffer) = FramebufferGuard::try_new(canvas, width, height, width) else {
        uart::puts("desktop: framebuffer guard refused\n");
        return;
    };
    #[cfg(fullerene_aarch64_ane)]
    {
        ane_fb::STG_STAGE.store(b'T', Ordering::Relaxed);
        ane_fb::event(b"STG RENDER-START");
    }
    solvent::render(&mut framebuffer);
    #[cfg(fullerene_aarch64_ane)]
    {
        ane_fb::STG_STAGE.store(b'K', Ordering::Relaxed);
        ane_fb::event(b"STG RENDER-OK");
    }
    uart::puts("desktop: rendered\n");

    // v331l: the mobile shell reclaims the panel. The solvent render is
    // the desktop-composite proof (the milestone), but the phone's face
    // is the mobile home: after the proof, the shell re-renders itself
    // over the canvas. The desktop becomes a launchable full-screen app
    // once the touch path lands.
    #[cfg(fullerene_aarch64_ane)]
    {
        super::mobile_home::render_home();
        ane_fb::event(b"STG HOME-RESUME");
        uart::puts("mobile home: resumed\n");
    }

    // 4. Propagate the canvas to every display channel the gates accept.
    // v315: when bring_up runs from the pump's park recovery (IRQ context),
    // the 9.6 MB panel flush would block the IRQ for minutes and starve the
    // SP805 (the v315 2m07s deaths). The tick-driven row flushes push the
    // canvas out afterwards; only the boot-thread path flushes inline.
    if !PARK_RECOVERY_RUN.load(Ordering::Relaxed) {
        #[cfg(fullerene_ane_desktop_landscape)]
        unsafe {
            ane_fb::flush_panel_rotated();
        }
        #[cfg(not(fullerene_ane_desktop_landscape))]
        ane_fb::flush_panel();
    }
    uart::puts("desktop: done\n");
}

/// Frame allocator adapter: `pages` contiguous frames from the ANE's
/// DTB-backed pool, or `None` when no run that long is free.
fn ane_alloc_frames(pages: usize) -> Option<u64> {
    super::allocator::with_global(|frames| frames.allocate_contiguous_frames(pages)).flatten()
}

/// Return a contiguous run to the bounded free list. A release that the
/// bounded list cannot take simply leaves the frames allocated.
fn ane_free_frames(start: u64, pages: usize) {
    let _ = super::allocator::with_global(|frames| {
        for index in 0..pages {
            frames.release_frame(start + (index as u64) * 4096);
        }
    });
}

/// Mirror solvent's render progress lines into the durable ring, so a
/// collection after a failed paint shows exactly how far it got.
fn render_progress(label: &[u8]) {
    for &byte in label {
        uart::putc(byte);
    }
    uart::putc(b'\n');
}
