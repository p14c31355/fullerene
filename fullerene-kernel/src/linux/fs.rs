// Linux file system syscall implementations
use super::runtime::{LinuxRuntime, Runtime, LinuxFileDesc, copy_user_string, copy_from_user, copy_to_user, copy_val_to_user, errno_code, errno_result, to_linux_errno};
use super::types::*;
use super::numbers::*;
use crate::vfs;

pub fn sys_read(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2] as usize;
    if count == 0 {
        return 0;
    }
    // Stdin: read from keyboard
    if fd == 0 {
        if buf == 0 {
            return errno_code(EFAULT);
        }
        let count_min = count.min(512);
        // Single byte reads are most common for terminal input
        if count == 1 {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                unsafe { core::ptr::write_volatile(buf as *mut u8, ch) };
                return 1;
            }
            return 0;
        }
        // Multi-byte: drain line buffer into a kernel buffer, then copy to user
        let mut kernel_buf = [0u8; 512];
        let n = nitrogen::ps2::keyboard::drain_line_buffer(&mut kernel_buf[..count_min]);
        if n > 0 {
            if unsafe { copy_to_user(buf, &kernel_buf[..n]) }.is_err() {
                return errno_code(EFAULT);
            }
        }
        return n as u64;
    }
    // Stdout/stderr: write to serial
    if fd == 1 || fd == 2 {
        return errno_code(EBADF);
    }
    // Read from file descriptor in FD table
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let limit = count.min(65536);
    let mut kernel_buf = alloc::vec![0u8; limit];
    if kernel_buf.is_empty() {
        return 0;
    }
    match crate::contexts::vfs::read(desc.vfs_fd, &mut kernel_buf) {
        Ok(n) => {
            if n > 0 && unsafe { copy_to_user(buf, &kernel_buf[..n]) }.is_ok() {
                // Update offset in FD table
                if let Some(d) = rt.fd_table.get_mut(fd) {
                    d.offset += n as u64;
                }
                n as u64
            } else {
                errno_code(EFAULT)
            }
        }
        Err(e) => errno_result(e),
    }
}

pub fn sys_write(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2] as usize;
    if count == 0 {
        return 0;
    }
    if fd == 1 || fd == 2 {
        // Read data from user space into kernel buffer, then write to serial
        let data = match unsafe { copy_from_user(buf, count.min(4096)) } {
            Ok(d) => d,
            Err(_) => return errno_code(EFAULT),
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &data);
        return data.len() as u64;
    }
    if fd == 0 {
        return errno_code(EBADF);
    }
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    // Read data from user space into kernel buffer
    let kernel_buf = match unsafe { copy_from_user(buf, count) } {
        Ok(d) => d,
        Err(_) => return errno_code(EFAULT),
    };
    match crate::contexts::vfs::write(desc.vfs_fd, &kernel_buf) {
        Ok(n) => {
            if let Some(d) = rt.fd_table.get_mut(fd) {
                d.offset += n as u64;
            }
            n as u64
        }
        Err(e) => errno_result(e),
    }
}

