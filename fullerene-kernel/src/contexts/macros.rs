//! `define_context!` — one macro to generate static + init + get + with + with_mut.
//!
//! Usage:
//! ```ignore
//! define_context!(AudioContext, audio, AUDIO);
//! ```
//!
//! Generates:
//! - `static AUDIO: Mutex<Option<AudioContext>>`
//! - `pub fn init_audio()`
//! - `pub fn get_audio() -> &'static Mutex<Option<AudioContext>>`
//! - `pub fn with_audio<F,R>(f: F) -> Option<R>`
//! - `pub fn with_audio_mut<F,R>(f: F) -> Option<R>`
#[macro_export]
macro_rules! define_context {
    ($T:ty, $mod_name:ident, $STATIC:ident) => {
        static $STATIC: spin::Mutex<Option<$T>> = spin::Mutex::new(None);

        pub fn $mod_name() {
            *$STATIC.lock() = Some(<$T>::new());
        }

        paste::paste! {
            pub fn [<init_ $mod_name>]() {
                $mod_name();
            }

            pub fn [<get_ $mod_name>]() -> &'static spin::Mutex<Option<$T>> {
                &$STATIC
            }

            pub fn [<with_ $mod_name>]<F, R>(f: F) -> Option<R>
            where
                F: FnOnce(&$T) -> R,
            {
                $STATIC.lock().as_ref().map(f)
            }

            pub fn [<with_ $mod_name _mut>]<F, R>(f: F) -> Option<R>
            where
                F: FnOnce(&mut $T) -> R,
            {
                $STATIC.lock().as_mut().map(f)
            }
        }
    };
}