use super::{allocator::PhysicalFrameAllocator, cpu, elf, exceptions, mmu, task, uart};

const USER_CODE_ADDRESS: u64 = 0x4000_0000;
const USER_PAGE_SIZE: u64 = 4096;
const FIRST_STACK_ADDRESS: u64 = USER_CODE_ADDRESS + USER_PAGE_SIZE * 4;
const SECOND_STACK_ADDRESS: u64 = FIRST_STACK_ADDRESS;
const ELF_CODE_OFFSET: usize = 0x200;
const ELF_DATA_OFFSET: usize = 0x1000;
#[cfg(feature = "aarch64-user-fault-smoke")]
const ELF_CODE_WORD_COUNT: usize = 18;
#[cfg(not(feature = "aarch64-user-fault-smoke"))]
const ELF_CODE_WORD_COUNT: usize = 19;

/// Enter two tiny EL0 programs from the real AArch64 exception path.
///
/// The program executes the native syscall register convention (x8 = number,
/// x0..x5 = arguments). The normal smoke validates page permissions and
/// cooperative switching. The fault feature replaces the final switch/exit
/// sequence with an EL0 load from an unmapped page, exercising user-fault
/// retirement and the same saved-frame handoff.
pub(super) fn run(frames: &mut PhysicalFrameAllocator) -> ! {
    let first_address = USER_CODE_ADDRESS;
    // Both tasks intentionally use the same virtual image layout. Their
    // address-space slots provide the isolation boundary being exercised.
    let second_address = USER_CODE_ADDRESS;
    let first_image = make_smoke_elf(first_address);
    let second_image = make_smoke_elf(second_address);
    let Some(first) = elf::load_image(0, &first_image, frames) else {
        uart::puts("user-smoke: ELF load failed\n");
        cpu::wait_forever();
    };
    let Some(second) = elf::load_image(1, &second_image, frames) else {
        uart::puts("user-smoke: second ELF load failed\n");
        cpu::wait_forever();
    };
    let Some(first_stack_page) = frames.next_frame() else {
        uart::puts("user-smoke: first stack page unavailable\n");
        cpu::wait_forever();
    };
    let Some(second_stack_page) = frames.next_frame() else {
        uart::puts("user-smoke: second stack page unavailable\n");
        cpu::wait_forever();
    };
    if !elf::map_zeroed_user_page(0, FIRST_STACK_ADDRESS, first_stack_page)
        || !elf::map_zeroed_user_page(1, FIRST_STACK_ADDRESS, second_stack_page)
    {
        uart::puts("user-smoke: stack mapping failed\n");
        cpu::wait_forever();
    }

    uart::put_hex("user-smoke: task0 image-pages=", first.page_count as u64);
    uart::put_hex("user-smoke: task1 image-pages=", second.page_count as u64);
    uart::put_hex("user-smoke: task0 address-space=", 0);
    uart::put_hex("user-smoke: task1 address-space=", 1);
    uart::put_hex("user-smoke: task0 user-va=", USER_CODE_ADDRESS);
    uart::put_hex("user-smoke: task1 user-va=", USER_CODE_ADDRESS);
    uart::put_hex("user-smoke: task0 stack=", first_stack_page);
    uart::put_hex("user-smoke: task1 stack=", second_stack_page);
    unsafe {
        task::reset();
        assert!(task::install(
            0,
            1,
            b"task1",
            0,
            user_frame(first.entry, FIRST_STACK_ADDRESS + USER_PAGE_SIZE - 16)
        ));
        assert!(task::install(
            1,
            2,
            b"task2",
            1,
            user_frame(second.entry, SECOND_STACK_ADDRESS + USER_PAGE_SIZE - 16)
        ));
        assert!(mmu::activate_user_space(0));
        uart::put_hex("user-smoke: initial ttbr0=", mmu::active_ttbr0());
        exceptions::enter_user(&*task::current_frame())
    }
}

const fn user_frame(entry: u64, stack_top: u64) -> exceptions::Aarch64TrapFrame {
    exceptions::Aarch64TrapFrame {
        x: [0; 31],
        elr_el1: entry,
        spsr_el1: 0, // EL0t, with interrupts unmasked
        sp_el0: stack_top,
        esr_el1: 0,
        far_el1: 0,
        tpidr_el0: 0,
    }
}

