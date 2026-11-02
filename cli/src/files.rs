//! Shared file discovery and safe rewriting for `vibes fmt` and `vibes fix`.
//!
//! A directory operand is opened as a root handle and walked without
//! following symbolic links: only regular `.vibe` files are candidates, and
//! linked files and directories are skipped. Candidates are later opened
//! through that handle, which refuses paths that escape the root. An explicit
//! file operand authorizes its target, so a linked operand is rewritten where
//! it points and the link itself is kept.
//!
//! Every read and write checks that the opened file is still the regular file
//! found during discovery. A write opens without truncating, verifies the
//! file is unchanged since it was read, writes in place and truncates, so the
//! file keeps its identity, permissions and hard links. At most eight root
//! handles stay open; an evicted root is reopened only if it is still the same
//! directory.

use crate::compat;
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_OPEN_ROOTS: usize = 8;

/// Command-specific operand filtering and ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    /// Only `.vibe` operands, deduplicated and sorted by absolute path bytes.
    Format,
    /// All explicit files, preserving operand order and sorted tree traversal.
    Fix,
}

/// Discovered candidates and the roots they belong to.
pub struct Inputs {
    pub files: Vec<Candidate>,
    roots: Vec<Root>,
    open: Vec<usize>,
    next_eviction: usize,
}

pub struct Candidate {
    /// The directory operand that contains it, or `None` for an explicit file.
    root: Option<usize>,
    /// The path within the root, or the resolved target of an explicit file.
    name: PathBuf,
    /// The absolute path reported in messages and used for ordering.
    pub path: PathBuf,
    /// The explicit operand spelling or the path relative to its directory operand.
    pub label: String,
    identity: Identity,
}

struct Root {
    path: PathBuf,
    identity: Identity,
    handle: Option<dir::Dir>,
}

/// What a read learned about a file, checked again before writing.
#[derive(Clone)]
pub struct Info {
    identity: Identity,
    regular: bool,
    len: u64,
    modified: Option<SystemTime>,
}

impl Info {
    fn of(file: &fs::File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            identity: identity::file(file, &metadata)?,
            regular: metadata.is_file(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// A file's device and inode on Unix and WASI, or its volume serial number and
/// file ID on Windows. It is `None` only where the file system reports no
/// unique ID, which leaves the type, length and modification time checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity(Option<(u64, u128)>);

impl Identity {
    #[cfg(any(unix, target_os = "wasi", windows))]
    fn new(volume: u64, file: impl Into<u128>) -> Self {
        Self(Some((volume, file.into())))
    }
}

/// Where each platform reports file identities.
mod identity {
    use super::Identity;
    use std::{fs, io, path::Path};

    /// The metadata and identity of the file a path names, following links.
    pub fn stat(path: &Path) -> io::Result<(fs::Metadata, Identity)> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            // Attribute access opens directories and files alike, as
            // `fs::metadata` does, without reading their contents.
            let file = fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)?;
            Ok((file.metadata()?, handle(&file)))
        }
        #[cfg(not(windows))]
        {
            let metadata = fs::metadata(path)?;
            #[cfg(target_os = "wasi")]
            let identity = wasi(&rustix::fs::stat(path)?);
            #[cfg(not(target_os = "wasi"))]
            let identity = file_metadata(&metadata);
            Ok((metadata, identity))
        }
    }

    /// The identity of an open file, given its metadata.
    pub fn file(file: &fs::File, metadata: &fs::Metadata) -> io::Result<Identity> {
        #[cfg(windows)]
        {
            let _ = metadata;
            Ok(handle(file))
        }
        #[cfg(target_os = "wasi")]
        {
            let _ = metadata;
            Ok(wasi(&rustix::fs::fstat(file)?))
        }
        #[cfg(not(any(windows, target_os = "wasi")))]
        {
            let _ = file;
            Ok(file_metadata(metadata))
        }
    }

