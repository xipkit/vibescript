//! Script reading and module search paths for the Go-style commands.

use crate::compat;
use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// The largest script the commands compile, as the reference engine's default
/// `MaxSourceBytes`.
pub const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// Reads a script through one descriptor, rejecting non-regular files and
/// sources over [`MAX_SOURCE_BYTES`] before loading them.
///
/// Invalid UTF-8 is replaced with U+FFFD, as the reference lexer decodes it.
pub fn read(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| compat::path_error("open", path, &error))?;
    let metadata = file
        .metadata()
        .map_err(|error| compat::path_error("stat", path, &error))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds maximum size ({} > {MAX_SOURCE_BYTES} bytes)",
            metadata.len()
        ));
    }
    let mut data = Vec::new();
    file.by_ref()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|error| compat::path_error("read", path, &error))?;
    if data.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds maximum size (> {MAX_SOURCE_BYTES} bytes)"
        ));
    }
    Ok(match String::from_utf8_lossy(&data) {
        Cow::Borrowed(_) => String::from_utf8(data).expect("validated above"),
        Cow::Owned(text) => text,
    })
}

/// Builds the module search path: `base` first, then each extra directory,
/// all absolute and deduplicated by spelling, as the reference does.
pub fn module_paths(base: &Path, extras: &[OsString]) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut add = |label: &str, path: &Path| -> Result<(), String> {
        let absolute = compat::absolute(path).map_err(|error| {
            format!(
                "resolve {label} {}: {}",
                compat::quote(&compat::bytes(path.as_os_str())),
                compat::reason(&error)
            )
        })?;
        let quoted = compat::quote(&compat::bytes(absolute.as_os_str()));
        let metadata = fs::metadata(&absolute).map_err(|error| {
            format!(
                "access {label} {quoted}: {}",
                compat::path_error("stat", &absolute, &error)
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!("{label} {quoted} is not a directory"));
        }
        if !paths.contains(&absolute) {
            paths.push(absolute);
        }
        Ok(())
    };
    add("script directory", base)?;
    for extra in extras {
        add("module path", Path::new(extra))?;
    }
    Ok(paths)
}
