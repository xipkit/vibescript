use crate::{CallContext, Result};
#[cfg(target_vendor = "apple")]
use crate::{Error, ErrorKind};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::Dir;
use std::{ffi::OsStr, fs::File, io};

#[cfg(unix)]
pub(super) const PATH_WORKSPACE: usize = libc::PATH_MAX as usize;
#[cfg(not(unix))]
pub(super) const PATH_WORKSPACE: usize = 131072;

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
    let total = u32::from_ne_bytes(output[..4].try_into().unwrap()) as usize;
    let offset = i32::from_ne_bytes(output[4..8].try_into().unwrap()) as i64 + 4;
    let length = u32::from_ne_bytes(output[8..12].try_into().unwrap()) as usize;
    if !(12..=output.len()).contains(&total)
        || offset < 12
        || offset as usize > total
        || length == 0
        || length > total - offset as usize
        || output[offset as usize + length - 1] != 0
    {
        return Err(Error::new(
            ErrorKind::Runtime,
            "require: invalid stored filename response",
        ));
    }
    Ok(Some(
        &output[offset as usize..offset as usize + length - 1] == expected.as_bytes(),
    ))
}

#[cfg(not(target_vendor = "apple"))]
pub(super) fn stored_name(_: &mut CallContext, _: &Dir, _: &OsStr) -> Result<Option<bool>> {
    Ok(None)
}