    #[cfg(not(any(windows, target_os = "wasi")))]
    fn file_metadata(metadata: &fs::Metadata) -> Identity {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Identity::new(metadata.dev(), metadata.ino())
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Identity(None)
        }
    }

    /// WASI's `MetadataExt` is unstable, so device and inode come from `stat`.
    #[cfg(target_os = "wasi")]
    pub fn wasi(stat: &rustix::fs::Stat) -> Identity {
        Identity::new(stat.st_dev, stat.st_ino)
    }

    /// The volume serial number and file ID of an open file or directory.
    ///
    /// `FileIdInfo` reports ReFS's 128-bit IDs; file systems without it fall
    /// back to the 64-bit index. A zero or all-ones ID is not unique, so it
    /// gives no identity.
    #[cfg(windows)]
    pub fn handle(handle: &impl std::os::windows::io::AsRawHandle) -> Identity {
        use std::mem::{MaybeUninit, size_of};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx,
        };
        let raw = handle.as_raw_handle();
        let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
        // SAFETY: the handle stays borrowed for the call, and the buffer has
        // the size and alignment FileIdInfo requires.
        if unsafe {
            GetFileInformationByHandleEx(
                raw,
                FileIdInfo,
                id.as_mut_ptr().cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } != 0
        {
            // SAFETY: the successful call initialized the whole structure.
            let id = unsafe { id.assume_init() };
            let file = u128::from_le_bytes(id.FileId.Identifier);
            if file != 0 && file != u128::MAX {
                return Identity::new(id.VolumeSerialNumber, file);
            }
        }
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: as above, with the structure GetFileInformationByHandle fills.
        if unsafe { GetFileInformationByHandle(raw, info.as_mut_ptr()) } != 0 {
            // SAFETY: the successful call initialized the whole structure.
            let info = unsafe { info.assume_init() };
            let file = u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow);
            if file != 0 && file != u64::MAX {
                return Identity::new(info.dwVolumeSerialNumber.into(), file);
            }
        }
        Identity(None)
    }
}

/// Collects regular files according to the command's operand policy.
/// Recursive discovery only includes `.vibe` files and never follows links.
pub fn collect(targets: &[OsString], selection: Selection) -> Result<Inputs, String> {
    let mut inputs = Inputs {
        files: Vec::new(),
        roots: Vec::new(),
        open: Vec::new(),
        next_eviction: 0,
    };
    let mut seen = std::collections::HashSet::new();
    for target in targets {
        let label = target.to_string_lossy();
        let path = match selection {
            Selection::Format => compat::absolute(Path::new(target)),
            // Fix operands have always used filesystem resolution, including
            // `link/..`; lexical cleaning can select a different target.
            Selection::Fix => fs::canonicalize(target),
        }
        .map_err(|error| format!("resolve {label}: {}", compat::reason(&error)))?;
        let (metadata, identity) = identity::stat(&path).map_err(|error| {
            format!(
                "stat {label}: {}",
                compat::path_error("stat", &path, &error)
            )
        })?;
        if !metadata.is_dir() {
            if selection == Selection::Format && !is_vibe(&path) {
                continue;
            }
            if !metadata.is_file() {
                return Err(format!("{label} is not a regular file"));
            }
            // An explicit operand authorizes its selected target; recursive
            // discovery never follows a leaf or directory link.
            let resolved = fs::canonicalize(&path).map_err(|error| {
                format!(
                    "resolve {label}: {}",
                    compat::path_error("lstat", &path, &error)
                )
            })?;
            if selection == Selection::Fix || seen.insert(path.clone()) {
                inputs.files.push(Candidate {
                    root: None,
                    name: resolved,
                    path,
                    label: label.into_owned(),
                    identity,
                });
            }
            continue;
        }
        let root = match inputs.roots.iter().position(|root| root.path == path) {
            Some(root) => root,
            None => {
                inputs.roots.push(Root {
                    path: path.clone(),
                    identity,
                    handle: None,
                });
                inputs.roots.len() - 1
            }
        };
        let mut walked = inputs.walk(root, selection).map_err(|error| match error {
            Walk::Open(error) => error,
            Walk::Entry(error) => format!("walk {label}: {error}"),
        })?;
        // Component ordering keeps a directory's descendants ahead of a sibling
        // such as `b.vibe`, matching the fixer's sorted depth-first traversal.
        if selection == Selection::Fix {
            walked.sort_by(|a, b| a.0.cmp(&b.0));
        }
        for (name, identity) in walked {
            let joined = path.join(&name);
            if selection == Selection::Fix || seen.insert(joined.clone()) {
                inputs.files.push(Candidate {
                    root: Some(root),
                    label: name.to_string_lossy().replace('\\', "/"),
                    name,
                    path: joined,
                    identity,
                });
            }
        }
    }
    if selection == Selection::Format {
        inputs.files.sort_by(|a, b| {
            compat::bytes(a.path.as_os_str()).cmp(&compat::bytes(b.path.as_os_str()))
        });
    }
    Ok(inputs)
}

