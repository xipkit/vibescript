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
    absolute_target_with(root_path, target, |prefix| {
        // Only this metadata query uses an ambient path. Contents are opened
        // from the retained root after matching directory identity.
        let directory = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(prefix)?;
        let metadata = directory.metadata()?;
        Ok(metadata.is_dir()
            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && identity(&directory)? == identity(root)?)
    })
}

fn absolute_target_with<'a>(
    root_path: &Path,
    target: &'a Path,
    matches_root: impl FnOnce(&Path) -> io::Result<bool>,
) -> io::Result<&'a Path> {
    // Exact component prefixes need no filesystem identity. The retained-root
    // walk still confines the suffix on providers without unique file IDs.
    if let Ok(relative) = target.strip_prefix(root_path) {
        return Ok(relative);
    }
    let confined = || io::Error::from(io::ErrorKind::InvalidData);
    let root_components = root_path.components().count();
    let remainder = target
        .components()
        .count()
        .checked_sub(root_components)
        .ok_or_else(confined)?;
    let prefix = target.ancestors().nth(remainder).ok_or_else(confined)?;
    if !matches_root(prefix)? {
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
    checked_identity(info.VolumeSerialNumber, info.FileId.Identifier)
}

fn checked_identity(volume: u64, id: [u8; 16]) -> io::Result<(u64, [u8; 16])> {
    if id == [0; 16] || id == [u8::MAX; 16] {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "module directory has no unique file identity",
        ));
    }
    Ok((volume, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_component_prefixes_do_not_require_a_file_identity() {
        let root = Path::new(r"C:\kits\root");
        let inside = Path::new(r"C:\kits\root\nested\helpers.vibe");
        let unavailable = |_: &Path| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "provider has no unique file identity",
            ))
        };
        assert_eq!(
            absolute_target_with(root, inside, unavailable).unwrap(),
            Path::new(r"nested\helpers.vibe")
        );
        assert_eq!(
            absolute_target_with(
                root,
                Path::new(r"C:\kits\ROOT\nested\helpers.vibe"),
                unavailable,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(
            absolute_target_with(root, Path::new(r"C:\kits\root-other\helpers.vibe"), |_| {
                Ok(false)
            })
            .is_err()
        );
    }

    #[test]
    fn unsupported_and_nonunique_file_ids_cannot_identify_a_directory() {
        for id in [[0; 16], [u8::MAX; 16]] {
            assert_eq!(
                checked_identity(42, id).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(checked_identity(42, [7; 16]).unwrap(), (42, [7; 16]));
        assert_ne!(
            checked_identity(42, [7; 16]).unwrap(),
            checked_identity(43, [7; 16]).unwrap()
        );
    }
}
