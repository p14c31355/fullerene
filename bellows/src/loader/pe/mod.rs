// Macro to read unaligned field from pointer
#[macro_export]
macro_rules! read_field {
    ($ptr:expr, $offset:expr, $ty:ty) => {
        unsafe { ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty) }
    };
}

pub mod headers;
pub mod loader;

pub use loader::load_efi_image;
