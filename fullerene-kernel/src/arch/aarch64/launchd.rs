//! Bootstrap the first real AArch64 user ELF.
//!
//! The payload is compiled by `build.rs` from a freestanding AArch64 source
//! file in the initramfs. Keeping the bootstrap here makes the kernel's first
//! user process go through the ordinary ELF, page-table, trap frame, and SVC
//! boundaries rather than through an in-kernel test program.

use super::{cpu, elf, exceptions, fs, mmu, task, uart, user_memory};

// Keep the bootstrap stack outside the fixed-address ET_EXEC image window.
// The old 0x4001_0000 page overlapped launchd's read-only data segment.
const STACK_ADDRESS: u64 = 0x47ff_0000;
const PAGE_SIZE: u64 = 4096;
// Android's Rust PID 1 has a bounded property-service parser and uses more
// than the original one-page launchd probe stack. Keep the initial stack
// explicitly mapped instead of relying on an unmapped downward guard page.
const BOOT_STACK_PAGES: usize = 16;
const MAX_LAUNCHD_IMAGE: usize = 4 * 1024 * 1024;

#[cfg(feature = "aarch64-linux-smoke")]
const BOOT_IMAGE_PATH: &[u8] = b"/bin/linux-smoke";
#[cfg(all(not(feature = "aarch64-linux-smoke"), feature = "aarch64-android-init"))]
// FullereneOS owns PID 1.  Keep the Rust image at the initramfs root so a
// physical `/system` mount cannot silently replace it with the stock Android
// init.  Stock init is intentionally not copied into this bounded initramfs;
// a future compatibility probe must use a separately selected artifact.
const BOOT_IMAGE_PATH: &[u8] = b"/init";
#[cfg(not(feature = "aarch64-linux-smoke"))]
#[cfg(not(feature = "aarch64-android-init"))]
const BOOT_IMAGE_PATH: &[u8] = b"/bin/launchd";
#[cfg(feature = "aarch64-linux-smoke")]
const BOOT_TASK_NAME: &[u8] = b"linux-smoke";
#[cfg(all(not(feature = "aarch64-linux-smoke"), feature = "aarch64-android-init"))]
const BOOT_TASK_NAME: &[u8] = b"init";
#[cfg(all(
    not(feature = "aarch64-linux-smoke"),
    not(feature = "aarch64-android-init")
))]
const BOOT_TASK_NAME: &[u8] = b"launchd";

static mut LAUNCHD_STAGING: [u8; MAX_LAUNCHD_IMAGE] = [0; MAX_LAUNCHD_IMAGE];