fn is_vibe(path: &Path) -> bool {
    compat::bytes(path.as_os_str()).ends_with(b".vibe")
}

enum Walk {
    /// Opening the root failed; reported as is.
    Open(String),
    /// Reading a directory within the root failed.
    Entry(String),
}

impl Inputs {
    /// Walks a root without following links, returning regular `.vibe` files.
    fn walk(
        &mut self,
        root: usize,
        selection: Selection,
    ) -> Result<Vec<(PathBuf, Identity)>, Walk> {
        let handle = self.handle(root).map_err(Walk::Open)?;
        let mut found = Vec::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(directory) = pending.pop() {
            let entries = handle.list(&directory).map_err(|error| {
                let shown = if directory.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    &directory
                };
                Walk::Entry(compat::path_error("open", shown, &error))
            })?;
            for entry in entries {
                let name = directory.join(&entry.name);
                match entry.kind {
                    dir::Kind::Directory => pending.push(name),
                    dir::Kind::Regular
                        if match selection {
                            Selection::Format => is_vibe(&name),
                            Selection::Fix => name.extension().is_some_and(|e| e == "vibe"),
                        } =>
                    {
                        let identity = handle.identity_of(&name).map_err(|error| {
                            Walk::Entry(compat::path_error("lstat", &name, &error))
                        })?;
                        found.push((name, identity));
                    }
                    _ => {}
                }
            }
        }
        Ok(found)
    }

    /// Returns a root's open handle, reopening an evicted root only when it
    /// is still the directory found during discovery.
    fn handle(&mut self, root: usize) -> Result<&dir::Dir, String> {
        if self.roots[root].handle.is_none() {
            let path = &self.roots[root].path;
            let handle =
                dir::Dir::root(path).map_err(|error| compat::path_error("open", path, &error))?;
            let identity = handle
                .identity()
                .map_err(|error| compat::path_error("stat", path, &error))?;
            if identity != self.roots[root].identity {
                return Err("directory changed after discovery".to_owned());
            }
            if self.open.len() == MAX_OPEN_ROOTS {
                let evicted = self.open[self.next_eviction];
                self.roots[evicted].handle = None;
                self.open[self.next_eviction] = root;
                self.next_eviction = (self.next_eviction + 1) % MAX_OPEN_ROOTS;
            } else {
                self.open.push(root);
            }
            self.roots[root].handle = Some(handle);
        }
        Ok(self.roots[root].handle.as_ref().expect("opened above"))
    }

    fn open(&mut self, index: usize, write: bool) -> Result<fs::File, String> {
        let name = self.files[index].name.clone();
        match self.files[index].root {
            Some(root) => self
                .handle(root)?
                .open(&name, write)
                .map_err(|error| compat::path_error("open", &name, &error)),
            None => fs::OpenOptions::new()
                .read(!write)
                .write(write)
                .open(&name)
                .map_err(|error| compat::path_error("open", &name, &error)),
        }
    }

    /// Reads a candidate, refusing a file that is no longer the one discovered.
    pub fn read(&mut self, index: usize) -> Result<(Vec<u8>, Info), String> {
        let mut file = self.open(index, false)?;
        let info = Info::of(&file).map_err(|error| compat::reason(&error))?;
        if !info.regular || info.identity != self.files[index].identity {
            return Err("file changed after discovery".to_owned());
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(info.len.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|error| compat::reason(&error))?;
        if data.len() as u64 != info.len {
            return Err("file changed while reading".to_owned());
        }
        Ok((data, info))
    }

    /// Rewrites a candidate in place if it is unchanged since it was read.
    pub fn write(&mut self, index: usize, original: &Info, data: &[u8]) -> Result<(), String> {
        let mut file = self.open(index, true)?;
        let current = Info::of(&file).map_err(|error| compat::reason(&error))?;
        if !current.regular
            || current.identity != original.identity
            || current.len != original.len
            || current.modified != original.modified
        {
            return Err("file changed before writing".to_owned());
        }
        file.write_all(data)
            .and_then(|()| file.set_len(data.len() as u64))
            .map_err(|error| compat::reason(&error))
    }
}