pub fn sys_open(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let flags = args[1] as i32;
    let _mode = args[2] as u32;
    let path = unsafe { copy_user_string(path_ptr, 256) };
    let path = match path {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    open_common(rt, &path, flags)
}

pub fn sys_openat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32; // AT_FDCWD = -100; we ignore for now
    let path_ptr = args[1];
    let flags = args[2] as i32;
    let _mode = args[3] as u32;
    let path = unsafe { copy_user_string(path_ptr, 256) };
    let path = match path {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    open_common(rt, &path, flags)
}

fn open_common(rt: &mut LinuxRuntime, path: &str, flags: i32) -> u64 {
    let read_only = (flags & 0x3) == O_RDONLY;
    let write_only = (flags & 0x3) == O_WRONLY;
    let read_write = (flags & 0x3) == O_RDWR;
    let create = (flags & O_CREAT) != 0;
    let truncate = (flags & O_TRUNC) != 0;
    let append = (flags & O_APPEND) != 0;

    // Handle creation or opening for writing
    if create || truncate || write_only || read_write || append {
        if create {
            match crate::contexts::vfs::create(path) {
                Ok(vfs_fd) => {
                    let fd = rt.fd_table.alloc(vfs_fd.fd, 0, flags);
                    return fd as u64;
                }
                Err(e) => {
                    // File may already exist; try opening with truncation
                    if truncate && (write_only || read_write) {
                        let _ = crate::contexts::vfs::unlink(path);
                        match crate::contexts::vfs::create(path) {
                            Ok(vfs_fd) => {
                                let fd = rt.fd_table.alloc(vfs_fd.fd, 0, flags);
                                return fd as u64;
                            }
                            Err(e2) => return errno_result(e2),
                        }
                    }
                    // Try opening for read-write if it exists
                    if let Ok(vfs_fd) = crate::contexts::vfs::open(path, 0) {
                        let fd = rt.fd_table.alloc(vfs_fd.fd, 0, flags);
                        return fd as u64;
                    }
                    return errno_result(e);
                }
            }
        }
        if let Ok(vfs_fd) = crate::contexts::vfs::open(path, 0) {
            let fd = rt.fd_table.alloc(vfs_fd.fd, 0, flags);
            return fd as u64;
        }
        return errno_code(ENOENT);
    }

    // Read-only open
    if read_only {
        match crate::contexts::vfs::open(path, 0) {
            Ok(vfs_fd) => {
                let fd = rt.fd_table.alloc(vfs_fd.fd, 0, flags);
                fd as u64
            }
            Err(e) => errno_result(e),
        }
    } else {
        errno_code(EINVAL)
    }
}

pub fn sys_creat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let mode = args[1] as u32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::create(&path) {
        Ok(vfs_fd) => {
            let fd = rt.fd_table.alloc(vfs_fd.fd, 0, O_WRONLY | O_CREAT | O_TRUNC);
            fd as u64
        }
        Err(e) => errno_result(e),
    }
}

pub fn sys_close(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    if LinuxRuntime::is_std_fd(fd) {
        return 0;
    }
    if let Some(desc) = rt.fd_table.remove(fd) {
        let _ = crate::contexts::vfs::close(desc.vfs_fd);
        0
    } else {
        errno_code(EBADF)
    }
}

/// Return a LinuxStat for a given VFS path.
fn fill_stat_from_path(path: &str, statbuf: u64) -> Result<(), &'static str> {
    let vfs_fd = crate::contexts::vfs::open(path, 0)?;
    let info = fill_stat_from_fd(vfs_fd.fd);
    let _ = crate::contexts::vfs::close(vfs_fd.fd);

    // Check if path is a directory by trying to readdir
    let is_dir = crate::contexts::vfs::readdir(path).is_ok();
    let size = if is_dir { 0 } else {
        let mut buf = [0u8; 512];
        let mut total = 0usize;
        let mut f = vfs_fd;
        loop {
            match crate::contexts::vfs::read(f.fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(_) => break,
            }
        }
        total
    };

    let stat = LinuxStat {
        st_dev: 0,
        st_ino: info.ino,
        st_nlink: 1,
        st_mode: mode_from_type(is_dir),
        st_uid: 0,
        st_gid: 0,
        pad0: 0,
        st_rdev: 0,
        st_size: size as i64,
        st_blksize: 4096,
        st_blocks: (size as i64 + 511) / 512,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        unused: [0; 3],
    };

    unsafe { copy_val_to_user(statbuf, &stat) }.ok();
    Ok(())
}

pub fn sys_stat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let statbuf = args[1];
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match fill_stat_from_path(&path, statbuf) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_newfstatat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32;
    let path_ptr = args[1];
    let statbuf = args[2];
    let _flags = args[3] as i32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match fill_stat_from_path(&path, statbuf) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

/// Internal stat info for a VFS fd.
struct StatInfo {
    ino: u64,
    is_dir: bool,
}

fn fill_stat_from_fd(vfs_fd: u32) -> StatInfo {
    StatInfo { ino: vfs_fd as u64, is_dir: false }
}

