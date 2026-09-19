use super::Dir;
use rustix::{
    fd::BorrowedFd,
    fs::{Mode, OFlags},
};
use std::{
    collections::VecDeque,
    ffi::{CStr, CString, OsString},
    io,
    os::wasi::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

pub(super) fn open(path: &Path) -> io::Result<(Dir, PathBuf)> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    }
    let (directory, mut resolved, tail) = preopen(path)?;
    let mut directories = vec![directory];
    let mut pending = VecDeque::new();
    prepend(&mut pending, &tail);
    let mut links = 0;
    while let Some(name) = pending.pop_front() {
        if name == "." {
            continue;
        }
        if name == ".." {
            if directories.len() == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "module root escapes its WASI preopen",
                ));
            }
            directories.pop();
            resolved.pop();
            continue;
        }
        let parent = directories.last().unwrap();
        let metadata = parent.symlink_metadata(&name)?;
        if metadata.file_type().is_symlink() {
            if links == 40 {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
            links += 1;
            let mut target = parent.read_link_contents(&name)?;
            if target.is_absolute() {
                let (directory, path, tail) = preopen(&target)?;
                directories = vec![directory];
                resolved = path;
                target = tail;
            }
            prepend(&mut pending, &target);
        } else if metadata.is_dir() {
            let directory = parent.open_dir_nofollow(&name)?;
            directories.push(directory);
            resolved.push(&name);
        } else {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
    }
    Ok((directories.pop().unwrap(), resolved))
}

fn prepend(pending: &mut VecDeque<OsString>, path: &Path) {
    for part in path.components().rev() {
        pending.push_front(part.as_os_str().to_owned());
    }
}

fn preopen(path: &Path) -> io::Result<(Dir, PathBuf, PathBuf)> {
    let mut normalized: PathBuf = path
        .components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect();
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    let path = CString::new(normalized.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "module root contains a NUL byte",
        )
    })?;
    let mut storage = vec![0_u8; 512];
    loop {
        let mut prefix = std::ptr::null();
        let mut relative = storage.as_mut_ptr().cast();
        // SAFETY: path is terminated and storage is writable for its given
        // length. Both output pointers remain valid through the copies below.
        let fd = unsafe {
            __wasilibc_find_relpath(path.as_ptr(), &mut prefix, &mut relative, storage.len())
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ERANGE)
                && storage.len() < super::super::platform::PATH_WORKSPACE
            {
                storage.resize(
                    (storage.len() * 2).min(super::super::platform::PATH_WORKSPACE),
                    0,
                );
                continue;
            }
            return Err(error);
        }
        // SAFETY: success returns terminated strings in libc's preopen table,
        // path, or storage. Copy them while all of that storage remains live.
        let (prefix, relative) = unsafe {
            (
                CStr::from_ptr(prefix).to_bytes().to_vec(),
                CStr::from_ptr(relative).to_bytes().to_vec(),
            )
        };
        // SAFETY: the returned descriptor is borrowed from wasi-libc's preopen
        // table. Open a new owned handle without closing or retaining the borrow.
        let preopened = unsafe { BorrowedFd::borrow_raw(fd) };
        let directory = rustix::fs::openat(
            preopened,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        return Ok((
            Dir(directory),
            Path::new("/").join(OsString::from_vec(prefix)),
            PathBuf::from(OsString::from_vec(relative)),
        ));
    }
}

// WASI preopen labels can have virtual ancestors. Start at the directory
// supplied by wasi-libc's public path-mapping API before resolving real links.
// SAFETY: This declaration matches wasi-libc's public libc-find-relpath.h ABI.
unsafe extern "C" {
    fn __wasilibc_find_relpath(
        path: *const libc::c_char,
        abs_prefix: *mut *const libc::c_char,
        relative_path: *mut *mut libc::c_char,
        relative_path_len: libc::size_t,
    ) -> libc::c_int;
}
