//! VFS types — re-exported from `crate::contexts::vfs`.
pub use crate::contexts::vfs::{
    change_directory, close, create, exists, init_vfs as init, mkdir, mount, open, read, readdir,
    seek, unlink, unmount, working_directory, write, FileDescriptor, FileSystem, InodeType,
    MemFileSystem, VNode, Vfs,
};
