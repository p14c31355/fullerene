use core::sync::atomic::{AtomicU64, Ordering};

use super::{allocator::PhysicalFrameAllocator, cpu, exceptions, mmu, uart};

const USER_CODE_ADDRESS: u64 = 0x4000_0000;
const USER_PAGE_SIZE: u64 = 4096;
const USER_STACK_TOP: u64 = USER_CODE_ADDRESS + USER_PAGE_SIZE - 16;

static SVC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Enter a tiny EL0 program from the real AArch64 exception path.
///
/// The program executes two SVC instructions. The first returns to EL0; the
/// second asks the handler to switch the prepared frame back to an EL1h
/// continuation. This validates page permissions, `eret`, and the mutable
/// trap-frame ABI without involving the future ELF loader yet.
pub(super) fn run(frames: &mut PhysicalFrameAllocator) -> ! {
    let Some(physical_page) = frames.next_frame() else {
        uart::puts("user-smoke: no physical page\n");
        cpu::wait_forever();
    };
    if !mmu::map_user_page(USER_CODE_ADDRESS, physical_page, true) {
        uart::puts("user-smoke: page mapping failed\n");
        cpu::wait_forever();
    }

    unsafe {
        let code = physical_page as *mut u32;
        core::ptr::write_volatile(code.add(0), 0xd400_0001); // svc #0
        core::ptr::write_volatile(code.add(1), 0xd400_0001); // svc #0
        core::ptr::write_volatile(code.add(2), 0x1400_0000); // b .
    }
    mmu::sync_code(physical_page);

    uart::put_hex("user-smoke: physical page=", physical_page);
    uart::put_hex("user-smoke: pc=", USER_CODE_ADDRESS);
    let frame = exceptions::Aarch64TrapFrame {
        x: [0; 31],
        elr_el1: USER_CODE_ADDRESS,
        spsr_el1: 0, // EL0t, with interrupts unmasked
        sp_el0: USER_STACK_TOP,
        esr_el1: 0,
        far_el1: 0,
    };
    exceptions::enter_user(&frame)
}

pub(super) fn handle_svc(frame: &mut exceptions::Aarch64TrapFrame) -> bool {
    let count = SVC_COUNT.fetch_add(1, Ordering::AcqRel);
    uart::put_hex("user-smoke: svc=", count + 1);
    if count == 0 {
        // QEMU and the AArch64 exception contract present ELR_EL1 at the
        // instruction after SVC, so resume without adding another word.
    } else {
        frame.x[0] = count + 1;
        frame.elr_el1 = return_from_user as *const () as usize as u64;
        frame.spsr_el1 = (frame.spsr_el1 & !0xf) | 0x5; // EL1h
    }
    true
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn return_from_user() -> ! {
    uart::puts("user-smoke: returned to EL1h\n");
    cpu::wait_forever()
}
