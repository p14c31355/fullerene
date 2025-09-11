#![no_std]
#![no_main]

use spin::once::Once;

struct VgaBuffer {
    ptr: *mut u8,
}

impl VgaBuffer {
    fn new() -> Self {
        Self { ptr: 0xb8000 as *mut u8 }
    }

    fn write(&self, s: &str) {
        for (i, byte) in s.bytes().enumerate() {
            unsafe {
                *self.ptr.add(i * 2) = byte;
                *self.ptr.add(i * 2 + 1) = 0x0f;
            }
        }
    }
}

unsafe impl Send for VgaBuffer {}
unsafe impl Sync for VgaBuffer {}

static VGA: Once<VgaBuffer> = Once::new();

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    VGA.call_once(|| VgaBuffer::new());
    VGA.get().unwrap().write("Hello QEMU by fullerene!");
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
