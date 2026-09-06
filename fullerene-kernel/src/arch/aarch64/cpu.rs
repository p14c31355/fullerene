use core::arch::asm;

#[inline(always)]
pub(crate) fn wait_for_event() {
    unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
}

pub(crate) fn wait_forever() -> ! {
    loop {
        wait_for_event();
    }
}

#[cfg(fullerene_aarch64_qemu_usb_sim)]
pub(crate) fn semihost_exit(passed: bool) -> ! {
    #[repr(C)]
    struct ExitBlock {
        reason: u64,
        status: u64,
    }

    let block = ExitBlock {
        reason: 0x20026,
        status: if passed { 0 } else { 1 },
    };
    unsafe {
        asm!(
            "hlt #0xf000",
            in("x0") 0x18usize,
            in("x1") &block as *const ExitBlock,
            options(noreturn),
        );
    }
}
