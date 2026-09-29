//! macOS-specific fast directory scanner using the getattrlistbulk syscall.
//!
//! This retrieves multiple directory entries with their attributes in a single
//! system call, avoiding per-file vnode creation in the kernel. On APFS/SSD
//! this is the fastest available scanning method.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const SCAN_BUF_SIZE: usize = 256 * 1024; // 256KB — fewer syscalls for large dirs

thread_local! {
    static SCAN_BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0u8; SCAN_BUF_SIZE]);
}

// --- macOS constants not in libc ---

const ATTR_BIT_MAP_COUNT: libc::c_ushort = 5;

// commonattr bits (from sys/attr.h)
const ATTR_CMN_RETURNED_ATTRS: libc::attrgroup_t = 0x8000_0000;
const ATTR_CMN_NAME: libc::attrgroup_t = 0x0000_0001;
const ATTR_CMN_OBJTYPE: libc::attrgroup_t = 0x0000_0008;

// dirattr bits
const ATTR_DIR_ENTRYCOUNT: libc::attrgroup_t = 0x0000_0002;

// fileattr bits
const ATTR_FILE_TOTALSIZE: libc::attrgroup_t = 0x0000_0002;

// Object types
const VDIR: u32 = 2; // directory

/// A directory entry with name, type, and size.
pub(crate) struct DirEntry {
    pub name: Box<str>,
    pub is_dir: bool,
    pub file_size: u64,
    /// Number of immediate entries (directories only; 0 if unknown).
    /// Used as a cheap weight for progress estimation.
    pub entry_count: u32,
}

/// A mounted file system.
pub(crate) struct MountPoint {
    pub path: std::path::PathBuf,
    /// Hidden from Finder (MNT_DONTBROWSE), e.g. system and virtual volumes.
    pub dont_browse: bool,
    pub local: bool,
}

/// List mounted file systems without blocking on unresponsive network mounts.
pub(crate) fn mount_points() -> Vec<MountPoint> {
    use std::os::unix::ffi::OsStrExt;

    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    // Leave headroom in case mounts appear between the two calls.
    let cap = count as usize + 16;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(cap);
    let bytes = (cap * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    unsafe { buf.set_len(n as usize) };

    buf.iter()
        .map(|fs| {
            let name = unsafe { std::ffi::CStr::from_ptr(fs.f_mntonname.as_ptr()) };
            let flags = fs.f_flags as libc::c_int;
            MountPoint {
                path: std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())),
                dont_browse: flags & libc::MNT_DONTBROWSE != 0,
                local: flags & libc::MNT_LOCAL != 0,
            }
        })
        .collect()
}

/// Open a directory and return its fd, or -1 on error.
pub(crate) fn open_dir(dir_path: &Path) -> libc::c_int {
    let c_path = match CString::new(dir_path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return -1,
    };
    unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) }
}

/// Open a subdirectory relative to a parent fd.
/// Uses stack buffer for short names to avoid heap allocation.
pub(crate) fn openat_dir(parent_fd: libc::c_int, name: &str) -> libc::c_int {
    let bytes = name.as_bytes();
    if bytes.contains(&0) {
        return -1;
    }
    // Stack buffer for names up to 255 bytes (covers nearly all filenames)
    if bytes.len() < 256 {
        let mut buf = [0u8; 256];
        buf[..bytes.len()].copy_from_slice(bytes);
        // buf[bytes.len()] is already 0 (null terminator)
        unsafe {
            libc::openat(
                parent_fd,
                buf.as_ptr() as *const libc::c_char,
                libc::O_RDONLY | libc::O_DIRECTORY,
            )
        }
    } else {
        let c_name = match CString::new(bytes) {
            Ok(p) => p,
            Err(_) => return -1,
        };
        unsafe {
            libc::openat(
                parent_fd,
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )
        }
    }
}

/// Close a directory fd.
pub(crate) fn close_dir(fd: libc::c_int) {
    if fd >= 0 {
        unsafe { libc::close(fd) };
    }
}

