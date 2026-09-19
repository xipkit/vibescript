use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, FileType, Mode, OFlags},
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::wasi::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

mod root;

#[derive(Debug)]
pub(super) struct Dir(OwnedFd);

impl Dir {
    pub(super) fn symlink_metadata(&self, name: &OsStr) -> io::Result<Metadata> {
        let stat = rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(Metadata(FileType::from_raw_mode(stat.st_mode)))
    }

    pub(super) fn read_link_contents(&self, name: &OsStr) -> io::Result<PathBuf> {
        // The caller reserves twice PATH_WORKSPACE before following a link.
        // Keep reads bounded even when a WASI host accepts unusually long targets.
        let mut target = vec![0; super::platform::PATH_WORKSPACE];
        let count = rustix::fs::readlinkat_raw(&self.0, name, &mut target[..])?;
        if count == target.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "module symlink target is too long",
            ));
        }
        target.truncate(count);
        Ok(PathBuf::from(OsString::from_vec(target)))
    }

    pub(super) fn open_dir_nofollow(&self, name: &OsStr) -> io::Result<Self> {
        rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(Self)
        .map_err(Into::into)
    }

    pub(super) fn open_file(&self, name: &OsStr) -> io::Result<File> {
        rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    pub(super) fn entries(&self) -> io::Result<ReadDir> {
        rustix::fs::Dir::read_from(&self.0)
            .map(ReadDir)
            .map_err(Into::into)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Metadata(FileType);

impl Metadata {
    pub(super) fn file_type(&self) -> Self {
        *self
    }
    pub(super) fn is_symlink(&self) -> bool {
        self.0 == FileType::Symlink
    }
    pub(super) fn is_dir(&self) -> bool {
        self.0 == FileType::Directory
    }
    pub(super) fn is_file(&self) -> bool {
        self.0 == FileType::RegularFile
    }
}

pub(super) struct ReadDir(rustix::fs::Dir);
pub(super) struct DirEntry(rustix::fs::DirEntry);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.0.next()?;
            // Match std::fs::ReadDir even when the WASI host includes dots.
            if entry
                .as_ref()
                .is_ok_and(|entry| matches!(entry.file_name().to_bytes(), b"." | b".."))
            {
                continue;
            }
            return Some(entry.map(DirEntry).map_err(Into::into));
        }
    }
}

impl DirEntry {
    pub(super) fn file_name(&self) -> &OsStr {
        OsStr::from_bytes(self.0.file_name().to_bytes())
    }
}

pub(super) fn open_root(path: &Path) -> io::Result<(Dir, PathBuf)> {
    root::open(path)
}