/// Directory handles that confine opens to their root.
#[cfg(not(target_os = "wasi"))]
mod dir {
    use super::Identity;
    use cap_std::{ambient_authority, fs::OpenOptions};
    use std::{fs, io, path::Path};

    pub struct Dir(cap_std::fs::Dir);

    pub enum Kind {
        Directory,
        Regular,
        Other,
    }

    pub struct Entry {
        pub name: std::ffi::OsString,
        pub kind: Kind,
    }

    impl Dir {
        pub fn root(path: &Path) -> io::Result<Self> {
            cap_std::fs::Dir::open_ambient_dir(path, ambient_authority()).map(Self)
        }

        pub fn identity(&self) -> io::Result<Identity> {
            #[cfg(windows)]
            return Ok(super::identity::handle(&self.0));
            #[cfg(not(windows))]
            Ok(identity(&self.0.dir_metadata()?))
        }

        /// The identity of an entry, without following a link.
        pub fn identity_of(&self, name: &Path) -> io::Result<Identity> {
            #[cfg(windows)]
            {
                // Windows reports file IDs only through a handle. Attribute
                // access is exempt from sharing modes, and opening a link
                // itself keeps a swapped-in link from taking its target's
                // identity.
                use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
                use cap_std::fs::OpenOptionsExt;
                use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
                let mut options = OpenOptions::new();
                options
                    .access_mode(FILE_READ_ATTRIBUTES)
                    .follow(FollowSymlinks::No);
                Ok(super::identity::handle(&self.0.open_with(name, &options)?))
            }
            #[cfg(not(windows))]
            Ok(identity(&self.0.symlink_metadata(name)?))
        }

        /// Lists a directory within the root without following links.
        pub fn list(&self, directory: &Path) -> io::Result<Vec<Entry>> {
            let entries = if directory.as_os_str().is_empty() {
                self.0.entries()?
            } else {
                self.0.read_dir(directory)?
            };
            let mut listed = Vec::new();
            for entry in entries {
                let entry = entry?;
                let kind = entry.file_type()?;
                listed.push(Entry {
                    name: entry.file_name(),
                    kind: if kind.is_dir() {
                        Kind::Directory
                    } else if kind.is_file() {
                        Kind::Regular
                    } else {
                        Kind::Other
                    },
                });
            }
            Ok(listed)
        }

        pub fn open(&self, name: &Path, write: bool) -> io::Result<fs::File> {
            let mut options = OpenOptions::new();
            options.read(!write).write(write);
            self.0
                .open_with(name, &options)
                .map(cap_std::fs::File::into_std)
        }
    }