/// Scan using an already-opened fd. Returns entries without closing the fd.
pub(crate) fn scan_dir_entries_fd(fd: libc::c_int) -> Vec<DirEntry> {
    if fd < 0 {
        return Vec::new();
    }

    let mut attrlist: libc::attrlist = unsafe { std::mem::zeroed() };
    attrlist.bitmapcount = ATTR_BIT_MAP_COUNT;
    attrlist.commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE;
    attrlist.dirattr = ATTR_DIR_ENTRYCOUNT;
    attrlist.fileattr = ATTR_FILE_TOTALSIZE;

    let mut results = Vec::new();

    SCAN_BUFFER.with(|buf| {
        let mut buffer = buf.borrow_mut();

        loop {
            let count = unsafe {
                libc::getattrlistbulk(
                    fd,
                    &attrlist as *const libc::attrlist as *mut libc::c_void,
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                    0,
                )
            };

            if count <= 0 {
                break;
            }

            parse_dir_entries(&buffer, count as usize, &mut results);
        }
    }); // end SCAN_BUFFER.with

    results
}

/// Read a native-endian u32 from `buf` at `offset`. Returns 0 if out of bounds.
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    buf.get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_ne_bytes)
        .unwrap_or(0)
}

/// Read a native-endian i32 from `buf` at `offset`. Returns 0 if out of bounds.
fn read_i32(buf: &[u8], offset: usize) -> i32 {
    buf.get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .map(i32::from_ne_bytes)
        .unwrap_or(0)
}

/// Read a native-endian u64 from `buf` at `offset`. Returns 0 if out of bounds.
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    buf.get(offset..offset + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_ne_bytes)
        .unwrap_or(0)
}

/// Parse buffer entries into DirEntry directly (no intermediate struct).
fn parse_dir_entries(buffer: &[u8], count: usize, results: &mut Vec<DirEntry>) {
    let buf_size = buffer.len();
    let mut offset = 0usize;
    for _ in 0..count {
        if offset + 4 > buf_size {
            break;
        }
        let entry_length = read_u32(buffer, offset) as usize;
        let entry_start = offset;
        if entry_length == 0 || entry_start + entry_length > buf_size {
            break;
        }

        let pos = entry_start + 4;
        if pos + 20 > entry_start + entry_length {
            offset += entry_length;
            continue;
        }
        let returned_dirattr = read_u32(buffer, pos + 8);
        let returned_fileattr = read_u32(buffer, pos + 12);
        let entry_end = entry_start + entry_length;

        let name_ref_pos = pos + 20;
        if name_ref_pos + 8 > entry_end {
            offset += entry_length;
            continue;
        }
        let name_data_offset = read_i32(buffer, name_ref_pos);
        // Safe cast: check for overflow before converting to usize
        let Some(name_abs) = (name_ref_pos as i32)
            .checked_add(name_data_offset)
            .and_then(|v| usize::try_from(v).ok())
        else {
            offset += entry_length;
            continue;
        };

        let name: Box<str> = if name_abs < entry_end {
            let slice = &buffer[name_abs..entry_end];
            let bytes = match slice.iter().position(|&b| b == 0) {
                Some(n) => &slice[..n],
                None => slice,
            };
            // Fast path: valid UTF-8 needs a single allocation.
            match std::str::from_utf8(bytes) {
                Ok(s) => s.into(),
                Err(_) => String::from_utf8_lossy(bytes).into(),
            }
        } else {
            offset += entry_length;
            continue;
        };

        // Attributes are packed in order: common, dir, file — only returned ones.
        let obj_type_pos = name_ref_pos + 8;
        let obj_type = read_u32(buffer, obj_type_pos);
        let mut attr_pos = obj_type_pos + 4;

        let entry_count = if returned_dirattr & ATTR_DIR_ENTRYCOUNT != 0 {
            let v = read_u32(buffer, attr_pos);
            attr_pos += 4;
            v
        } else {
            0
        };

        let file_size = if returned_fileattr & ATTR_FILE_TOTALSIZE != 0 {
            read_u64(buffer, attr_pos)
        } else {
            0
        };

        results.push(DirEntry {
            name,
            is_dir: obj_type == VDIR,
            file_size,
            entry_count,
        });
        offset += entry_length;
    }
}
