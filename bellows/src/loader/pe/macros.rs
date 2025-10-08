// Macro to read unaligned field from pointer
macro_rules! read_field {
    ($ptr:expr, $offset:expr, $ty:ty) => {
        unsafe { ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty) }
    };
}
