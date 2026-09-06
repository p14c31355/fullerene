//! Bootstrap the first real AArch64 user ELF.
//!
//! The payload is compiled by `build.rs` from a freestanding AArch64 source
//! file in the initramfs. Keeping the bootstrap here makes the kernel's first
//! user process go through the ordinary ELF, page-table, trap frame, and SVC
//! boundaries rather than through an in-kernel test program.

use super::{cpu, elf, exceptions, fs, mmu, task, uart};

// Keep the bootstrap stack outside the fixed-address ET_EXEC image window.
// The old 0x4001_0000 page overlapped launchd's read-only data segment.
const STACK_ADDRESS: u64 = 0x41ff_0000;
const PAGE_SIZE: u64 = 4096;
const MAX_LAUNCHD_IMAGE: usize = 96 * 1024;

static mut LAUNCHD_STAGING: [u8; MAX_LAUNCHD_IMAGE] = [0; MAX_LAUNCHD_IMAGE];

pub(super) fn run() -> ! {
    let image_length = unsafe {
        fs::read_kernel_path(
            "/bin/launchd",
            &mut *core::ptr::addr_of_mut!(LAUNCHD_STAGING),
        )
        .unwrap_or(0)
    };
    if image_length == 0 {
        uart::puts("user-launchd: initramfs image is empty\n");
        cpu::wait_forever();
    }
    let image = unsafe { &*core::ptr::addr_of!(LAUNCHD_STAGING) };
    let image = &image[..image_length];

    let Some((image, stack_page)) = super::allocator::with_global(|frames| {
        let image = elf::load_image(0, image, frames)?;
        let stack_page = frames.next_frame()?;
        if !elf::map_zeroed_user_page(0, STACK_ADDRESS, stack_page) {
            return None;
        }
        Some((image, stack_page))
    })
    .flatten() else {
        uart::puts("user-launchd: ELF load failed\n");
        cpu::wait_forever();
    };

    uart::put_hex("user-launchd: image-pages=", image.page_count as u64);
    uart::put_hex("user-launchd: entry=", image.entry);
    uart::put_hex("user-launchd: stack=", stack_page);
    task::reset();
    if !task::install(
        0,
        1,
        b"launchd",
        0,
        user_frame(image.entry, STACK_ADDRESS + PAGE_SIZE - 16),
    ) {
        uart::puts("user-launchd: task install failed\n");
        cpu::wait_forever();
    }
    if !mmu::activate_user_space(0) {
        uart::puts("user-launchd: address-space activation failed\n");
        cpu::wait_forever();
    }
    uart::put_hex("user-launchd: ttbr0=", mmu::active_ttbr0());

    unsafe { exceptions::enter_user(&*task::current_frame()) }
}

const fn user_frame(entry: u64, stack_top: u64) -> exceptions::Aarch64TrapFrame {
    exceptions::Aarch64TrapFrame {
        x: [0; 31],
        elr_el1: entry,
        spsr_el1: 0, // EL0t, with interrupts unmasked
        sp_el0: stack_top,
        esr_el1: 0,
        far_el1: 0,
    }
}