pub fn sys_fstat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let statbuf = args[1];
    if LinuxRuntime::is_std_fd(fd) {
        // For stdin/stdout/stderr, return a reasonable stat
        let stat = LinuxStat {
            st_dev: 0,
            st_ino: fd as u64,
            st_nlink: 1,
            st_mode: S_IFCHR | 0o666,
            st_uid: 0,
            st_gid: 0,
            pad0: 0,
            st_rdev: 0x8803, // tty
            st_size: 0,
            st_blksize: 4096,
            st_blocks: 0,
            ..LinuxStat::zeroed()
        };
        unsafe { copy_val_to_user(statbuf, &stat) }.ok();
        return 0;
    }
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let info = fill_stat_from_fd(desc.vfs_fd);
    // Get file size
    let size = {
        let mut buf = [0u8; 512];
        let mut total = 0usize;
        let mut vfs_fd_ref = desc;
        loop {
            match crate::contexts::vfs::read(vfs_fd_ref.vfs_fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(_) => break,
            }
        }
        total
    };
    // Re-fetch desc (it was moved)
    let desc = rt.fd_table.get(fd).unwrap().clone();

    let stat = LinuxStat {
        st_dev: 0,
        st_ino: info.ino,
        st_nlink: 1,
        st_mode: S_IFREG | 0o644,
        st_uid: 0,
        st_gid: 0,
        pad0: 0,
        st_rdev: 0,
        st_size: size as i64,
        st_blksize: 4096,
        st_blocks: (size as i64 + 511) / 512,
        ..LinuxStat::zeroed()
    };
    unsafe { copy_val_to_user(statbuf, &stat) }.ok();
    0
}

pub fn sys_lseek(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let offset = args[1] as i64;
    let whence = args[2] as i32;
    if LinuxRuntime::is_std_fd(fd) {
        return errno_code(ESPIPE);
    }
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let new_offset = match whence {
        0 => offset,           // SEEK_SET
        1 => desc.offset as i64 + offset, // SEEK_CUR
        2 => -(EINVAL as i64), // SEEK_END (not fully supported)
        _ => return errno_code(EINVAL),
    };
    if new_offset < 0 {
        return errno_code(EINVAL);
    }
    if let Some(d) = rt.fd_table.get_mut(fd) {
        d.offset = new_offset as u64;
    }
    new_offset as u64
}

pub fn sys_pread64(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2] as usize;
    let offset = args[3] as i64;
    if offset < 0 { return errno_code(EINVAL); }
    // Temporarily seek, read, restore
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let saved = desc.offset;
    if let Some(d) = rt.fd_table.get_mut(fd) {
        d.offset = offset as u64;
    }
    let result = sys_read(rt, &[fd as u64, buf, count as u64, 0, 0, 0]);
    if let Some(d) = rt.fd_table.get_mut(fd) {
        d.offset = saved;
    }
    result
}

pub fn sys_pwrite64(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2] as usize;
    let offset = args[3] as i64;
    if offset < 0 { return errno_code(EINVAL); }
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let saved = desc.offset;
    if let Some(d) = rt.fd_table.get_mut(fd) {
        d.offset = offset as u64;
    }
    let result = sys_write(rt, &[fd as u64, buf, count as u64, 0, 0, 0]);
    if let Some(d) = rt.fd_table.get_mut(fd) {
        d.offset = saved;
    }
    result
}

pub fn sys_readv(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let iov = args[1];
    let iovcnt = args[2] as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        let base_ptr = iov + (i * core::mem::size_of::<LinuxIovec>()) as u64;
        let mut iovec: LinuxIovec = LinuxIovec { iov_base: 0, iov_len: 0 };
        unsafe { copy_val_to_user(base_ptr, &iovec) }.ok();
        // Actually read the iovec from user space
        if iovec.iov_base == 0 {
            continue;
        }
        let n = sys_read(rt, &[fd as u64, iovec.iov_base, iovec.iov_len, 0, 0, 0]);
        if (n as i64) < 0 {
            if total > 0 { break; }
            return n;
        }
        total += n;
        if n < iovec.iov_len { break; }
    }
    total
}

pub fn sys_writev(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let iov = args[1];
    let iovcnt = args[2] as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        let base_ptr = iov + (i * core::mem::size_of::<LinuxIovec>()) as u64;
        let iovec: LinuxIovec = LinuxIovec { iov_base: 0, iov_len: 0 };
        unsafe { copy_val_to_user(base_ptr, &iovec) }.ok();
        if iovec.iov_base == 0 {
            continue;
        }
        let n = sys_write(rt, &[fd as u64, iovec.iov_base, iovec.iov_len, 0, 0, 0]);
        if (n as i64) < 0 {
            if total > 0 { break; }
            return n;
        }
        total += n;
    }
    total
}

