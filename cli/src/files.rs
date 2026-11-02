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
//! found during discovery. On Unix, opens don't block, so a FIFO or device
//! swapped in after discovery is refused rather than waited on. A read stops
//! at [`MAX_SOURCE_BYTES`], the limit `vibes run` applies, even if the file
//! grows while it is read. A write opens without truncating, verifies the
//! file is unchanged since it was read, writes in place and truncates, so the
//! file keeps its identity, permissions and hard links. At most eight root
//! handles stay open; an evicted root is reopened only if it is still the same
//! directory.

use crate::{compat, source::MAX_SOURCE_BYTES};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
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
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            identity: Identity::of(metadata),
            regular: metadata.is_file(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

/// A file's device and inode where the platform reports them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity(Option<(u64, u64)>);

impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self(Some((metadata.dev(), metadata.ino())))
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Self(None)
        }
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
        let metadata = fs::metadata(&path).map_err(|error| {
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
                    identity: Identity::of(&metadata),
                });
            }
            continue;
        }
        let root = match inputs.roots.iter().position(|root| root.path == path) {
            Some(root) => root,
            None => {
                inputs.roots.push(Root {
                    path: path.clone(),
                    identity: Identity::of(&metadata),
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

/// Reads `len` bytes and checks that the stream ends there. Reading at most
/// one byte more bounds a file that grows while it is read.
fn read_exactly(reader: &mut impl Read, len: u64) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(len.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|error| compat::reason(&error))?;
    if data.len() as u64 != len {
        return Err("file changed while reading".to_owned());
    }
    Ok(data)
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
            None => {
                let mut options = fs::OpenOptions::new();
                options.read(!write).write(write);
                // A FIFO swapped in must not block the open. The target was
                // resolved during discovery, so a link here is a replacement.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
                }
                options
                    .open(&name)
                    .map_err(|error| compat::path_error("open", &name, &error))
            }
        }
    }

    /// Reads a candidate, refusing a file that is no longer the one discovered
    /// or that is larger than [`MAX_SOURCE_BYTES`].
    pub fn read(&mut self, index: usize) -> Result<(Vec<u8>, Info), String> {
        let mut file = self.open(index, false)?;
        let metadata = file.metadata().map_err(|error| compat::reason(&error))?;
        let info = Info::of(&metadata);
        if !info.regular || info.identity != self.files[index].identity {
            return Err("file changed after discovery".to_owned());
        }
        if info.len > MAX_SOURCE_BYTES {
            return Err(format!(
                "source exceeds maximum size ({} > {MAX_SOURCE_BYTES} bytes)",
                info.len
            ));
        }
        Ok((read_exactly(&mut file, info.len)?, info))
    }

    /// Rewrites a candidate in place if it is unchanged since it was read.
    pub fn write(&mut self, index: usize, original: &Info, data: &[u8]) -> Result<(), String> {
        let mut file = self.open(index, true)?;
        let metadata = file.metadata().map_err(|error| compat::reason(&error))?;
        let current = Info::of(&metadata);
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
            Ok(identity(&self.0.dir_metadata()?))
        }

        /// The identity of an entry, without following a link.
        pub fn identity_of(&self, name: &Path) -> io::Result<Identity> {
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
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            self.0
                .open_with(name, &options)
                .map(cap_std::fs::File::into_std)
        }
    }

    fn identity(metadata: &cap_std::fs::Metadata) -> Identity {
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            Identity(Some((metadata.dev(), metadata.ino())))
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Identity(None)
        }
    }
}

/// WASI already confines paths to the host's preopened directories, so a
/// root is its path, and links are refused when a candidate is opened.
#[cfg(target_os = "wasi")]
mod dir {
    use super::Identity;
    use std::{fs, io, path::Path, path::PathBuf};

