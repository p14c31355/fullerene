#[macro_use]
pub mod macros;

pub mod headers;
pub mod loader;

pub use loader::load_efi_image;