pub(super) fn run() -> ! {
    #[cfg(feature = "aarch64-android-init")]
    uart::puts(if fs::android_system_mounted() {
        "user-launchd: Fullerene Rust init source=initramfs; Android system mounted\n"
    } else {
        "user-launchd: Fullerene Rust init source=initramfs; Android system absent\n"
    });
    let image_length = unsafe {
        fs::read_kernel_path(
            core::str::from_utf8(BOOT_IMAGE_PATH).unwrap_or("/bin/launchd"),
            &mut *core::ptr::addr_of_mut!(LAUNCHD_STAGING),
        )
        .unwrap_or(0)
    };
    if image_length == 0 {
        uart::puts("user-launchd: initramfs image is empty\n");
        cpu::wait_forever();
    }
    let main_image = unsafe { &*core::ptr::addr_of!(LAUNCHD_STAGING) };
    let main_image = &main_image[..image_length];
    let Some(main_image) =
        super::allocator::with_global(|frames| elf::load_image(0, main_image, frames)).flatten()
    else {
        uart::puts("user-launchd: ELF load failed\n");
        cpu::wait_forever();
    };

    // A dynamically linked Android executable is entered through its
    // PT_INTERP image. The main executable has already been copied into user
    // pages, so the shared staging buffer can be reused for linker64.
    let mut interpreter_entry = main_image.entry;
    let mut interpreter_base = 0u64;
    if main_image.interpreter_path_len != 0 {
        let path =
            core::str::from_utf8(&main_image.interpreter_path[..main_image.interpreter_path_len])
                .unwrap_or("");
        let interpreter_length = unsafe {
            fs::read_kernel_path(path, &mut *core::ptr::addr_of_mut!(LAUNCHD_STAGING)).unwrap_or(0)
        };
        if interpreter_length == 0 {
            uart::puts("user-launchd: PT_INTERP image is unavailable\n");
            cpu::wait_forever();
        }
        let interpreter_image = unsafe { &*core::ptr::addr_of!(LAUNCHD_STAGING) };
        let interpreter_image = &interpreter_image[..interpreter_length];
        let Some(interpreter) = super::allocator::with_global(|frames| {
            elf::load_image_at(
                0,
                interpreter_image,
                frames,
                elf::INTERPRETER_LOAD_BASE,
                false,
            )
        })
        .flatten() else {
            uart::puts("user-launchd: PT_INTERP ELF load failed\n");
            cpu::wait_forever();
        };
        interpreter_entry = interpreter.entry;
        interpreter_base = interpreter.load_bias;
        uart::put_hex(
            "user-launchd: interpreter-pages=",
            interpreter.page_count as u64,
        );
        uart::put_hex("user-launchd: interpreter-entry=", interpreter.entry);
    }

    let Some(stack_page) = super::allocator::with_global(|frames| {
        let mut mapped = 0usize;
        let mut first_page = 0u64;
        while mapped < BOOT_STACK_PAGES {
            let virtual_address = STACK_ADDRESS - mapped as u64 * PAGE_SIZE;
            let stack_page = frames.next_frame()?;
            if !elf::map_zeroed_user_page(0, virtual_address, stack_page) {
                return None;
            }
            first_page = stack_page;
            mapped += 1;
        }
        Some(first_page)
    })
    .flatten() else {
        uart::puts("user-launchd: stack allocation failed\n");
        cpu::wait_forever();
    };

    uart::put_hex("user-launchd: image-pages=", main_image.page_count as u64);
    uart::put_hex("user-launchd: entry=", main_image.entry);
    uart::put_hex("user-launchd: stack=", stack_page);
    task::reset();
    let personality = if cfg!(any(
        feature = "aarch64-linux-smoke",
        feature = "aarch64-android-init"
    )) {
        task::AbiPersonality::LinuxAarch64
    } else {
        task::AbiPersonality::Native
    };
    let mut frame = user_frame(interpreter_entry, STACK_ADDRESS + PAGE_SIZE - 16);
    if !task::install_with_personality(0, 1, BOOT_TASK_NAME, 0, frame, personality) {
        uart::puts("user-launchd: task install failed\n");
        cpu::wait_forever();
    }
    if !mmu::activate_user_space(0) {
        uart::puts("user-launchd: address-space activation failed\n");
        cpu::wait_forever();
    }
    if personality == task::AbiPersonality::LinuxAarch64 {
        let Some(stack) = install_linux_stack(
            &mut frame,
            &main_image,
            (interpreter_base != 0).then_some(interpreter_base),
        ) else {
            uart::puts("user-launchd: Linux initial stack failed\n");
            cpu::wait_forever();
        };
        if !task::set_current_stack(&mut frame, stack) {
            uart::puts("user-launchd: Linux stack publish failed\n");
            cpu::wait_forever();
        }
    }
    uart::put_hex("user-launchd: ttbr0=", mmu::active_ttbr0());

    unsafe { exceptions::enter_user(&*task::current_frame()) }
}

fn install_linux_stack(
    frame: &mut exceptions::Aarch64TrapFrame,
    image: &elf::LoadedImage,
    interpreter_base: Option<u64>,
) -> Option<u64> {
    let mut cursor = STACK_ADDRESS + PAGE_SIZE;
    let argument = BOOT_IMAGE_PATH;
    cursor = cursor.checked_sub(argument.len() as u64 + 1)?;
    let argument_address = cursor;
    user_memory::copy_to_user(argument_address, argument).ok()?;
    user_memory::copy_to_user(argument_address + argument.len() as u64, &[0]).ok()?;

    cursor &= !15;
    let mut words = [0u64; 32];
    let mut count = 0usize;
    let mut push = |value: u64| {
        words[count] = value;
        count += 1;
    };
    push(1); // argc
    push(argument_address);
    push(0); // argv terminator
    push(0); // envp terminator
    push(7); // AT_BASE
    push(interpreter_base.unwrap_or(0));
    push(3); // AT_PHDR
    push(image.phdr);
    push(4); // AT_PHENT
    push(image.phent);
    push(5); // AT_PHNUM
    push(image.phnum);
    push(6); // AT_PAGESZ
    push(PAGE_SIZE);
    push(9); // AT_ENTRY
    push(image.entry);
    push(23); // AT_SECURE
    push(0);
    push(31); // AT_EXECFN
    push(argument_address);
    push(0); // AT_NULL
    push(0);
    let bytes = count.checked_mul(core::mem::size_of::<u64>())? as u64;
    let stack_pointer = cursor.checked_sub(bytes)? & !15;
    let raw = unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes as usize) };
    user_memory::copy_to_user(stack_pointer, raw).ok()?;
    frame.sp_el0 = stack_pointer;
    Some(stack_pointer)
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
