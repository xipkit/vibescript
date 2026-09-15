use super::{Name, clean, files, policy::Policy, trimmed};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Origin {
    pub root: files::Root,
    pub relative: Arc<[u8]>,
}

pub(super) struct Resolver {
    roots: Vec<files::Root>,
    policy: Policy,
    limit: usize,
}

pub(super) struct Candidates<'a> {
    roots: std::slice::Iter<'a, files::Root>,
    relative_root: Option<files::Root>,
    name: Value,
    explicit: bool,
}

pub(super) struct Candidate {
    pub root: files::Root,
    pub relative: Value,
    pub path: PathBuf,
    explicit: bool,
    _path: Option<Charge>,
}

impl Resolver {
    pub fn new(paths: &[PathBuf], allow: &[String], deny: &[String], limit: usize) -> Result<Self> {
        let mut roots = Vec::with_capacity(paths.len());
        for path in paths {
            if trimmed(path.as_os_str().as_encoded_bytes()).is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "module path cannot be empty",
                ));
            }
            let root = files::Root::new(path)
                .map_err(|e| configuration_error("opening module path", e))?;
            roots.push(root);
        }
        let policy = Policy::new(allow, deny)?;
        Ok(Self {
            roots,
            policy,
            limit,
        })
    }

    pub fn candidates<'a>(
        &'a self,
        ctx: &mut CallContext,
        input: &[u8],
        caller: Option<&Origin>,
    ) -> Result<Candidates<'a>> {
        ctx.checkpoint()?;
        let request = Name::parse(ctx, input)?;
        let mut name = request.normalized;
        let relative_root = if request.relative {
            let Some(caller) = caller else {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "require: relative module requires a module caller",
                ));
            };
            name = relative_name(ctx, &caller.relative, name.as_bytes().unwrap())?;
            Some(caller.root.clone())
        } else {
            if self.roots.is_empty() {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "require: module paths not configured",
                ));
            }
            None
        };
        self.policy.check(ctx, name.as_bytes().unwrap())?;
        let roots = if request.relative {
            &self.roots[..0]
        } else {
            &self.roots
        };
        Ok(Candidates {
            roots: roots.iter(),
            relative_root,
            name,
            explicit: request.relative,
        })
    }

    pub fn read(
        &self,
        ctx: &mut CallContext,
        candidate: &Candidate,
    ) -> Result<Option<files::Source>> {
        let relative = candidate.path.strip_prefix(candidate.root.path()).unwrap();
        let file = match candidate.root.open(ctx, relative) {
            Ok(files::Opened::File(file)) => file,
            Ok(files::Opened::Missing) => return Ok(None),
            Ok(files::Opened::BrokenLink) if candidate.explicit => return Ok(None),
            Ok(files::Opened::BrokenLink) => return Err(files::escape()),
            Err(error) if error.kind == ErrorKind::Name => return Ok(None),
            Err(error) => return Err(error),
        };
        match files::read(ctx, file, self.limit) {
            Ok(source) => Ok(Some(source)),
            Err(error) if error.kind == ErrorKind::Name => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn valid(
        &self,
        ctx: &mut CallContext,
        candidate: &Candidate,
        stamp: files::Stamp,
    ) -> Result<bool> {
        let result = (|| {
            let relative = candidate.path.strip_prefix(candidate.root.path()).unwrap();
            let files::Opened::File(file) = candidate.root.open(ctx, relative)? else {
                return Ok(false);
            };
            Ok(files::stamp(ctx, &file)? == stamp)
        })();
        match result {
            Err(_) if !ctx.exhausted() => Ok(false),
            other => other,
        }
    }
}

impl Candidates<'_> {
    pub fn next(&mut self, ctx: &mut CallContext) -> Result<Option<Candidate>> {
        ctx.checkpoint()?;
        ctx.charge(1)?;
        let root = if self.explicit {
            self.relative_root.take()
        } else {
            self.roots.next().cloned()
        };
        let Some(root) = root else { return Ok(None) };
        let bytes = self.name.as_bytes().unwrap();
        let Some(capacity) = root
            .path()
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .checked_add(bytes.len().saturating_mul(if cfg!(unix) { 1 } else { 3 }))
            .and_then(|n| n.checked_add(1))
        else {
            return ctx.fail(ErrorKind::Memory, "module path size overflow");
        };
        ctx.charge(capacity as u64)?;
        let path_charge = ctx.reserve(capacity)?;
        let _conversion = ctx.reserve(if cfg!(unix) {
            0
        } else {
            bytes.len().saturating_mul(3)
        })?;
        let relative = native_path(bytes);
        let mut path = PathBuf::with_capacity(capacity);
        path.push(root.path());
        path.push(relative.as_ref());
        if path.capacity() > capacity {
            return ctx.fail(ErrorKind::Memory, "module path exceeded reserved storage");
        }
        if !path.starts_with(root.path()) {
            return Err(files::escape());
        }
        Ok(Some(Candidate {
            root,
            relative: self.name.clone(),
            path,
            explicit: self.explicit,
            _path: path_charge,
        }))
    }
}

impl Candidate {
    // Cached code owns immutable origin metadata independently of a call's values.
    pub fn origin(&self) -> Origin {
        Origin {
            root: self.root.clone(),
            relative: Arc::from(self.relative.as_bytes().unwrap()),
        }
    }
}

fn relative_name(ctx: &mut CallContext, caller: &[u8], request: &[u8]) -> Result<Value> {
    let directory = caller
        .iter()
        .rposition(|&b| b == b'/')
        .map_or(&caller[..0], |at| &caller[..at]);
    let Some(capacity) = directory
        .len()
        .checked_add(request.len())
        .and_then(|n| n.checked_add(1))
    else {
        return ctx.fail(ErrorKind::Memory, "module path size overflow");
    };
    ctx.charge(capacity as u64)?;
    let mut joined = Buffer::with_capacity(ctx, capacity)?;
    if !directory.is_empty() {
        joined.extend(ctx, directory)?;
        joined.push(ctx, b'/')?;
    }
    joined.extend(ctx, request)?;
    let mut normalized = Buffer::with_capacity(ctx, capacity)?;
    clean(&joined.data, &mut normalized.data);
    if normalized.data == b".."
        || normalized.data.starts_with(b"../")
        || normalized.data.first() == Some(&b'/')
    {
        return Err(files::escape());
    }
    normalized.shrink(ctx)?;
    Value::from_bytes(ctx, normalized)
}

#[cfg(unix)]
fn native_path(bytes: &[u8]) -> Cow<'_, Path> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(Path::new(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn native_path(mut bytes: &[u8]) -> Cow<'static, Path> {
    let mut text = String::with_capacity(bytes.len().saturating_mul(3));
    while !bytes.is_empty() {
        let (rune, width, _) = crate::scan::rune(bytes);
        text.push(rune);
        bytes = &bytes[width..];
    }
    Cow::Owned(PathBuf::from(text))
}

fn configuration_error(operation: &str, error: std::io::Error) -> Error {
    Error::new(ErrorKind::Argument, format!("{operation}: {error}"))
}

#[cfg(test)]
mod tests;
