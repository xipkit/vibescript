use super::Dir;
use crate::{CallContext, Result, loading::files::root::escape};
use cap_std::fs::MetadataExt as _;
use std::{
    fs::OpenOptions,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::Path,
};

pub(super) fn absolute_target<'a>(
    ctx: &mut CallContext,
    root: &Dir,
    root_path: &Path,
    target: &'a Path,
) -> Result<&'a Path> {
    if let Ok(relative) = target.strip_prefix(root_path) {
        return Ok(relative);
    }
    let retained = root.dir_metadata();
    ctx.checkpoint()?;
    let retained = retained.map_err(|_| escape())?;
    // Unix mount aliases and macOS casing can differ from the root spelling.
    // Query directory identity without reading content;
    // subsequent component opens still use the retained capability directory.
    for prefix in target.ancestors() {
        ctx.charge(prefix.as_os_str().as_encoded_bytes().len() as u64 + 1)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(prefix);
        ctx.checkpoint()?;
        let Ok(directory) = directory else {
            continue;
        };
        let metadata = directory.metadata();
        ctx.checkpoint()?;
        let metadata = metadata.map_err(|_| escape())?;
        if metadata.dev() == retained.dev() && metadata.ino() == retained.ino() {
            return target.strip_prefix(prefix).map_err(|_| escape());
        }
    }
    Err(escape())
}
