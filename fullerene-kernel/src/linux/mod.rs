//! Linux ABI emulation layer for Fullerene.

#[macro_export]
macro_rules! linux_stub {
    ($name:ident, $ret:expr) => {
        pub fn $name(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
            $ret
        }
    };
}
#[macro_export]
macro_rules! linux_stub_errno {
    ($name:ident, $err:expr) => {
        pub fn $name(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
            errno_code($err)
        }
    };
}

pub mod fs;
pub mod launch;
pub mod memory;
pub mod misc;
pub mod numbers;
pub mod process;
pub mod runtime;
pub mod signal;
pub mod test_binary;
pub mod time;
pub mod types;

pub use numbers::*;
pub use runtime::{DispatchMode, LinuxErrno, LinuxRuntime, errno_code};