    #[cfg(not(windows))]
    fn identity(metadata: &cap_std::fs::Metadata) -> Identity {
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            Identity::new(metadata.dev(), metadata.ino())
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Identity(None)
        }
    }
}

/// WASI directory handles. A path below a root is opened one component at a
/// time from the root's descriptor without following links, so a replaced
/// directory or file cannot redirect it, even within a broader host preopen.
#[cfg(target_os = "wasi")]
mod dir {
    use super::{Identity, identity};
    use rustix::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        fs::{AtFlags, FileType, Mode, OFlags},
    };
    use std::{
        ffi::OsStr,
        fs, io,
        os::wasi::ffi::OsStrExt,
        path::{Component, Components, Path},
    };

    pub struct Dir(OwnedFd);

    pub enum Kind {
        Directory,
        Regular,
        Other,
    }

    pub struct Entry {
        pub name: std::ffi::OsString,
        pub kind: Kind,
    }

    impl Dir {
        pub fn root(path: &Path) -> io::Result<Self> {
            Ok(Self(rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY,
                Mode::empty(),
            )?))
        }

        pub fn identity(&self) -> io::Result<Identity> {
            Ok(identity::wasi(&rustix::fs::fstat(&self.0)?))
        }

        pub fn list(&self, directory: &Path) -> io::Result<Vec<Entry>> {
            let opened = self.descend(directory.components())?;
            let fd = opened.as_ref().map_or(self.0.as_fd(), AsFd::as_fd);
            let mut listed = Vec::new();
            for entry in rustix::fs::Dir::read_from(fd)? {
                let entry = entry?;
                let name = entry.file_name();
                if matches!(name.to_bytes(), b"." | b"..") {
                    continue;
                }
                let kind = match entry.file_type() {
                    FileType::Unknown => FileType::from_raw_mode(
                        rustix::fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode,
                    ),
                    kind => kind,
                };
                listed.push(Entry {
                    name: OsStr::from_bytes(name.to_bytes()).to_owned(),
                    kind: match kind {
                        FileType::Directory => Kind::Directory,
                        FileType::RegularFile => Kind::Regular,
                        _ => Kind::Other,
                    },
                });
            }
            Ok(listed)
        }

        pub fn identity_of(&self, name: &Path) -> io::Result<Identity> {
            self.at(name, |parent, leaf| {
                Ok(identity::wasi(&rustix::fs::statat(
                    parent,
                    leaf,
                    AtFlags::SYMLINK_NOFOLLOW,
                )?))
            })
        }

        pub fn open(&self, name: &Path, write: bool) -> io::Result<fs::File> {
            self.at(name, |parent, leaf| {
                if !write {
                    let flags = OFlags::RDONLY | OFlags::NOFOLLOW;
                    return Ok(rustix::fs::openat(parent, leaf, flags, Mode::empty())?.into());
                }
                // Wasmtime reports a directory's inheriting rights without
                // FD_WRITE unless it is a preopen, and wasi-libc's `openat`
                // masks a file's rights with them, so ask for writing directly.
                let leaf = leaf.to_str().ok_or(io::ErrorKind::InvalidInput)?;
                let rights = wasi::RIGHTS_FD_WRITE
                    | wasi::RIGHTS_FD_FILESTAT_GET
                    | wasi::RIGHTS_FD_FILESTAT_SET_SIZE;
                // SAFETY: `parent` is a live descriptor for the call, and the
                // returned descriptor is new and owned by nothing else.
                let fd = unsafe {
                    wasi::path_open(parent.as_raw_fd() as wasi::Fd, 0, leaf, 0, rights, 0, 0)
                }
                .map_err(|error| io::Error::from_raw_os_error(error.raw().into()))?;
                // SAFETY: as above, the descriptor is newly opened and unowned.
                Ok(unsafe { fs::File::from_raw_fd(fd as RawFd) })
            })
        }

        /// Calls `f` with the directory that holds `name`'s last component.
        fn at<T>(
            &self,
            name: &Path,
            f: impl FnOnce(BorrowedFd<'_>, &OsStr) -> io::Result<T>,
        ) -> io::Result<T> {
            let mut components = name.components();
            let Some(Component::Normal(leaf)) = components.next_back() else {
                return Err(escape());
            };
            let parent = self.descend(components)?;
            f(parent.as_ref().map_or(self.0.as_fd(), AsFd::as_fd), leaf)
        }

        /// Opens a directory below the root without following links, or
        /// returns `None` for the root itself.
        fn descend(&self, components: Components<'_>) -> io::Result<Option<OwnedFd>> {
            let mut current: Option<OwnedFd> = None;
            for component in components {
                let Component::Normal(part) = component else {
                    return Err(escape());
                };
                let parent = current.as_ref().map_or(self.0.as_fd(), AsFd::as_fd);
                let next = rustix::fs::openat(
                    parent,
                    part,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?;
                current = Some(next);
            }
            Ok(current)
        }
    }

    fn escape() -> io::Error {
        io::Error::other("path escapes from parent")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join(".cache/tmp");
            fs::create_dir_all(&base).unwrap();
            // WASI has no process IDs, so runs there are told apart by time.
            #[cfg(not(target_os = "wasi"))]
            let run = u128::from(std::process::id());
            #[cfg(target_os = "wasi")]
            let run = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = base.join(format!("files-{name}-{run}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            // WASI's realpath fails beneath a preopen whose ancestors are
            // hidden, as in scripts/check-wasi; the path is already absolute.
            #[cfg(not(target_os = "wasi"))]
            let path = fs::canonicalize(path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Links `link` to a relative `target`; WASI hosts refuse absolute ones.
    #[cfg(any(unix, target_os = "wasi"))]
    fn symlink(target: &str, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(target_os = "wasi")]
        rustix::fs::symlink(target, link).unwrap();
    }

    /// Writes `text` to a new file at `path` with `original`'s length and
    /// modification time, so only identity tells the two apart.
    fn lookalike(original: &Path, path: &Path, text: &str) {
        let metadata = fs::metadata(original).unwrap();
        assert_eq!(text.len() as u64, metadata.len());
        write(path, text);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(metadata.modified().unwrap())
            .unwrap();
        let copy = fs::metadata(path).unwrap();
        assert_eq!(copy.len(), metadata.len());
        assert_eq!(copy.modified().unwrap(), metadata.modified().unwrap());
    }

    /// Moves `path` aside, keeping the original alive so its ID is not
    /// reused, and renames a lookalike into its place.
    fn swap(path: &Path, text: &str) {
        let staged = path.with_extension("staged");
        lookalike(path, &staged, text);
        fs::rename(path, path.with_extension("saved")).unwrap();
        fs::rename(&staged, path).unwrap();
    }

    /// The selections a test can collect with. Fix operands and explicit
    /// files resolve with `fs::canonicalize`, which WASI cannot do beneath a
    /// preopen whose ancestors are hidden, as in scripts/check-wasi (VIBE-126).
    fn selections(explicit: bool) -> Vec<Selection> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        if cfg!(not(target_os = "wasi")) || fs::canonicalize(base).is_ok() {
            vec![Selection::Format, Selection::Fix]
        } else {
            eprintln!("skipping canonicalized operands: WASI cannot resolve {base:?}");
            if explicit {
                vec![]
            } else {
                vec![Selection::Format]
            }
        }
    }

    fn collect_one(root: &Path, selection: Selection) -> Inputs {
        collect(&[root.as_os_str().to_owned()], selection).unwrap()
    }

    const ORIGINAL: &str = "original  \n";

    /// Writes `ORIGINAL` with a whole-second modification time, which a
    /// lookalike can copy exactly even where Node's WASI drops fractions.
    fn write_original(path: &Path) {
        write(path, ORIGINAL);
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }
    const LOOKALIKE: &str = "imposter  \n";

    /// Swaps a candidate for a same-size, same-mtime file before reading and
    /// again after, and checks that neither the read nor the write accepts it.
    fn rejects_lookalikes(
        name: &str,
        explicit: bool,
        operand: impl Fn(&Path) -> PathBuf,
        replace: impl Fn(&Path),
    ) {
        for selection in selections(explicit) {
            for after_read in [false, true] {
                let scratch = Scratch::new(&format!("{name}-{selection:?}-{after_read}"));
                let path = scratch.0.join("root/nested/file.vibe");
                write_original(&path);
                let mut inputs = collect_one(&operand(&scratch.0), selection);
                let original = if after_read {
                    let (data, info) = inputs.read(0).unwrap();
                    assert_eq!(data, ORIGINAL.as_bytes());
                    Some(info)
                } else {
                    None
                };
                replace(&path);
                let context = format!("{name}, {selection:?}, after read: {after_read}");
                match original {
                    None => assert!(inputs.read(0).is_err(), "{context}: read a lookalike"),
                    Some(info) => assert!(
                        inputs.write(0, &info, b"formatted\n").is_err(),
                        "{context}: wrote a lookalike"
                    ),
                }
                assert_eq!(fs::read_to_string(&path).unwrap(), LOOKALIKE, "{context}");
            }
        }
    }

    #[test]
    fn explicit_files_reject_lookalikes() {
        rejects_lookalikes(
            "explicit",
            true,
            |scratch| scratch.join("root/nested/file.vibe"),
            |path| swap(path, LOOKALIKE),
        );
    }

    #[test]
    fn discovered_files_reject_lookalikes() {
        rejects_lookalikes(
            "discovered",
            false,
            |scratch| scratch.join("root"),
            |path| swap(path, LOOKALIKE),
        );
    }

    #[test]
    fn discovered_files_reject_a_replaced_ancestor() {
        rejects_lookalikes(
            "ancestor",
            false,
            |scratch| scratch.join("root"),
            |path| {
                let nested = path.parent().unwrap();
                let decoy = nested.with_file_name("decoy");
                lookalike(path, &decoy.join("file.vibe"), LOOKALIKE);
                fs::rename(nested, nested.with_file_name("saved")).unwrap();
                fs::rename(&decoy, nested).unwrap();
            },
        );
    }

    #[test]
    fn an_evicted_root_rejects_a_replaced_directory() {
        for selection in selections(false) {
            for after_read in [false, true] {
                let scratch = Scratch::new(&format!("replaced-root-{selection:?}-{after_read}"));
                let targets: Vec<PathBuf> = (0..32)
                    .map(|i| {
                        let target = scratch.0.join(format!("{i:03}"));
                        write_original(&target.join("file.vibe"));
                        target
                    })
                    .collect();
                let operands: Vec<OsString> =
                    targets.iter().map(|t| t.as_os_str().to_owned()).collect();
                let mut inputs = collect(&operands, selection).unwrap();
                let original = if after_read {
                    let (_, info) = inputs.read(0).unwrap();
                    // Reading eight other roots evicts the first again.
                    for index in 1..=MAX_OPEN_ROOTS {
                        inputs.read(index).unwrap();
                    }
                    Some(info)
                } else {
                    None
                };
                let decoy = scratch.0.join("decoy");
                lookalike(
                    &targets[0].join("file.vibe"),
                    &decoy.join("file.vibe"),
                    LOOKALIKE,
                );
                fs::rename(&targets[0], scratch.0.join("saved")).unwrap();
                fs::rename(&decoy, &targets[0]).unwrap();
                let context = format!("{selection:?}, after read: {after_read}");
                let error = match original {
                    None => inputs.read(0).err(),
                    Some(info) => inputs.write(0, &info, b"formatted\n").err(),
                };
                assert_eq!(
                    error.as_deref(),
                    Some("directory changed after discovery"),
                    "{context}"
                );
                assert_eq!(
                    fs::read_to_string(targets[0].join("file.vibe")).unwrap(),
                    LOOKALIKE,
                    "{context}"
                );
            }
        }
    }

    #[cfg(any(unix, target_os = "wasi"))]
    #[test]
    fn candidates_reject_replacement() {
        for selection in selections(false) {
            for replaced in ["leaf", "ancestor", "regular file"] {
                let scratch = Scratch::new(&format!("replace-{}", replaced.replace(' ', "-")));
                let root = scratch.0.join("root");
                let nested = root.join("nested");
                let path = nested.join("file.vibe");
                write(&path, "original  \n");
                let outside = scratch.0.join("outside").join("file.vibe");
                write(&outside, "outside  \n");
                let mut inputs = collect_one(&root, selection);
                let (_, original) = inputs.read(0).unwrap();
                match replaced {
                    "leaf" => {
                        fs::remove_file(&path).unwrap();
                        symlink("../../outside/file.vibe", &path);
                    }
                    "ancestor" => {
                        fs::rename(&nested, root.join("nested-saved")).unwrap();
                        symlink("../outside", &nested);
                    }
                    _ => {
                        fs::rename(&path, nested.join("file.vibe.saved")).unwrap();
                        write(&path, "replacement\n");
                    }
                }
                assert!(
                    inputs.read(0).is_err(),
                    "{replaced}: read accepted a replacement"
                );
                assert!(
                    inputs.write(0, &original, b"formatted\n").is_err(),
                    "{replaced}: write accepted a replacement"
                );
                assert_eq!(fs::read_to_string(&outside).unwrap(), "outside  \n");
                if replaced == "regular file" {
                    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\n");
                }
            }
        }
    }

    #[cfg(any(unix, target_os = "wasi"))]
    #[test]
    fn keeps_the_opened_root() {
        for selection in selections(false) {
            let scratch = Scratch::new("pinned");
            let root = scratch.0.join("root");
            write(&root.join("file.vibe"), "inside  \n");
            let mut inputs = collect_one(&root, selection);
            let moved = scratch.0.join("moved");
            fs::rename(&root, &moved).unwrap();
            let outside = scratch.0.join("outside");
            write(&outside.join("file.vibe"), "outside  \n");
            symlink("outside", &root);
            let (data, info) = inputs.read(0).unwrap();
            assert_eq!(data, b"inside  \n");
            inputs
                .write(0, &info, &vibescript_tools::format::format_bytes(&data))
                .unwrap();
            assert_eq!(
                fs::read_to_string(moved.join("file.vibe")).unwrap(),
                "inside\n"
            );
            assert_eq!(
                fs::read_to_string(outside.join("file.vibe")).unwrap(),
                "outside  \n"
            );
        }
    }

    #[cfg(any(unix, target_os = "wasi"))]
    #[test]
    fn an_evicted_root_rejects_replacement() {
        for selection in selections(false) {
            let scratch = Scratch::new("evicted");
            let targets: Vec<PathBuf> = (0..32)
                .map(|i| {
                    let target = scratch.0.join(format!("{i:03}"));
                    write(&target.join("file.vibe"), "original  \n");
                    target
                })
                .collect();
            let operands: Vec<OsString> =
                targets.iter().map(|t| t.as_os_str().to_owned()).collect();
            let mut inputs = collect(&operands, selection).unwrap();
            fs::rename(&targets[0], scratch.0.join("000-saved")).unwrap();
            let outside = scratch.0.join("outside");
            write(&outside.join("file.vibe"), "outside  \n");
            symlink("outside", &targets[0]);
            let saved = fs::File::open(scratch.0.join("000-saved/file.vibe")).unwrap();
            let info = Info::of(&saved).unwrap();
            assert!(inputs.read(0).is_err());
            assert!(inputs.write(0, &info, b"formatted\n").is_err());
            assert_eq!(
                fs::read_to_string(outside.join("file.vibe")).unwrap(),
                "outside  \n"
            );
        }
    }
}
