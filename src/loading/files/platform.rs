#[cfg(target_os = "wasi")]
pub(super) use super::wasi::{Dir, ReadDir};
use crate::{CallContext, Result};
#[cfg(target_vendor = "apple")]
use crate::{Error, ErrorKind};
#[cfg(not(target_os = "wasi"))]
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
#[cfg(not(target_os = "wasi"))]
pub(super) use cap_std::fs::{Dir, ReadDir};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[cfg(windows)]
mod windows;

#[cfg(all(unix, not(target_os = "wasi")))]
mod unix;

pub(super) fn absolute_target<'a>(
    ctx: &mut CallContext,
    root: &Dir,
    root_path: &Path,
    target: &'a Path,
) -> Result<&'a Path> {
    #[cfg(all(unix, not(target_os = "wasi")))]
    return unix::absolute_target(ctx, root, root_path, target);
    #[cfg(not(all(unix, not(target_os = "wasi"))))]
    {
        #[cfg(windows)]
        let relative = windows::absolute_target(root, root_path, target);
        #[cfg(not(windows))]
        let relative = {
            let _ = root;
            target.strip_prefix(root_path).map_err(|_| ())
        };
        ctx.checkpoint()?;
        relative.map_err(|_| super::root::escape())
    }
}

#[cfg(unix)]
pub(super) const PATH_WORKSPACE: usize = libc::PATH_MAX as usize;
// wasi-libc's PATH_MAX, which the libc crate does not export for WASI.
#[cfg(target_os = "wasi")]
pub(super) const PATH_WORKSPACE: usize = 4096;
#[cfg(not(any(unix, target_os = "wasi")))]
pub(super) const PATH_WORKSPACE: usize = 131072;

#[cfg(not(target_os = "wasi"))]
pub(super) fn open(parent: &Dir, name: &OsStr) -> io::Result<File> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    parent
        .open_with(name, &options)
        .map(cap_std::fs::File::into_std)
}

#[cfg(target_vendor = "apple")]
pub(super) fn stored_name(
    ctx: &mut CallContext,
    parent: &Dir,
    expected: &OsStr,
) -> Result<Option<bool>> {
    use crate::budget::Buffer;
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let bytes = expected.as_bytes();
    ctx.charge((bytes.len() as u64).saturating_add(1))?;
    if bytes.contains(&0) {
        return Err(Error::new(
            ErrorKind::Runtime,
            "require: module path contains a NUL byte",
        ));
    }
    let Some(capacity) = bytes.len().checked_add(1) else {
        return ctx.fail(ErrorKind::Memory, "module path size overflow");
    };
    let mut c_path = Buffer::with_capacity(ctx, capacity)?;
    c_path.extend(ctx, bytes)?;
    c_path.push(ctx, 0)?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_NAME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut output = [0u8; 1024];
    ctx.checkpoint()?;
    // SAFETY: parent is an open directory and the single filename has one
    // terminating NUL and no interior NUL. The buffers have their declared sizes.
    // NOFOLLOW prevents a replaced entry from redirecting this metadata query.
    let result = unsafe {
        libc::getattrlistat(
            parent.as_raw_fd(),
            c_path.data.as_ptr().cast(),
            (&raw mut attributes).cast(),
            output.as_mut_ptr().cast(),
            output.len(),
            u64::from(libc::FSOPT_NOFOLLOW),
        )
    };
    let error = if result != 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    ctx.checkpoint()?;
    if let Some(error) = error {
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(Some(false));
        }
        if matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::ENOSYS)) {
            return Ok(None);
        }
        return Err(super::io_error("checking module filename", error));
    }
    let total = u32::from_ne_bytes([output[0], output[1], output[2], output[3]]) as usize;
    // A negative offset becomes 0, which the check below refuses.
    let offset = usize::try_from(
        i64::from(i32::from_ne_bytes([
            output[4], output[5], output[6], output[7],
        ])) + 4,
    )
    .unwrap_or(0);
    let length = u32::from_ne_bytes([output[8], output[9], output[10], output[11]]) as usize;
    let invalid_response = || {
        Error::new(
            ErrorKind::Runtime,
            "require: invalid stored filename response",
        )
    };
    if !(12..=output.len()).contains(&total) || offset < 12 {
        return Err(invalid_response());
    }
    let name = output
        .get(offset..total)
        .and_then(|tail| tail.get(..length))
        .and_then(|name| name.strip_suffix(&[0]))
        .ok_or_else(invalid_response)?;
    Ok(Some(name == expected.as_bytes()))
}

#[cfg(not(target_vendor = "apple"))]
pub(super) fn stored_name(_: &mut CallContext, _: &Dir, _: &OsStr) -> Result<Option<bool>> {
    Ok(None)
}

#[cfg(not(target_os = "wasi"))]
pub(super) fn open_root(path: &Path) -> io::Result<(Dir, PathBuf)> {
    let path = std::fs::canonicalize(path)?;
    let directory = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
    Ok((directory, path))
}

#[cfg(target_os = "wasi")]
pub(super) fn open_root(path: &Path) -> io::Result<(Dir, PathBuf)> {
    super::wasi::open_root(path)
}

#[cfg(target_os = "wasi")]
pub(super) fn open(parent: &Dir, name: &OsStr) -> io::Result<File> {
    parent.open_file(name)
}