fn make_smoke_elf(user_address: u64) -> [u8; ELF_DATA_OFFSET + 8] {
    let mut image = [0u8; ELF_DATA_OFFSET + 8];
    image[0..4].copy_from_slice(b"\x7fELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // ELFDATA2LSB
    image[6] = 1; // EV_CURRENT
    write_u16(&mut image, 16, 2); // ET_EXEC
    write_u16(&mut image, 18, 183); // EM_AARCH64
    write_u32(&mut image, 20, 1);
    write_u64(&mut image, 24, user_address + ELF_CODE_OFFSET as u64);
    write_u64(&mut image, 32, 64); // e_phoff
    write_u16(&mut image, 52, 64); // e_ehsize
    write_u16(&mut image, 54, 56); // e_phentsize
    write_u16(&mut image, 56, 2); // e_phnum

    let ph = 64;
    write_u32(&mut image, ph, 1); // PT_LOAD
    write_u32(&mut image, ph + 4, 5); // PF_R | PF_X
    write_u64(&mut image, ph + 8, 0);
    write_u64(&mut image, ph + 16, user_address);
    write_u64(
        &mut image,
        ph + 32,
        (ELF_CODE_OFFSET + ELF_CODE_WORD_COUNT * 4) as u64,
    );
    write_u64(&mut image, ph + 40, USER_PAGE_SIZE);
    write_u64(&mut image, ph + 48, USER_PAGE_SIZE);
    for (index, word) in smoke_code_words(user_address).iter().copied().enumerate() {
        let offset = ELF_CODE_OFFSET + index * 4;
        image[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    let data_ph = ph + 56;
    write_u32(&mut image, data_ph, 1); // PT_LOAD
    write_u32(&mut image, data_ph + 4, 6); // PF_R | PF_W
    write_u64(&mut image, data_ph + 8, ELF_DATA_OFFSET as u64);
    write_u64(&mut image, data_ph + 16, user_address + USER_PAGE_SIZE);
    write_u64(&mut image, data_ph + 32, 8);
    write_u64(&mut image, data_ph + 40, USER_PAGE_SIZE);
    write_u64(&mut image, data_ph + 48, USER_PAGE_SIZE);
    image[ELF_DATA_OFFSET..ELF_DATA_OFFSET + 8].copy_from_slice(&0xfeed_cafe_u64.to_le_bytes());
    image
}

const fn smoke_code_words(user_address: u64) -> [u32; ELF_CODE_WORD_COUNT] {
    let code_address = user_address + ELF_CODE_OFFSET as u64;
    let data_address = user_address + USER_PAGE_SIZE;
    let code_low = (code_address & 0xffff) as u32;
    let code_high = ((code_address >> 16) & 0xffff) as u32;
    let data_low = (data_address & 0xffff) as u32;
    let data_high = ((data_address >> 16) & 0xffff) as u32;
    [
        0xd280_0008,                    // mov x8, #0 (ABI_QUERY)
        0xd400_0001,                    // svc #0
        0xd280_0288,                    // mov x8, #20 (GETPID)
        0xd400_0001,                    // svc #0
        0xd280_0000 | (code_low << 5),  // movz x0, code address low
        0xf2a0_0000 | (code_high << 5), // movk x0, code address high, lsl #16
        0xd280_00a1,                    // mov x1, #5
        0xd280_02a8,                    // mov x8, #21 (GET_PROCESS_NAME)
        0xd400_0001,                    // svc #0, expected -EFAULT (code is RO)
        0xd280_0000 | (data_low << 5),  // movz x0, data address low
        0xf2a0_0000 | (data_high << 5), // movk x0, data address high, lsl #16
        0xd280_00a1,                    // mov x1, #5
        0xd280_02a8,                    // mov x8, #21 (GET_PROCESS_NAME)
        0xd400_0001,                    // svc #0
        #[cfg(not(feature = "aarch64-user-fault-smoke"))]
        0xd280_02c8, // mov x8, #22 (YIELD)
        #[cfg(not(feature = "aarch64-user-fault-smoke"))]
        0xd400_0001, // svc #0
        #[cfg(not(feature = "aarch64-user-fault-smoke"))]
        0xd280_0028, // mov x8, #1 (EXIT)
        #[cfg(not(feature = "aarch64-user-fault-smoke"))]
        0xd400_0001, // svc #0
        #[cfg(feature = "aarch64-user-fault-smoke")]
        0xd280_0000, // movz x0, #0 (invalid address low half)
        #[cfg(feature = "aarch64-user-fault-smoke")]
        0xf2a0_a000, // movk x0, #0x0500, lsl #16 => 0x05000000
        #[cfg(feature = "aarch64-user-fault-smoke")]
        0xf940_0000, // ldr x0, [x0] (unmapped EL0 read)
        0x1400_0000,                    // b .
    ]
}

fn write_u16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