    pub struct Dir(PathBuf);

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
            if !fs::metadata(path)?.is_dir() {
                return Err(io::Error::other("not a directory"));
            }
            Ok(Self(path.to_owned()))
        }

        pub fn identity(&self) -> io::Result<Identity> {
            Ok(Identity::of(&fs::metadata(&self.0)?))
        }

        pub fn list(&self, directory: &Path) -> io::Result<Vec<Entry>> {
            let mut listed = Vec::new();
            for entry in fs::read_dir(self.0.join(directory))? {
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

        pub fn identity_of(&self, name: &Path) -> io::Result<Identity> {
            Ok(Identity::of(&fs::symlink_metadata(self.0.join(name))?))
        }

        pub fn open(&self, name: &Path, write: bool) -> io::Result<fs::File> {
            let path = self.0.join(name);
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(io::Error::other("path escapes from parent"));
            }
            fs::OpenOptions::new().read(!write).write(write).open(path)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.cache/tmp");
            fs::create_dir_all(&base).unwrap();
            let path = base.join(format!("files-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
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

    fn collect_one(root: &Path, selection: Selection) -> Inputs {
        collect(&[root.as_os_str().to_owned()], selection).unwrap()
    }

    #[test]
    fn candidates_reject_replacement() {
        for selection in [Selection::Format, Selection::Fix] {
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
                        symlink(&outside, &path).unwrap();
                    }
                    "ancestor" => {
                        fs::rename(&nested, root.join("nested-saved")).unwrap();
                        symlink(outside.parent().unwrap(), &nested).unwrap();
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

    #[test]
    fn reads_stop_at_the_source_limit() {
        let limit = usize::try_from(MAX_SOURCE_BYTES).unwrap();
        for selection in [Selection::Format, Selection::Fix] {
            let scratch = Scratch::new("limit");
            let root = scratch.0.join("root");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("fits.vibe"), vec![b'#'; limit]).unwrap();
            fs::write(root.join("over.vibe"), vec![b'#'; limit + 1]).unwrap();
            let explicit = [root.join("fits.vibe"), root.join("over.vibe")];
            for operands in [vec![root.clone()], explicit.to_vec()] {
                let operands: Vec<OsString> = operands.into_iter().map(Into::into).collect();
                let mut inputs = collect(&operands, selection).unwrap();
                assert_eq!(inputs.read(0).unwrap().0.len(), limit);
                assert_eq!(
                    inputs.read(1).map(|_| ()).unwrap_err(),
                    format!(
                        "source exceeds maximum size ({} > {limit} bytes)",
                        limit + 1
                    )
                );
            }
        }
    }

    #[test]
    fn a_growing_stream_is_read_one_byte_past_its_length() {
        let mut grown = std::io::Cursor::new(vec![b'#'; 10]);
        assert_eq!(
            read_exactly(&mut grown, 4).unwrap_err(),
            "file changed while reading"
        );
        assert_eq!(grown.position(), 5);
        let mut endless = std::io::repeat(b'#');
        assert_eq!(
            read_exactly(&mut endless, MAX_SOURCE_BYTES).unwrap_err(),
            "file changed while reading"
        );
        let mut exact = std::io::Cursor::new(b"text".to_vec());
        assert_eq!(read_exactly(&mut exact, 4).unwrap(), b"text");
    }

    fn mkfifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    }

    /// A candidate swapped for a FIFO, directly or through a link, must be
    /// refused rather than block the open until a writer or reader appears.
    #[test]
    fn a_fifo_swapped_in_is_refused_without_blocking() {
        for selection in [Selection::Format, Selection::Fix] {
            for explicit in [false, true] {
                for linked in [false, true] {
                    let scratch = Scratch::new(&format!("fifo-{explicit}-{linked}"));
                    let root = scratch.0.join("root");
                    let path = root.join("file.vibe");
                    write(&path, "original  \n");
                    let mut inputs = collect_one(if explicit { &path } else { &root }, selection);
                    let (_, info) = inputs.read(0).unwrap();
                    fs::remove_file(&path).unwrap();
                    if linked {
                        mkfifo(&root.join("pipe"));
                        symlink("pipe", &path).unwrap();
                    } else {
                        mkfifo(&path);
                    }
                    let (sender, receiver) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let read = inputs.read(0).map(|_| ());
                        let written = inputs.write(0, &info, b"formatted\n");
                        sender.send((read, written)).unwrap();
                    });
                    let (read, written) = receiver
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap_or_else(|_| {
                            panic!("explicit {explicit}, linked {linked}: open blocked on a FIFO")
                        });
                    assert!(read.is_err(), "explicit {explicit}, linked {linked}: read");
                    assert!(
                        written.is_err(),
                        "explicit {explicit}, linked {linked}: write"
                    );
                }
            }
        }
    }

    #[test]
    fn keeps_the_opened_root() {
        for selection in [Selection::Format, Selection::Fix] {
            let scratch = Scratch::new("pinned");
            let root = scratch.0.join("root");
            write(&root.join("file.vibe"), "inside  \n");
            let mut inputs = collect_one(&root, selection);
            let moved = scratch.0.join("moved");
            fs::rename(&root, &moved).unwrap();
            let outside = scratch.0.join("outside");
            write(&outside.join("file.vibe"), "outside  \n");
            symlink(&outside, &root).unwrap();
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

    #[test]
    fn an_evicted_root_rejects_replacement() {
        for selection in [Selection::Format, Selection::Fix] {
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
            symlink(&outside, &targets[0]).unwrap();
            let info = Info::of(&fs::metadata(scratch.0.join("000-saved/file.vibe")).unwrap());
            assert!(inputs.read(0).is_err());
            assert!(inputs.write(0, &info, b"formatted\n").is_err());
            assert_eq!(
                fs::read_to_string(outside.join("file.vibe")).unwrap(),
                "outside  \n"
            );
        }
    }
}