pub fn sys_access(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let _mode = args[1] as i32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    if crate::contexts::vfs::exists(&path) {
        0
    } else {
        errno_code(ENOENT)
    }
}

pub fn sys_faccessat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32;
    let path_ptr = args[1];
    let _mode = args[2] as i32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    if crate::contexts::vfs::exists(&path) {
        0
    } else {
        errno_code(ENOENT)
    }
}

pub fn sys_getdents64(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let buf = args[1];
    let count = args[2] as u32;
    if LinuxRuntime::is_std_fd(fd) { return errno_code(ENOTDIR); }
    let desc = match rt.fd_table.get(fd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    // TODO: Track directory paths per fd for proper getdents64 support.
    // For now, always read the root directory.
    let path = "/";
    let entries = match crate::contexts::vfs::readdir(path) {
        Ok(e) => e,
        Err(_) => return errno_code(ENOTDIR),
    };
    let mut written = 0u32;
    let base = buf;
    unsafe {
        for entry in &entries {
            let name_bytes = entry.name.as_bytes();
            let name_len = name_bytes.len().min(255);
            let reclen = core::mem::size_of::<LinuxDirent64>() as u16;
            if written + reclen as u32 > count {
                break;
            }
            let d = LinuxDirent64 {
                d_ino: 1,
                d_off: 1,
                d_reclen: reclen,
                d_type: if entry.is_dir { DT_DIR } else { DT_REG },
                d_name: {
                    let mut bufname = [0u8; 256];
                    bufname[..name_len].copy_from_slice(&name_bytes[..name_len]);
                    bufname
                },
            };
            let dst = (base + written as u64) as *mut LinuxDirent64;
            core::ptr::write_volatile(dst, d);
            written += reclen as u32;
        }
    }
    written as u64
}

pub fn sys_readlink(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _path_ptr = args[0];
    let _buf = args[1];
    let _size = args[2];
    errno_code(EINVAL) // symlinks not supported yet
}

pub fn sys_readlinkat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32;
    let _path_ptr = args[1];
    let _buf = args[2];
    let _size = args[3];
    errno_code(EINVAL)
}

