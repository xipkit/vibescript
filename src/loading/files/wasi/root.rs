use super::Dir;
use rustix::{
    fs::{Mode, OFlags},
    io::Errno,
};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io,
    path::{Component, Path, PathBuf},
};

enum Part {
    Root,
    Parent,
    Name(OsString),
}

/// Resolves a configured root to its physical guest path and opens it, as
/// native targets do with `canonicalize` followed by opening the result.
///
/// wasi-libc's `realpath` fails when an ancestor of a preopen is not itself
/// accessible, as with `/scripts/app` under a `/scripts/app` preopen. Until
/// the walk reaches a directory the host exposes, unreachable components are
/// treated as such virtual ancestors, and the final open decides whether the
/// path exists. After that every component must exist, and links are expanded
/// before `..` is applied.
pub(super) fn open(path: &Path) -> io::Result<(Dir, PathBuf)> {
    if path.as_os_str().is_empty() {
        return Err(Errno::NOENT.into());
    }
    let mut pending = VecDeque::new();
    prepend(&mut pending, path);
    if !path.is_absolute() {
        prepend(&mut pending, &std::env::current_dir()?);
    }
    let mut resolved = PathBuf::from("/");
    // Leading components of `resolved` the host could not reach, and the
    // confirmed directories that follow them.
    let (mut unreached, mut reached) = (0_usize, 0_usize);
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::Root => {
                resolved = PathBuf::from("/");
                (unreached, reached) = (0, 0);
                continue;
            }
            Part::Parent if reached > 0 => {
                resolved.pop();
                reached -= 1;
                continue;
            }
            Part::Parent if unreached > 0 => return Err(Errno::NOENT.into()),
            Part::Parent => continue,
            Part::Name(name) => name,
        };
        let candidate = resolved.join(&name);
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(_) if reached == 0 => {
                resolved = candidate;
                unreached += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if metadata.is_symlink() {
            if links == 40 {
                return Err(Errno::LOOP.into());
            }
            links += 1;
            prepend(&mut pending, &std::fs::read_link(&candidate)?);
        } else if metadata.is_dir() {
            resolved = candidate;
            reached += 1;
        } else {
            return Err(Errno::NOTDIR.into());
        }
    }
    let directory = rustix::fs::open(&resolved, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
    Ok((Dir(directory), resolved))
}

fn prepend(pending: &mut VecDeque<Part>, path: &Path) {
    for part in path.components().rev() {
        pending.push_front(match part {
            Component::Prefix(_) | Component::RootDir => Part::Root,
            Component::CurDir => continue,
            Component::ParentDir => Part::Parent,
            Component::Normal(name) => Part::Name(name.to_owned()),
        });
    }
}
