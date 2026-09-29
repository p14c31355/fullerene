use std::io;
use std::path::Path;

/// Build the `io::Error` shape that this crate's argument guards used to spell out
/// inline, five lines at a time.
///
/// Every guard in `main.rs` and `bin/bramble-usb.rs` wrote:
///
/// ```ignore
/// return Err(io::Error::new(
///     io::ErrorKind::InvalidInput,
///     "some message",
/// ));
/// ```
///
/// 430-odd copies of that block dominated both files and buried the *conditions*
/// they guard, which is exactly what makes a missing or duplicated guard hard to
/// spot. These helpers keep the identical error value (same kind, same conversion
/// of the message) while collapsing each site to one line.
///
/// The message parameter accepts anything `io::Error::new` accepted, so both
/// `&'static str` literals and `format!(...)` strings work unchanged.
macro_rules! io_error_helpers {
    ($($fn_name:ident => $kind:ident, $doc:literal;)*) => {
        $(
            #[doc = $doc]
            pub fn $fn_name(
                message: impl Into<Box<dyn std::error::Error + Send + Sync>>,
            ) -> io::Error {
                io::Error::new(io::ErrorKind::$kind, message)
            }
        )*
    };
}

io_error_helpers! {
    invalid_input => InvalidInput, "A caller mistake: bad argument or flag combination.";
    invalid_data => InvalidData, "Malformed data found while reading an artifact.";
    unsupported => Unsupported, "The requested operation is not available on this target.";
    not_found => NotFound, "A required file or directory is missing.";
    timed_out => TimedOut, "An external tool did not finish in time.";
    other => Other, "A failure that does not fit the categories above.";
}

/// Finds the path to `libpthread.so.0` in common locations.
///
/// This function is a workaround for the `LD_PRELOAD` issue with QEMU on some systems.
/// It checks a list of common paths for the library and returns the first one that exists.
/// If the library is not found, it returns a default path.
pub fn find_libpthread() -> Option<String> {
    const COMMON_PATHS: &[&str] = &[
        "/lib/x86_64-linux-gnu/libpthread.so.0", // Debian/Ubuntu
        "/usr/lib64/libpthread.so.0",            // Fedora/CentOS
        "/usr/lib/libpthread.so.0",              // Arch/Other
    ];

    for path in COMMON_PATHS {
        if Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}
