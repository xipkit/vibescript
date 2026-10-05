use super::Dir;
use std::{
    fs::OpenOptions,
    io,
    mem::MaybeUninit,
    os::windows::{fs::MetadataExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo,
    GetFileInformationByHandleEx,
};

pub(super) fn absolute_target<'a>(
    root: &Dir,
    root_path: &Path,
    target: &'a Path,
) -> io::Result<&'a Path> {
    let confined = || io::Error::from(io::ErrorKind::InvalidData);
    let root_components = root_path.components().count();
    let remainder = target
        .components()
        .count()
        .checked_sub(root_components)
        .ok_or_else(confined)?;
    let prefix = target.ancestors().nth(remainder).ok_or_else(confined)?;
    // Only this metadata query uses an ambient path. Contents are opened from
    // the retained root after matching directory identity, including on
    // filesystems with case-sensitive directories.
    let directory = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(prefix)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || identity(&directory)? != identity(root)?
    {
        return Err(confined());
    }
    let mut relative = target.components();
    for _ in 0..root_components {
        relative.next();
    }
    Ok(relative.as_path())
}

fn identity(handle: &impl AsRawHandle) -> io::Result<(u64, [u8; 16])> {
    let mut info = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the live handle remains borrowed throughout the call, and the
    // output buffer has the size and alignment required by FileIdInfo.
    if unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful FileIdInfo call initialized the complete structure.
    let info = unsafe { info.assume_init() };
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}
