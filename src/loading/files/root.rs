use super::{io_error, not_regular, platform, spelling_from_directory};
use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer};
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::{
    fs::File,
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
struct Directory {
    handle: Dir,
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub(in crate::loading) struct Root(Arc<Directory>);

impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Root {}

impl Hash for Root {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug)]
pub(in crate::loading) enum Opened {
    File(File),
    Missing,
    BrokenLink,
}

impl Root {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let handle = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
        Ok(Self(Arc::new(Directory { handle, path })))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn open(&self, ctx: &mut CallContext, relative: &Path) -> Result<Opened> {
        ctx.checkpoint()?;
        ctx.charge(relative.as_os_str().as_encoded_bytes().len() as u64)?;
        if relative.as_os_str().is_empty() {
            return Ok(Opened::Missing);
        }
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(escape());
        }
        // Every library operation below receives one component and disables
        // symlink following. Its path workspace cannot grow with the whole walk.
        let _scratch = ctx.reserve(platform::PATH_WORKSPACE * 4 + 1024)?;
        let mut walk = Walk {
            root: self,
            directories: Buffer::empty(),
            file: None,
            leaf: false,
            links: 0,
            broken: false,
            matches: true,
        };
        if !walk.resolve(ctx, relative, true)? {
            return Ok(if walk.broken {
                Opened::BrokenLink
            } else {
                Opened::Missing
            });
        }
        if !walk.matches {
            return Ok(Opened::Missing);
        }
        walk.file.take().map(Opened::File).ok_or_else(not_regular)
    }
}

struct Walk<'a> {
    root: &'a Root,
    directories: Buffer<Dir>,
    file: Option<File>,
    leaf: bool,
    links: usize,
    broken: bool,
    matches: bool,
}

impl Walk<'_> {
    fn parent(&self) -> &Dir {
        self.directories.data.last().unwrap_or(&self.root.0.handle)
    }

    fn resolve(&mut self, ctx: &mut CallContext, path: &Path, exact: bool) -> Result<bool> {
        ctx.charge(path.as_os_str().as_encoded_bytes().len() as u64)?;
        for part in path.components() {
            ctx.charge(1)?;
            if self.leaf {
                return Err(not_directory());
            }
            let name = match part {
                Component::CurDir => continue,
                Component::ParentDir => {
                    if self.directories.data.pop().is_none() {
                        return Err(escape());
                    }
                    continue;
                }
                Component::Normal(name) => name,
                _ => return Err(escape()),
            };
            if name.as_encoded_bytes().len() >= platform::PATH_WORKSPACE {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "require: module path component is too long",
                ));
            }
            let metadata = self.parent().symlink_metadata(name);
            ctx.checkpoint()?;
            let metadata = match metadata {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(io_error("checking module source", error)),
            };
            if exact && self.matches {
                self.matches = match platform::stored_name(ctx, self.parent(), name)? {
                    Some(matches) => matches,
                    None => spelling_from_directory(ctx, self.parent(), name)?,
                };
            }
            if metadata.file_type().is_symlink() {
                if self.links == 40 {
                    return Err(Error::new(
                        ErrorKind::Runtime,
                        "require: too many module symlinks",
                    ));
                }
                self.links += 1;
                // readlink's growing buffer can hold up to twice the platform
                // path limit. Keep its reservation through nested expansion.
                let _target = ctx.reserve(platform::PATH_WORKSPACE * 2)?;
                let target = self.parent().read_link_contents(name);
                ctx.checkpoint()?;
                let target = target.map_err(|e| io_error("reading module symlink", e))?;
                if target.capacity() > platform::PATH_WORKSPACE * 2 {
                    return ctx.fail(
                        ErrorKind::Memory,
                        "module symlink exceeded reserved storage",
                    );
                }
                let relative = if target.is_absolute() {
                    ctx.charge(self.root.path().as_os_str().as_encoded_bytes().len() as u64)?;
                    let target = target
                        .strip_prefix(self.root.path())
                        .map_err(|_| escape())?;
                    self.directories.data.clear();
                    target
                } else {
                    &target
                };
                if !self.resolve(ctx, relative, false)? {
                    self.broken = true;
                    return Ok(false);
                }
            } else if metadata.is_dir() {
                // Reserve before acquiring a descriptor, including the stack
                // slot retained for a later '..' inside a symlink target.
                if self.directories.data.len() == self.directories.data.capacity() {
                    ctx.charge(self.directories.data.len() as u64)?;
                    self.directories.ensure(
                        ctx,
                        self.directories.data.capacity().max(4).saturating_mul(2),
                    )?;
                }
                let directory = self.parent().open_dir_nofollow(name);
                ctx.checkpoint()?;
                self.directories
                    .data
                    .push(directory.map_err(|e| io_error("opening module directory", e))?);
            } else {
                self.leaf = true;
                if self.matches && metadata.is_file() {
                    let file = platform::open(self.parent(), name);
                    ctx.checkpoint()?;
                    self.file = Some(file.map_err(|e| io_error("opening module source", e))?);
                }
            }
        }
        if self.leaf && directory_required(path) {
            return Err(not_directory());
        }
        Ok(true)
    }
}

fn directory_required(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    let separator = |byte: u8| byte == b'/' || cfg!(windows) && byte == b'\\';
    bytes.last().is_some_and(|&last| separator(last))
        || bytes.ends_with(b".") && bytes.len() > 1 && separator(bytes[bytes.len() - 2])
}

fn not_directory() -> Error {
    Error::new(
        ErrorKind::Runtime,
        "require: module path component is not a directory",
    )
}

pub(in crate::loading) fn escape() -> Error {
    Error::new(
        ErrorKind::Runtime,
        "require: module name escapes module root",
    )
}
