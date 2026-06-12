//! Unified Context System — replaces scattered statics with named structs.
//!
//! # Macro-generated globals
//!
//! Each context uses [`context_global!`] which produces:
//! - `static XXX: Mutex<Option<XxxContext>>`
//! - `pub fn init_xxx()`
//! - `pub fn get_xxx() -> &'static Mutex<Option<XxxContext>>>`
//! - `pub fn with_xxx_mut<F, R>(f: F) -> Option<R>`
//! - `pub fn with_xxx<F, R>(f: F) -> Option<R>`

macro_rules! context_global {
    ($name:ident, $ctx:ty) => {
        paste::paste! {
            static [<$name:upper>]: spin::Mutex<Option<$ctx>> = spin::Mutex::new(None);

            pub fn [<init_ $name:lower>]() {
                *[<$name:upper>].lock() = Some(<$ctx>::new());
            }

            pub fn [<get_ $name:lower>]() -> &'static spin::Mutex<Option<$ctx>> {
                &[<$name:upper>]
            }

            pub fn [<with_ $name:lower _mut>]<F, R>(f: F) -> Option<R>
            where F: FnOnce(&mut $ctx) -> R {
                [<$name:upper>].lock().as_mut().map(f)
            }

            pub fn [<with_ $name:lower>]<F, R>(f: F) -> Option<R>
            where F: FnOnce(&$ctx) -> R {
                [<$name:upper>].lock().as_ref().map(f)
            }
        }
    };

    ($name:ident, $ctx:ty, $init:expr) => {
        paste::paste! {
            static [<$name:upper>]: spin::Mutex<Option<$ctx>> = spin::Mutex::new(None);

            pub fn [<init_ $name:lower>]() {
                *[<$name:upper>].lock() = Some($init);
            }

            pub fn [<get_ $name:lower>]() -> &'static spin::Mutex<Option<$ctx>> {
                &[<$name:upper>]
            }

            pub fn [<with_ $name:lower _mut>]<F, R>(f: F) -> Option<R>
            where F: FnOnce(&mut $ctx) -> R {
                [<$name:upper>].lock().as_mut().map(f)
            }

            pub fn [<with_ $name:lower>]<F, R>(f: F) -> Option<R>
            where F: FnOnce(&$ctx) -> R {
                [<$name:upper>].lock().as_ref().map(f)
            }
        }
    };
}

pub mod audio;
pub mod boot;
pub mod framebuffer;
pub mod input;
pub mod memory;
pub mod pci;
pub mod window;

pub use audio::AudioContext;
pub use boot::BootContext;
pub use framebuffer::FramebufferContext;
pub use input::InputContext;
pub use memory::MemoryContext;
pub use pci::PciContext;
pub use window::WindowContext;
