//! Extended floating-point state support for user processes.
//!
//! Fullerene does not compile the kernel with AVX enabled, but Linux user
//! binaries may use XMM/YMM registers.  The kernel therefore enables the
//! x87/SSE/AVX state components and keeps one XSAVE image per process.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::Mutex;

/// The x87, SSE and AVX components occupy 832 bytes in the standard XSAVE
/// layout.  Keep the image larger and cache-line aligned so the layout remains
/// sufficient if the enabled mask grows modestly in the future.
pub const XSAVE_AREA_SIZE: usize = 1024;

pub const XSAVE_MASK: u64 = 0x7; // x87 | SSE | AVX

#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct XsaveState {
    bytes: [u8; XSAVE_AREA_SIZE],
}

impl XsaveState {
    pub const fn zeroed() -> Self {
        Self {
            bytes: [0; XSAVE_AREA_SIZE],
        }
    }

    /// Return a clean initial image captured during CPU initialization.
    pub fn initial() -> Self {
        *INITIAL_STATE.lock()
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.bytes.as_ptr() as *mut u8
    }
}

impl Default for XsaveState {
    fn default() -> Self {
        Self::zeroed()
    }
}

static INITIAL_STATE: Mutex<XsaveState> = Mutex::new(XsaveState::zeroed());
static ENABLED: AtomicBool = AtomicBool::new(false);
static MASK: AtomicU64 = AtomicU64::new(0);

/// Whether the current CPU has XSAVE-backed AVX state enabled.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Return the low/high halves of the active XSAVE mask for assembly callers.
pub fn mask() -> u64 {
    MASK.load(Ordering::Acquire)
}

/// Return a process-owned state pointer, or null when the feature is absent.
pub fn state_ptr(state: &XsaveState) -> *mut u8 {
    if enabled() {
        state.as_ptr()
    } else {
        core::ptr::null_mut()
    }
}

/// Enable x87/SSE/AVX state management on the boot CPU.
pub fn init() {
    #[cfg(any(target_os = "none", target_os = "uefi"))]
    unsafe {
        let basic = core::arch::x86_64::__cpuid(1);
        let has_xsave = basic.ecx & (1 << 26) != 0;
        let has_avx = basic.ecx & (1 << 28) != 0;
        let max_leaf = core::arch::x86_64::__cpuid(0).eax;
        if !has_xsave || !has_avx || max_leaf < 0xD {
            petroleum::serial::serial_log(format_args!(
                "[FPU] AVX unavailable xsave={} avx={} max_leaf={:#x}\n",
                has_xsave, has_avx, max_leaf
            ));
            return;
        }

        let xstate = core::arch::x86_64::__cpuid_count(0xD, 0);
        let supported_xcr0 = (xstate.eax as u64) | ((xstate.edx as u64) << 32);
        if supported_xcr0 & XSAVE_MASK != XSAVE_MASK {
            petroleum::serial::serial_log(format_args!(
                "[FPU] AVX xstate unavailable supported={:#x}\n",
                supported_xcr0
            ));
            return;
        }
        let avx_component = core::arch::x86_64::__cpuid_count(0xD, 2);
        let avx_area_end = (avx_component.ebx as usize)
            .checked_add(avx_component.eax as usize)
            .unwrap_or(usize::MAX);
        if avx_area_end > XSAVE_AREA_SIZE {
            petroleum::serial::serial_log(format_args!(
                "[FPU] AVX XSAVE component too large end={} limit={}\n",
                avx_area_end, XSAVE_AREA_SIZE
            ));
            return;
        }

        use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
        use x86_64::registers::xcontrol::{XCr0, XCr0Flags};

        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        Cr0::write(cr0);

        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE | Cr4Flags::OSXSAVE);
        Cr4::write(cr4);

        XCr0::write(XCr0Flags::X87 | XCr0Flags::SSE | XCr0Flags::AVX);

        // Establish a deterministic initial image.  The AVX instruction is
        // safe here because CR4.OSXSAVE and XCR0.AVX are already enabled.
        let mxcsr: u32 = 0x1f80;
        core::arch::asm!(
            "fninit",
            "pxor xmm0, xmm0",
            "pxor xmm1, xmm1",
            "pxor xmm2, xmm2",
            "pxor xmm3, xmm3",
            "pxor xmm4, xmm4",
            "pxor xmm5, xmm5",
            "pxor xmm6, xmm6",
            "pxor xmm7, xmm7",
            "pxor xmm8, xmm8",
            "pxor xmm9, xmm9",
            "pxor xmm10, xmm10",
            "pxor xmm11, xmm11",
            "pxor xmm12, xmm12",
            "pxor xmm13, xmm13",
            "pxor xmm14, xmm14",
            "pxor xmm15, xmm15",
            "vzeroall",
            "ldmxcsr [{mxcsr}]",
            mxcsr = in(reg) &mxcsr,
            options(nostack, preserves_flags)
        );

        MASK.store(XSAVE_MASK, Ordering::Release);
        let mut initial = XsaveState::zeroed();
        save(initial.as_mut_ptr());
        *INITIAL_STATE.lock() = initial;
        ENABLED.store(true, Ordering::Release);

        let enabled_size = core::arch::x86_64::__cpuid_count(0xD, 0).ebx;

        petroleum::serial::serial_log(format_args!(
            "[FPU] XSAVE/AVX enabled mask={:#x} area={} bytes\n",
            XSAVE_MASK, enabled_size
        ));
    }
}

/// Save the active x87/SSE/AVX state to an aligned XSAVE image.
#[inline(always)]
pub unsafe fn save(destination: *mut u8) {
    let mask = MASK.load(Ordering::Relaxed);
    if mask == 0 || destination.is_null() {
        return;
    }
    unsafe {
        core::arch::asm!(
            "xsave [{destination}]",
            destination = in(reg) destination,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
}

/// Restore the active x87/SSE/AVX state from an aligned XSAVE image.
#[inline(always)]
pub unsafe fn restore(source: *mut u8) {
    let mask = MASK.load(Ordering::Relaxed);
    if mask == 0 || source.is_null() {
        return;
    }
    unsafe {
        core::arch::asm!(
            "xrstor [{source}]",
            source = in(reg) source,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
}
