// Linux ABI data structures
#![allow(dead_code)]

use core::fmt;

/// Linux struct stat (x86_64)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub unused: [i64; 3],
}

impl LinuxStat {
    pub const fn zeroed() -> Self {
        Self {
            st_dev: 0,
            st_ino: 0,
            st_nlink: 0,
            st_mode: 0,
            st_uid: 0,
            st_gid: 0,
            pad0: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 0,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
            unused: [0; 3],
        }
    }
}

/// Linux struct statx (x86_64)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxStatx {
    pub stx_mask: u32,
    pub stx_blksize: u32,
    pub stx_attributes: u64,
    pub stx_nlink: u32,
    pub stx_uid: u32,
    pub stx_gid: u32,
    pub stx_mode: u16,
    pub pad: [u16; 1],
    pub stx_ino: u64,
    pub stx_size: u64,
    pub stx_blocks: u64,
    pub stx_attributes_mask: u64,
    pub stx_atime: LinuxStatxTimestamp,
    pub stx_btime: LinuxStatxTimestamp,
    pub stx_ctime: LinuxStatxTimestamp,
    pub stx_mtime: LinuxStatxTimestamp,
    pub stx_rdev_major: u32,
    pub stx_rdev_minor: u32,
    pub stx_dev_major: u32,
    pub stx_dev_minor: u32,
    pub spare: [u64; 14],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxStatxTimestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub pad: i32,
}

/// Linux struct sigaction (x86_64)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LinuxSigAction {
    pub sa_handler: u64,
    pub sa_flags: u64,
    pub sa_restorer: u64,
    pub sa_mask: u64,
}

impl Default for LinuxSigAction {
    fn default() -> Self {
        Self {
            sa_handler: 0,
            sa_flags: 0,
            sa_restorer: 0,
            sa_mask: 0,
        }
    }
}

impl fmt::Debug for LinuxSigAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigAction")
            .field("handler", &self.sa_handler)
            .field("flags", &self.sa_flags)
            .field("restorer", &self.sa_restorer)
            .field("mask", &self.sa_mask)
            .finish()
    }
}

/// Linux struct timespec (x86_64)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxTimespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// Linux struct timeval
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxTimeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// Linux struct timezone
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxTimezone {
    pub tz_minuteswest: i32,
    pub tz_dsttime: i32,
}

/// Linux struct utsname (x86_64)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxUtsname {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

impl LinuxUtsname {
    pub fn new() -> Self {
        Self {
            sysname: Self::str_to_fixed("Linux"),
            nodename: Self::str_to_fixed("fullerene"),
            release: Self::str_to_fixed("6.6.0-fullerene"),
            version: Self::str_to_fixed("#1 Fullerene OS"),
            machine: Self::str_to_fixed("x86_64"),
            domainname: Self::str_to_fixed("(none)"),
        }
    }

    fn str_to_fixed(s: &str) -> [u8; 65] {
        let mut buf = [0u8; 65];
        let bytes = s.as_bytes();
        let len = bytes.len().min(64);
        buf[..len].copy_from_slice(&bytes[..len]);
        buf[len] = 0;
        buf
    }
}

/// Linux struct dirent64 for getdents64
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxDirent64 {
    pub d_ino: u64,
    pub d_off: i64,
    pub d_reclen: u16,
    pub d_type: u8,
    pub d_name: [u8; 256],
}

/// Linux struct rlimit
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxRLimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

/// Linux struct winsize (for TIOCGWINSZ ioctl)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxWinsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Linux struct iovec (for readv/writev)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxIovec {
    pub iov_base: u64,
    pub iov_len: u64,
}

/// Linux struct sysinfo
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LinuxSysinfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u16,
    pub totalhigh: u64,
    pub freehigh: u64,
    pub mem_unit: u32,
    pub pad: [u8; 20],
}

impl LinuxSysinfo {
    pub fn new() -> Self {
        Self {
            uptime: 0,
            loads: [0; 3],
            totalram: 128 * 1024 * 1024,
            freeram: 64 * 1024 * 1024,
            sharedram: 0,
            bufferram: 0,
            totalswap: 0,
            freeswap: 0,
            procs: 1,
            totalhigh: 0,
            freehigh: 0,
            mem_unit: 1,
            pad: [0u8; 20],
        }
    }
}

// File mode constants (S_IFMT etc.)
pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

// Permissions
pub const S_IRWXU: u32 = 0o700;
pub const S_IRUSR: u32 = 0o400;
pub const S_IWUSR: u32 = 0o200;
pub const S_IXUSR: u32 = 0o100;
pub const S_IRWXG: u32 = 0o070;
pub const S_IRGRP: u32 = 0o040;
pub const S_IWGRP: u32 = 0o020;
pub const S_IXGRP: u32 = 0o010;
pub const S_IRWXO: u32 = 0o007;
pub const S_IROTH: u32 = 0o004;
pub const S_IWOTH: u32 = 0o002;
pub const S_IXOTH: u32 = 0o001;

pub fn mode_from_type(is_dir: bool) -> u32 {
    if is_dir {
        S_IFDIR | S_IRWXU | S_IRGRP | S_IXGRP | S_IROTH | S_IXOTH
    } else {
        S_IFREG | S_IRUSR | S_IWUSR | S_IRGRP | S_IROTH
    }
}