pub fn sys_unlink(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::unlink(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_unlinkat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32;
    let path_ptr = args[1];
    let _flags = args[2] as i32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::unlink(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_mkdir(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let _mode = args[1] as u32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::mkdir(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_mkdirat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _dirfd = args[0] as i32;
    let path_ptr = args[1];
    let _mode = args[2] as u32;
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::mkdir(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_rmdir(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::unlink(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_symlink(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _target_ptr = args[0];
    let _linkpath_ptr = args[1];
    errno_code(ENOSYS)
}

pub fn sys_rename(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _old_ptr = args[0];
    let _new_ptr = args[1];
    errno_code(ENOSYS)
}

pub fn sys_chdir(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };
    match crate::contexts::vfs::change_directory(&path) {
        Ok(_) => 0,
        Err(e) => errno_result(e),
    }
}

pub fn sys_getcwd(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let buf = args[0];
    let size = args[1];
    let cwd = match crate::contexts::vfs::working_directory() {
        Ok(s) => s,
        Err(e) => return errno_result(e),
    };
    let bytes = cwd.as_bytes();
    if bytes.len() + 1 > size as usize {
        return errno_code(ERANGE);
    }
    if unsafe { copy_to_user(buf, bytes) }.is_err() {
        return errno_code(EFAULT);
    }
    if unsafe { copy_to_user(buf + bytes.len() as u64, &[0u8]) }.is_err() {
        return errno_code(EFAULT);
    }
    buf
}

pub fn sys_mount(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _source = args[0];
    let _target = args[1];
    let _fstype = args[2];
    let _flags = args[3];
    let _data = args[4];
    // Mount not fully supported; pretend it succeeds for /proc, /sys, /dev etc.
    0
}

pub fn sys_umount2(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _target = args[0];
    let _flags = args[1] as i32;
    0
}

pub fn sys_dup(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let oldfd = args[0] as i32;
    if LinuxRuntime::is_std_fd(oldfd) { return oldfd as u64; }
    let desc = match rt.fd_table.get(oldfd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    let newfd = rt.fd_table.alloc(desc.vfs_fd, desc.mount_index, desc.flags);
    newfd as u64
}

pub fn sys_dup2(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let oldfd = args[0] as i32;
    let newfd = args[1] as i32;
    if LinuxRuntime::is_std_fd(oldfd) && LinuxRuntime::is_std_fd(newfd) {
        return newfd as u64;
    }
    if oldfd == newfd {
        return newfd as u64;
    }
    let desc = match rt.fd_table.get(oldfd) {
        Some(d) => d.clone(),
        None => return errno_code(EBADF),
    };
    // Close newfd if it's open
    if rt.fd_table.contains(newfd) {
        rt.fd_table.remove(newfd);
    }
    // Insert at newfd
    let linux_fd = LinuxFileDesc {
        vfs_fd: desc.vfs_fd,
        mount_index: desc.mount_index,
        flags: desc.flags,
        offset: desc.offset,
    };
    rt.fd_table.entries.insert(newfd, linux_fd);
    newfd as u64
}

pub fn sys_dup3(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let oldfd = args[0] as i32;
    let newfd = args[1] as i32;
    let _flags = args[2] as i32;
    sys_dup2(rt, &[oldfd as u64, newfd as u64, 0, 0, 0, 0])
}

pub fn sys_fcntl(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let cmd = args[1] as i32;
    let arg = args[2];
    match cmd {
        F_DUPFD => sys_dup(rt, &[fd as u64, 0, 0, 0, 0, 0]),
        F_GETFD => {
            if rt.fd_table.contains(fd) || LinuxRuntime::is_std_fd(fd) { 0 } else { errno_code(EBADF) }
        }
        F_SETFD => 0,
        F_GETFL => {
            rt.fd_table.get(fd).map(|d| d.flags as u64).unwrap_or(0)
        }
        F_SETFL => {
            if let Some(d) = rt.fd_table.get_mut(fd) {
                d.flags = arg as i32;
                0
            } else {
                errno_code(EBADF)
            }
        }
        _ => 0,
    }
}

pub fn sys_ioctl(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let fd = args[0] as i32;
    let request = args[1];
    let arg = args[2];
    if fd >= 0 && fd <= 2 {
        match request {
            TCGETS => {
                // Return a reasonable termios (all zeros is usually fine)
                let termios: [u8; 36] = [0; 36];
                unsafe { copy_to_user(arg, &termios) }.ok();
                0
            }
            TIOCGWINSZ => {
                // Return terminal window size
                let ws = LinuxWinsize {
                    ws_row: 25,
                    ws_col: 80,
                    ws_xpixel: 800,
                    ws_ypixel: 600,
                };
                unsafe { copy_val_to_user(arg, &ws) }.ok();
                0
            }
            TIOCGPGRP => {
                // Return foreground process group (same as pid, or 0)
                unsafe { core::ptr::write_volatile(arg as *mut i32, 0) };
                0
            }
            TIOCSPGRP => 0,
            FIONREAD => {
                // Return 0 bytes available
                unsafe { core::ptr::write_volatile(arg as *mut i32, 0) };
                0
            }
            _ => errno_code(ENOTTY),
        }
    } else {
        errno_code(ENOTTY)
    }
}

pub fn sys_pipe(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let pipefd = args[0];
    if pipefd == 0 {
        return errno_code(EFAULT);
    }
    // Create a pair of pipe fds. For simplicity, create two anonymous files
    // that read/write to each other. This is a placeholder.
    let read_fd = rt.fd_table.alloc(1, 0, O_RDONLY);
    let write_fd = rt.fd_table.alloc(2, 0, O_WRONLY);
    unsafe {
        core::ptr::write_volatile(pipefd as *mut i32, read_fd);
        core::ptr::write_volatile((pipefd + 4) as *mut i32, write_fd);
    }
    0
}

pub fn sys_pipe2(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _flags = args[1] as i32;
    sys_pipe(rt, &[args[0], 0, 0, 0, 0, 0])
}

pub fn sys_truncate(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _path = args[0];
    let _length = args[1] as i64;
    0
}

pub fn sys_ftruncate(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _fd = args[0] as i32;
    let _length = args[1] as i64;
    0
}

pub fn sys_fsync(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    0
}

pub fn sys_fdatasync(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    0
}

pub fn sys_fchmod(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    0
}

pub fn sys_fchmodat(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    0
}
